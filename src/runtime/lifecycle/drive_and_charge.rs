// SPDX-License-Identifier: AGPL-3.0-only

/// A kept drive at close (C1 R49). Its children travel in the delta (R11), so
/// the carrier holds only the row.
struct ClosedDrive {
    drive: ProjectionDrive,
}

struct ClosedCharge {
    charge: ProjectionCharge,
    samples: Vec<ProjectionChargeSample>,
}

fn position_from_sample(
    position_id: i64,
    drive_id: Option<i64>,
    car_id: i64,
    sample: &ParsedSample,
) -> Result<Option<ProjectionPosition>, LifecycleError> {
    let Some((latitude, longitude)) = valid_coordinates(sample.latitude, sample.longitude) else {
        if sample.latitude.is_some() || sample.longitude.is_some() {
            return Err(LifecycleError::InvalidCoordinates);
        }
        return Ok(None);
    };
    Ok(Some(ProjectionPosition {
        id: position_id,
        drive_id,
        car_id,
        date_ms: sample.drive_timestamp_ms,
        latitude,
        longitude,
        speed: sample.speed,
        power: sample.power,
        battery_level: sample.battery_level,
        usable_battery_level: sample.usable_battery_level,
        elevation: sample.elevation,
        odometer: sample.odometer,
        ideal_battery_range_km: sample.ideal_range_km,
        est_battery_range_km: sample.est_range_km,
        rated_battery_range_km: sample.rated_range_km,
        fan_status: sample.fan_status,
        driver_temp_setting: sample.driver_temp_setting,
        passenger_temp_setting: sample.passenger_temp_setting,
        is_climate_on: sample.is_climate_on,
        is_rear_defroster_on: sample.is_rear_defroster_on,
        is_front_defroster_on: sample.is_front_defroster_on,
        inside_temp: sample.inside_temp,
        outside_temp: sample.outside_temp,
        battery_heater: sample.battery_heater,
        battery_heater_on: sample.battery_heater_on,
        battery_heater_no_power: sample.battery_heater_no_power,
        tpms_pressure_fl: sample.tpms_pressure_fl,
        tpms_pressure_fr: sample.tpms_pressure_fr,
        tpms_pressure_rl: sample.tpms_pressure_rl,
        tpms_pressure_rr: sample.tpms_pressure_rr,
    }))
}

/// Shortest movement that counts as a drive or as the car having moved, in
/// kilometres (C1 R47, R53, R61a, R65, R70): 0.1 km.
///
/// Below it, movement is a parking manoeuvre or gear shuffling, not a trip. A
/// full-source odometer reports six decimals of a mile (millimetres), so when
/// it is known and fine it decides alone. A coarse or missing odometer falls
/// back to displacement, and 100 m there equals this threshold, so both
/// notions of "moved" agree; it is also far above the metre-level open-sky
/// GNSS accuracy (GPS SPS Performance Standard). One value serves as the
/// keep threshold, the "unmoved" tolerance of charges and the standalone
/// movement trigger, so the unit has one notion of "moved".
pub(crate) const K_DMIN_KM: f64 = 0.1;

/// Longest time between two standalone positions of a parked car (C1 R53):
/// 15 minutes.
///
/// Parked data changes slowly (drain of the order of 0.1 to 1 percentage point
/// per hour, cabin heat-soak over tens of minutes), so one row per 15 minutes
/// resolves every whole-percent step even at about 1 % per hour, while costing
/// at most 96 rows per online day, small against the Hub's storage budget. The
/// plan's idle target forbids a durable row per empty poll.
pub(crate) const K_PARK_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// [`K_PARK_INTERVAL`] in milliseconds, the unit of every observation time.
const K_PARK_INTERVAL_MS: i64 = K_PARK_INTERVAL.as_millis() as i64;

/// Smallest vertical movement that counts as terrain in the elevation walk
/// (C1 R51): 2 m, the smallest integer dead band above the 1 m quantum.
///
/// Elevations are whole metres (public `smallint`), so readings that flip
/// between neighbouring integers are quantisation, not terrain. A band larger
/// than one quantum removes that inflation while each reversal loses less than
/// 2 m. ST-G3 measures the band on real drives before release.
pub(crate) const K_ELEV_BAND: i64 = 2;

/// Lowest elevation the walk accepts, in metres (C1 R51). Land surface spans
/// about -430 m to 8,849 m; the margin covers ellipsoid-versus-sea-level
/// height. Values outside `K_ELEV_MIN..=K_ELEV_MAX` are faults.
pub(crate) const K_ELEV_MIN: i64 = -1_000;

/// Highest elevation the walk accepts, in metres (C1 R51), see [`K_ELEV_MIN`].
pub(crate) const K_ELEV_MAX: i64 = 9_000;

/// Largest value of the public `smallint` columns `ascent`, `descent` and
/// `duration_min` (C1 R50): 32,767. Larger values saturate at it, which keeps
/// every Hub row representable in the public schema that imported history,
/// packs and clients are shaped after. A saturated value is a true lower
/// bound, more useful than null for an implausible but possible extreme;
/// positions keep raw elevations, so the exact figure stays recomputable.
pub(crate) const K_SMALLINT_MAX: i64 = 32_767;

/// One folded observation of an open drive (C1 R42), built from the fresh
/// families only (R6a): a stale value is older than one already used and must
/// not move a "last" value backwards.
struct DrivePoint {
    /// The drive group's fact time `t_drive`.
    at_ms: i64,
    /// A valid coordinate pair of a fresh position family.
    coordinates: Option<(f64, f64)>,
    speed: Option<i64>,
    power: Option<f64>,
    battery_level: Option<i64>,
    ideal_range_km: Option<f64>,
    rated_range_km: Option<f64>,
    odometer_km: Option<f64>,
    /// The odometer came from a stream or sparse source (R47).
    odometer_coarse: bool,
    outside_temp: Option<f64>,
    inside_temp: Option<f64>,
    charge_energy_added: Option<f64>,
    evidence: DriveEvidence,
}

/// The one ascent and descent function (C1 R51), shared by the live fold and
/// by the terrain recomputation (R52). The state of the walk lives in the open
/// drive (`last_elevation`, `elevation_count`, `elevation_ascent`,
/// `elevation_descent`).
///
/// A drive that was open across the upgrade carries walk state this function
/// did not produce, so it is exempt from "live equals recomputed" until C9
/// fills it, which happens only for drives without car elevations; a drive
/// with car elevations stays exempt (R52, R85).
///
/// Whole-metre readings that flip between neighbours are quantisation, so a
/// reading only counts when it is at least [`K_ELEV_BAND`] from the reference;
/// it then becomes the new reference. No other smoothing is applied, because
/// it would need lookahead or tuning without a Hub measurement. The invariant
/// `ascent - descent == last reference - first reference` makes the walk
/// testable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ElevationWalk {
    pub reference_m: Option<i64>,
    pub count: u32,
    pub ascent_m: i64,
    pub descent_m: i64,
}

impl ElevationWalk {
    /// Step one elevation. `None` and values outside `K_ELEV_MIN..=K_ELEV_MAX`
    /// are ignored; the first valid value sets the reference. Arithmetic is
    /// saturating.
    pub(crate) fn step(&mut self, elevation_m: Option<i64>) {
        let Some(elevation) = elevation_m.filter(|value| (K_ELEV_MIN..=K_ELEV_MAX).contains(value))
        else {
            return;
        };
        self.count = self.count.saturating_add(1);
        let Some(reference) = self.reference_m else {
            self.reference_m = Some(elevation);
            return;
        };
        let change = elevation.saturating_sub(reference);
        if change >= K_ELEV_BAND {
            self.ascent_m = self.ascent_m.saturating_add(change);
            self.reference_m = Some(elevation);
        } else if change.saturating_neg() >= K_ELEV_BAND {
            self.descent_m = self.descent_m.saturating_add(change.saturating_neg());
            self.reference_m = Some(elevation);
        }
    }

    /// `(ascent, descent)`: `(None, None)` with fewer than two valid readings,
    /// because no change can be measured; otherwise each total saturated at
    /// [`K_SMALLINT_MAX`] (R50).
    pub(crate) fn totals(&self) -> (Option<i64>, Option<i64>) {
        if self.count < 2 {
            return (None, None);
        }
        (
            Some(self.ascent_m.min(K_SMALLINT_MAX)),
            Some(self.descent_m.min(K_SMALLINT_MAX)),
        )
    }
}

impl OpenDrive {
    /// A drive with nothing folded yet.
    fn empty(id: i64, car_id: i64, start_date_ms: i64) -> Self {
        Self {
            id,
            car_id,
            start_date_ms,
            start_latitude: None,
            start_longitude: None,
            start_soc: None,
            start_rated_range_km: None,
            speed_max: None,
            outside_temp_sum: 0.0,
            outside_temp_count: 0,
            position_count: 0,
            last_position_date_ms: None,
            last_latitude: None,
            last_longitude: None,
            last_soc: None,
            last_rated_range_km: None,
            last_odometer: None,
            first_odometer: None,
            power_max: None,
            power_min: None,
            inside_temp_sum: 0.0,
            inside_temp_count: 0,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            elevation_ascent: 0,
            elevation_descent: 0,
            last_elevation: None,
            saw_offline: false,
            last_charge_energy_added: None,
            last_ideal_range_km: None,
            last_folded_at_ms: None,
            last_motion_at_ms: None,
            max_displacement_m: 0.0,
            elevation_count: 0,
            odometer_coarse: false,
            still_checkpoint: None,
            positions: Vec::new(),
        }
    }

    /// The elevation walk as stored in this drive (C1 R51).
    fn elevation_walk(&self) -> ElevationWalk {
        ElevationWalk {
            reference_m: self.last_elevation,
            count: self.effective_elevation_count(),
            ascent_m: self.elevation_ascent,
            descent_m: self.elevation_descent,
        }
    }
}

/// Fold one point into an open drive (C1 R42). One fold serves live and seeded
/// drives. `emitted` is the position row this fold stores, if any.
///
/// Aggregates do not depend on location access (`vehicle_data` omits location
/// without `location_data` on firmware 2023.38 and later), so speed, odometer
/// and energy count even without a position. The elevation walk steps only for
/// stored positions, with their stored elevation, so the live ascent and
/// descent equal [`elevation_totals`] over the stored positions (R52).
fn observe_drive_point(
    open: &mut OpenDrive,
    point: &DrivePoint,
    emitted: Option<&ProjectionPosition>,
) {
    open.last_folded_at_ms = Some(
        open.last_folded_at_ms
            .map_or(point.at_ms, |last| last.max(point.at_ms)),
    );
    if let Some(position) = emitted {
        open.position_count = open.position_count.saturating_add(1);
        open.last_position_date_ms = Some(position.date_ms);
        open.last_latitude = Some(position.latitude);
        open.last_longitude = Some(position.longitude);
        let mut walk = open.elevation_walk();
        walk.step(position.elevation);
        open.last_elevation = walk.reference_m;
        open.elevation_count = walk.count;
        open.elevation_ascent = walk.ascent_m;
        open.elevation_descent = walk.descent_m;
    }
    if let Some((latitude, longitude)) = point.coordinates {
        if open.start_latitude.is_none() || open.start_longitude.is_none() {
            open.start_latitude = Some(latitude);
            open.start_longitude = Some(longitude);
        }
        if let (Some(start_latitude), Some(start_longitude)) =
            (open.start_latitude, open.start_longitude)
            && let (Ok(from), Ok(to)) = (
                crate::location::Wgs84Point::new(start_latitude, start_longitude),
                crate::location::Wgs84Point::new(latitude, longitude),
            )
        {
            open.max_displacement_m = open
                .max_displacement_m
                .max(crate::location::great_circle_distance_m(from, to));
        }
    }
    if let Some(odometer) = point.odometer_km.filter(|value| value.is_finite()) {
        let first = *open.first_odometer.get_or_insert(odometer);
        // An odometer cannot decrease, and mixed-source rounding must not
        // create a negative distance, so "last" is the greatest reading and
        // never below the first.
        open.last_odometer = Some(
            open.last_odometer
                .map_or(odometer, |last| last.max(odometer))
                .max(first),
        );
        open.odometer_coarse |= point.odometer_coarse;
    }
    if let Some(level) = point.battery_level {
        open.start_soc.get_or_insert(level);
        open.last_soc = Some(level);
    }
    if let Some(ideal) = point.ideal_range_km {
        open.start_ideal_range_km.get_or_insert(ideal);
        open.end_ideal_range_km = Some(ideal);
        open.last_ideal_range_km = Some(ideal);
    }
    if let Some(rated) = point.rated_range_km {
        open.start_rated_range_km.get_or_insert(rated);
        open.last_rated_range_km = Some(rated);
    }
    if let Some(speed) = point.speed {
        open.speed_max = Some(open.speed_max.map_or(speed, |max| max.max(speed)));
    }
    if let Some(power) = point.power {
        open.power_max = Some(open.power_max.map_or(power, |max| max.max(power)));
        open.power_min = Some(open.power_min.map_or(power, |min| min.min(power)));
    }
    if let Some(temp) = point.outside_temp {
        open.outside_temp_sum += temp;
        open.outside_temp_count = open.outside_temp_count.saturating_add(1);
    }
    if let Some(temp) = point.inside_temp {
        open.inside_temp_sum += temp;
        open.inside_temp_count = open.inside_temp_count.saturating_add(1);
    }
    if let Some(energy) = point.charge_energy_added {
        open.last_charge_energy_added = Some(energy);
    }
    // Still checkpoint (R44): the first fold with no drive information after
    // the last motion is the moment the car was first seen not moving.
    match point.evidence {
        DriveEvidence::Motion => {
            open.last_motion_at_ms = Some(point.at_ms);
            open.still_checkpoint = None;
        }
        DriveEvidence::NoInfo if open.still_checkpoint.is_none() => {
            let mut copy = open.clone();
            copy.positions = Vec::new();
            copy.still_checkpoint = None;
            open.still_checkpoint = Some(Box::new(copy));
        }
        DriveEvidence::NoInfo | DriveEvidence::Park => {}
    }
}

/// Fold one stored position into the open drive (C1 R42). The retained caller
/// is the imported open-drive seeding: the position was recorded by the source
/// as part of a drive, so it is an emitted position and a Motion fold, and
/// `odometer_coarse` is left alone. Its outside and inside temperatures are
/// folded like a live reading (R42), so the seeded means and the live ones
/// are one mean over every reading.
fn observe_drive_position(open: &mut OpenDrive, position: &ProjectionPosition) {
    let point = DrivePoint {
        at_ms: position.date_ms,
        coordinates: Some((position.latitude, position.longitude)),
        speed: position.speed,
        power: position.power,
        battery_level: position.battery_level,
        ideal_range_km: position.ideal_battery_range_km,
        rated_range_km: position.rated_battery_range_km,
        odometer_km: position.odometer,
        odometer_coarse: false,
        outside_temp: position.outside_temp,
        inside_temp: position.inside_temp,
        charge_energy_added: None,
        evidence: DriveEvidence::Motion,
    };
    observe_drive_point(open, &point, Some(position));
}

/// The longest interval between two samples whose straight line is
/// integrated, in ms (C2 R17, R21).
///
/// Ten minutes is five times the slowest default charging cadence (Fleet
/// polling 120 s; Owner wire 30 s), so up to four missed Fleet polls are
/// bridged and a longer silence is not an ordinary polling interval. It also
/// bounds what one unobserved power change can misplace: at 11 kW an interval
/// holds at most 1.83 kWh, and a step at an unknown time inside it moves at
/// most half the step's energy. At the default configuration it equals C1's
/// `T_gap`, so the intervals left unintegrated are the silences C1 R65 judges.
/// It is a constant, not the configured `T_gap`, so a stored session can be
/// judged from its stored samples alone without the collector configuration.
pub(crate) const K_INT_MAX_MS: i64 = 600_000;

/// Half the counter's 0.01 kWh step, in kWh (C2 R19, R44a): a difference below
/// it is float noise between equal stored values, not energy.
const K_ENERGY_EPS_KWH: f64 = 0.005;

/// Largest battery energy that may precede the first sample and still be
/// counted at ratio 1, in kWh (C2 R19 row 3). The count understates the inlet
/// energy by `e0 * (q - 1)`; with `q <= 1.25`, the upper end of the AC
/// inlet-to-battery ratio band that gated measurement G2 checks, the error is
/// at most 0.01 kWh, one counter step, the resolution of the energy added
/// itself.
const K_PRE_SAMPLE_KWH: f64 = 0.04;

/// Smallest counted battery energy, in kWh, from which the session's own
/// inlet-to-battery ratio is trusted (C2 R19 row 4, R20). The counter's
/// 0.01 kWh step at each end of the measured energy limits the ratio error to
/// about 1 % at this size.
const K_RATIO_MIN_KWH: f64 = 1.0;

/// Lowest plausible AC inlet energy as a share of the energy added (C2 R20).
/// An AC charger cannot put more energy into the battery than it draws, so the
/// ratio is at least 1; the 5 % allowance covers whole-kW quantisation of the
/// reported power and the sampling of power changes.
const K_AC_FLOOR: f64 = 0.95;

/// Milliseconds in one hour, the divisor that turns kW times ms into kWh (C2
/// R17).
const MS_PER_HOUR: f64 = 3_600_000.0;

/// Energy added and energy used of one charge, before they reach a row (C2
/// R19). `None` is unknown and `Some(0.0)` a measured zero (R3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ChargeEnergyTotals {
    pub added_kwh: Option<f64>,
    pub used_kwh: Option<f64>,
}

/// Round half away from zero to two decimals, with a result of `-0.0` written
/// as `0.0` (C2 notation `round2`, R4): the scale of the energy and cost
/// columns, so a stored value does not change on a pack round trip.
fn round2(value: f64) -> f64 {
    let rounded = round_to_precision(value, 2);
    if rounded == 0.0 { 0.0 } else { rounded }
}

/// The sample's counter when usable: known, finite and not negative (C2 R10).
/// Tesla's counter is a non-negative kWh total.
fn usable_energy_counter(sample: &ProjectionChargeSample) -> Option<f64> {
    finite_reading(sample.charge_energy_added_kwh).filter(|counter| *counter >= 0.0)
}

/// Fold one usable counter value into the segment fields (C2 R11, table 8.1).
///
/// A first value with no baseline starts at zero: no earlier row holds any
/// part of the counter. A value below the preset baseline is a new Tesla
/// session (the same proof C1 R61a uses), so the baseline becomes zero. A
/// value below the segment maximum is a restart: the finished segment's
/// energy goes into the carry and the whole new value is new energy.
fn fold_energy_counter(open: &mut OpenCharge, counter: f64) {
    let mut baseline = open.first_energy_added.unwrap_or(0.0);
    match open.max_energy_added {
        None => {
            if counter < baseline {
                baseline = 0.0;
            }
            open.max_energy_added = Some(counter);
        }
        Some(max) if counter >= max => open.max_energy_added = Some(counter),
        Some(max) => {
            open.energy_added_carry_kwh += max - baseline;
            baseline = 0.0;
            open.max_energy_added = Some(counter);
        }
    }
    open.first_energy_added = Some(baseline);
    open.last_energy_added = Some(counter);
}

/// Fold the interval that ends at this sample (C2 R17, R18, table 8.2).
///
/// An interval with no time (`dt <= 0`) changes nothing: it holds no energy,
/// and duplicate timestamps in imported samples must not void a measurement.
/// An interval is powered when both end powers are known and it is at most
/// [`K_INT_MAX_MS`]; the trapezoid then adds `(p0 + p1) / 2 * dt` to the
/// inlet sum. The trapezoid is exact for a linear change (DC taper, ramp-up),
/// has zero expected error for a step at an unknown time inside the interval,
/// is symmetric in time and needs only the previous sample. A powered interval
/// whose counters are both usable and did not fall also feeds the ratio sums;
/// a counter restart interval is left out because its battery energy
/// straddles two sessions.
fn fold_charge_interval(
    open: &mut OpenCharge,
    dt_ms: i64,
    power_kw: Option<f64>,
    counter: Option<f64>,
) {
    if dt_ms <= 0 {
        return;
    }
    let powered = match (open.last_sample_power_kw, power_kw) {
        (Some(before), Some(now)) if dt_ms <= K_INT_MAX_MS => Some((before, now)),
        _ => None,
    };
    let Some((before, now)) = powered else {
        open.fully_powered = false;
        return;
    };
    let used = (before + now) / 2.0 * (dt_ms as f64) / MS_PER_HOUR;
    open.energy_used_kwh = Some(open.energy_used_kwh.unwrap_or(0.0) + used);
    if let (Some(previous), Some(counter)) = (open.prev_energy_added_kwh, counter)
        && counter >= previous
    {
        open.ratio_used_kwh += used;
        open.ratio_added_kwh += counter - previous;
    }
}

/// Fold one stored charge sample into the open charge (C2 R10 to R18, R31 to
/// R36). Pure (R2): the sums run in `f64`, in sample order, with the
/// operations as written, so a restarted fold equals an uninterrupted one.
///
/// Order: the counter (R11), this sample's kind evidence and charger type
/// (R31, R32), the power under the updated kind (R33 to R35, with the
/// session's last phase count as tie-break), then the interval from the
/// previous sample (R17, R18). The first sample (nothing folded yet: no count
/// and no previous time, which also keeps an old blob from being taken for a
/// new session, R25) starts the integral at zero and records the battery
/// energy that arrived before it. This is the one writer of `is_dc` and
/// `fast_charger_type` (seam S2) and of `last_energy_added` (S4), and it
/// writes `last_sample_timestamp_ms` itself (S5) so the seeded fold and the
/// slice fold work without C1's step.
fn observe_charge_sample(open: &mut OpenCharge, sample: &ProjectionChargeSample) {
    let first = open.sample_count == 0 && open.last_sample_timestamp_ms.is_none();
    let counter = usable_energy_counter(sample);
    if let Some(counter) = counter {
        fold_energy_counter(open, counter);
    }
    open.is_dc = session_kind_after(open.is_dc, sample_kind_evidence(sample));
    open.fast_charger_type = session_charger_type_after(open.fast_charger_type.take(), sample);
    let power = sample_grid_power_kw(sample, open.is_dc, open.phase_count);
    if first {
        open.energy_used_kwh = Some(0.0);
        open.fully_powered = true;
        let baseline = open.first_energy_added.unwrap_or(0.0);
        open.pre_sample_kwh = counter.map(|counter| counter - baseline);
    } else if let Some(previous_ms) = open.last_sample_timestamp_ms {
        let dt_ms = sample.timestamp_ms.saturating_sub(previous_ms);
        fold_charge_interval(open, dt_ms, power.power_kw, counter);
    } else {
        // Samples were folded but no time was kept: the span to this sample
        // is unmeasured, and no energy is invented for it (R3).
        open.fully_powered = false;
    }
    open.prev_energy_added_kwh = counter;
    open.last_sample_power_kw = power.power_kw;
    open.last_sample_timestamp_ms = Some(sample.timestamp_ms);
    if power.phase_count.is_some() {
        open.phase_count = power.phase_count;
    }
    open.sample_count = open.sample_count.saturating_add(1);
}

fn valid_coordinates(latitude: Option<f64>, longitude: Option<f64>) -> Option<(f64, f64)> {
    match (latitude, longitude) {
        (Some(latitude), Some(longitude))
            if latitude.is_finite()
                && longitude.is_finite()
                && (-90.0..=90.0).contains(&latitude)
                && (-180.0..=180.0).contains(&longitude) =>
        {
            Some((latitude, longitude))
        }
        _ => None,
    }
}

/// Turn an open drive into its closed row, or `None` when the drive is not
/// kept (C1 R47, R49). The end is the drive's latest fold time, never earlier
/// than the start; the caller substitutes the still checkpoint for a drive
/// closed for lack of evidence (R44). `explicit_end` is the time of a Park or
/// plugged-in statement that closes the drive (R43 items 6 and 7): the end is
/// that time, never earlier than the start, even when a later position was
/// folded first. `always_keep` is set for a drive created by seeding an
/// imported open session (R14, R47).
///
/// Keep rule (R47): `distance_km` known and at least [`K_DMIN_KM`] decides
/// alone while the odometer is fine; an unknown or coarse odometer falls back
/// to the greatest displacement from the start, at least `K_DMIN_KM` in
/// metres. Displacement uses the maximum distance, so a round trip is not
/// mistaken for no movement. There is no duration or sample minimum: a short
/// fast trip is still a trip.
fn finalize_drive(
    open: OpenDrive,
    always_keep: bool,
    explicit_end: Option<i64>,
) -> Result<Option<ClosedDrive>, LifecycleError> {
    let end_date_ms = explicit_end
        .unwrap_or_else(|| open.effective_last_folded_ms())
        .max(open.start_date_ms);
    // R46: odometer-based only; GPS path length is never substituted.
    let distance_km = match (open.first_odometer, open.last_odometer) {
        (Some(first), Some(last)) if first.is_finite() && last.is_finite() => {
            Some((last - first).max(0.0))
        }
        _ => None,
    };
    let kept = always_keep
        || distance_km.is_some_and(|distance| distance >= K_DMIN_KM)
        || ((distance_km.is_none() || open.odometer_coarse)
            && open.max_displacement_m >= K_DMIN_KM * 1000.0);
    if !kept {
        return Ok(None);
    }
    // R49: nearest minute (half away from zero), saturated at the smallint
    // limit (R50).
    let duration_min = ((end_date_ms - open.start_date_ms) as f64 / 60_000.0)
        .round()
        .min(K_SMALLINT_MAX as f64) as i64;
    let mean =
        |sum: f64, count: u32| (count > 0).then(|| round_to_precision(sum / f64::from(count), 1));
    let (ascent, descent) = open.elevation_walk().totals();
    let drive = ProjectionDrive {
        id: open.id,
        car_id: open.car_id,
        optimized_at_ms: None,
        start_date_ms: open.start_date_ms,
        end_date_ms,
        distance_km,
        duration_min: Some(duration_min),
        efficiency: None,
        outside_temp_avg: mean(open.outside_temp_sum, open.outside_temp_count),
        inside_temp_avg: mean(open.inside_temp_sum, open.inside_temp_count),
        speed_max: open.speed_max,
        power_max: open.power_max,
        power_min: open.power_min,
        start_ideal_range_km: open
            .start_ideal_range_km
            .map(|km| round_to_precision(km, 2)),
        end_ideal_range_km: open.end_ideal_range_km.map(|km| round_to_precision(km, 2)),
        start_address: None,
        end_address: None,
        start_geofence: None,
        end_geofence: None,
        start_latitude: open
            .start_latitude
            .map(|value| round_to_precision(value, 6)),
        start_longitude: open
            .start_longitude
            .map(|value| round_to_precision(value, 6)),
        end_latitude: open.last_latitude.map(|value| round_to_precision(value, 6)),
        end_longitude: open
            .last_longitude
            .map(|value| round_to_precision(value, 6)),
        start_soc: open.start_soc,
        end_soc: open.last_soc,
        start_rated_range_km: open
            .start_rated_range_km
            .map(|km| round_to_precision(km, 2)),
        end_rated_range_km: open.last_rated_range_km.map(|km| round_to_precision(km, 2)),
        ascent,
        descent,
    };
    Ok(Some(ClosedDrive { drive }))
}

/// The one ascent and descent function (C1 R51, R52), called at drive close
/// through [`ElevationWalk`] and when terrain elevation arrives later through
/// `recompute_terrain_drive`. The elevations are the drive's stored positions
/// in `(date_ms, id)` order. `(None, None)` with fewer than two valid
/// readings; each total saturates at [`K_SMALLINT_MAX`].
pub(crate) fn elevation_totals(
    elevations: impl IntoIterator<Item = Option<i64>>,
) -> (Option<i64>, Option<i64>) {
    let mut walk = ElevationWalk::default();
    for elevation in elevations {
        walk.step(elevation);
    }
    walk.totals()
}

fn finalize_charge(
    open: OpenCharge,
    end_date_ms: Option<i64>,
) -> Result<ClosedCharge, LifecycleError> {
    let end_date_ms = end_date_ms.unwrap_or(open.start_date_ms);
    if end_date_ms < open.start_date_ms {
        return Err(LifecycleError::InvalidTimeline);
    }
    let duration_min = ((end_date_ms - open.start_date_ms) / 60_000).max(0);
    let outside_temp_avg = if open.outside_temp_count > 0 {
        Some(open.outside_temp_sum / f64::from(open.outside_temp_count))
    } else {
        None
    };
    let totals = charge_energy_totals(&open);
    let charge = ProjectionCharge {
        id: open.id,
        car_id: open.car_id,
        start_date_ms: open.start_date_ms,
        end_date_ms: Some(end_date_ms),
        charge_energy_added: totals.added_kwh,
        charge_energy_used_kwh: totals.used_kwh,
        start_ideal_range_km: open.start_ideal_range_km,
        end_ideal_range_km: open.last_ideal_range_km,
        cost: None,
        fast_charger_type: open.fast_charger_type,
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
        start_latitude: open.start_latitude,
        start_longitude: open.start_longitude,
        start_battery_level: open.start_battery_level,
        end_battery_level: open.last_battery_level,
        duration_min: Some(duration_min),
        address: None,
        location_name: None,
        geofence: None,
        is_dc: open.is_dc,
        charge_rate_km_per_hour: None,
        max_charger_power_kw: open.max_charger_power_kw,
        outside_temp_avg,
        start_rated_range_km: open.start_rated_range_km,
        end_rated_range_km: open.last_rated_range_km,
    };
    Ok(ClosedCharge {
        charge,
        samples: open.samples,
    })
}

/// Fold the display values of one parsed observation into the open charge
/// (C2 R27, seam S3). The only writer of these fields; C1 reads them for the
/// closed row's end values and averages and for its snapshot (C1 R69).
///
/// Battery level and the two ranges keep the last known value; the maximum
/// charger power is the largest known non-negative reading; each known
/// outside temperature joins the sum and the count, whose quotient is the
/// arithmetic mean of the folded values. The parsed values are already at the
/// column scale (C1 R36) and equal what the sample row carries (C1 R6a), so
/// nothing is folded twice. The charge analogue of C1 R42 and R49 for drives.
fn observe_charge_aggregate(open: &mut OpenCharge, sample: &ParsedSample) {
    open.last_battery_level = sample.battery_level.or(open.last_battery_level);
    open.last_ideal_range_km = finite_reading(sample.ideal_range_km).or(open.last_ideal_range_km);
    open.last_rated_range_km = finite_reading(sample.rated_range_km).or(open.last_rated_range_km);
    if let Some(power) = finite_reading(sample.charger_power_kw).filter(|power| *power >= 0.0) {
        open.max_charger_power_kw = Some(
            open.max_charger_power_kw
                .map_or(power, |maximum| maximum.max(power)),
        );
    }
    if let Some(temperature) = finite_reading(sample.outside_temp) {
        open.outside_temp_sum += temperature;
        open.outside_temp_count = open.outside_temp_count.saturating_add(1);
    }
}

/// Energy added and energy used of an open charge (C2 R19, R20), from its
/// accumulators alone, so close needs no sample list.
///
/// `E = k + (m - b)` is the counter's own measurement across every segment
/// (R13). The energy used is chosen by the first matching row:
/// 1. no counter: the integral, only when every interval was measured;
/// 2. nothing reached the battery: zero for a single sample (it spans no
///    observed time), else the integral when gap-free (a gap may hide grid
///    draw the counter does not see, such as battery heating);
/// 3. a gap-free integral with at most [`K_PRE_SAMPLE_KWH`] before the first
///    sample: the integral plus that energy at ratio 1;
/// 4. at least [`K_RATIO_MIN_KWH`] counted: `E * U_r / A_r`, the session's own
///    measured ratio applied to the energy whose power was not observed;
/// 5. otherwise unknown.
///
/// An AC session whose result falls below [`K_AC_FLOOR`] of `E` is a
/// systematic power error and is not published (R20); DC and unknown kinds
/// have no floor, and nothing is capped above, because cold-weather heating
/// can draw far more than the battery stores. Both outputs are rounded once,
/// here (R4).
pub(crate) fn charge_energy_totals(open: &OpenCharge) -> ChargeEnergyTotals {
    let energy = open.max_energy_added.map(|maximum| {
        open.energy_added_carry_kwh + (maximum - open.first_energy_added.unwrap_or(0.0))
    });
    let samples = open.sample_count;
    let integral = open.energy_used_kwh;
    let gap_free = open.fully_powered;
    let used = match energy {
        None => (samples >= 2 && gap_free).then_some(integral).flatten(),
        Some(energy) if energy < K_ENERGY_EPS_KWH => {
            if samples <= 1 {
                Some(0.0)
            } else if gap_free {
                integral
            } else {
                None
            }
        }
        Some(energy) => {
            let before_first = open
                .pre_sample_kwh
                .filter(|before| *before <= K_PRE_SAMPLE_KWH);
            if gap_free && samples >= 2 && before_first.is_some() {
                integral
                    .zip(before_first)
                    .map(|(integral, before)| integral + before)
            } else if open.ratio_added_kwh >= K_RATIO_MIN_KWH {
                Some((energy * open.ratio_used_kwh) / open.ratio_added_kwh)
            } else {
                None
            }
        }
    };
    let below_ac_floor = |used: f64| {
        open.is_dc == Some(false)
            && energy.is_some_and(|energy| energy >= K_RATIO_MIN_KWH && used < K_AC_FLOOR * energy)
    };
    ChargeEnergyTotals {
        added_kwh: energy.map(|energy| round2(energy.max(0.0))),
        used_kwh: used
            .filter(|used| used.is_finite() && !below_ac_floor(*used))
            .map(round2),
    }
}

/// The totals of a stored slice of samples (C2 R26): the slice, in sample
/// order (`timestamp_ms`, `id`), folded through [`observe_charge_sample`] on a
/// scratch charge whose baseline is `baseline_kwh` and whose other energy
/// fields are default, then [`charge_energy_totals`]. It equals the incremental
/// result for the same samples and baseline. One definition for tests and for
/// any later recomputation from stored samples.
#[allow(dead_code)] // C2 R26: read by tests and by a recomputation from stored samples; no live caller yet.
pub(crate) fn charge_energy_from_samples(
    samples: &[ProjectionChargeSample],
    baseline_kwh: Option<f64>,
) -> ChargeEnergyTotals {
    let mut ordered = samples.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|sample| (sample.timestamp_ms, sample.id));
    let mut scratch = OpenCharge::empty(0, 0, 0);
    scratch.first_energy_added = baseline_kwh;
    for sample in ordered {
        observe_charge_sample(&mut scratch, sample);
    }
    charge_energy_totals(&scratch)
}

/// Highest battery level, in per cent, whose samples enter the efficiency
/// figure (C2 R62 step 1). Up to about 90 % a charge is in its
/// constant-current part, where the battery energy goes into range; above it
/// the constant-voltage phase spends energy on cell balancing without range,
/// and packs that charge to full re-anchor their state-of-charge estimate,
/// which moves the reported range without energy. A session ending at 100 %
/// still contributes its lower part.
const K_EFF_MAX_LEVEL: i64 = 90;

/// Smallest battery-level span, in points, between the first and last window
/// sample for a session to give an efficiency figure (C2 R62 step 4). A
/// one-point step of the state-of-charge estimate at one end of a 10-point
/// span moves the figure by about a tenth, and steps at both ends in opposite
/// directions by up to about two tenths; the car median (R64) rejects such
/// sessions, and a longer minimum would exclude ordinary top-ups of 10 to 20
/// points. Points, not kWh, so the bound is the same on every pack size. The
/// gated measurement G5 tests it.
const K_EFF_MIN_SPAN: i64 = 10;

/// Fewest session figures from which the car figure is published (C2 R65). A
/// median of three tolerates one bad session; fewer cannot, so below this the
/// stored car efficiency is left as it is.
pub(crate) const K_EFF_MIN_FIGURES: usize = 3;

/// One charging session's efficiency, in Wh per km of rated range, or `None`
/// (C2 R62). `samples` are the session's stored samples; they are read in
/// sample order (`timestamp_ms`, then `id`), so the caller's order does not
/// matter.
///
/// 1. Window samples: battery level, rated range and a usable counter known,
///    and level at most [`K_EFF_MAX_LEVEL`].
/// 2. The first and last window samples are `A` and `B`; fewer than two give
///    `None`.
/// 3. `None` if any sample from `A` to `B` inclusive reports the battery
///    heater on (a heating battery spends part of the measured energy on
///    heat), or the counter decreases between consecutive window samples (a
///    decrease means two sessions).
/// 4. `None` unless the level rises by at least [`K_EFF_MIN_SPAN`] and both the
///    counter and the rated range rise from `A` to `B`.
/// 5. `1000 * dE / dR`, when finite and positive.
///
/// Both ends are state readings, so the gaps between them do not matter and
/// nothing is integrated. Rated range is the fixed basis (R61). The pack
/// chemistry changes no rule (R62a).
pub(crate) fn session_efficiency_wh_per_km(samples: &[ProjectionChargeSample]) -> Option<f64> {
    let mut ordered = samples.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|sample| (sample.timestamp_ms, sample.id));
    let in_window = |sample: &ProjectionChargeSample| {
        sample
            .battery_level
            .is_some_and(|level| level <= K_EFF_MAX_LEVEL)
            && finite_reading(sample.rated_range_km).is_some()
            && usable_energy_counter(sample).is_some()
    };
    let first = ordered.iter().position(|sample| in_window(sample))?;
    let last = ordered.iter().rposition(|sample| in_window(sample))?;
    if first == last {
        return None;
    }
    let span = &ordered[first..=last];
    if span
        .iter()
        .any(|sample| sample.battery_heater_on == Some(true) || sample.battery_heater == Some(true))
    {
        return None;
    }
    let counters = span
        .iter()
        .filter(|sample| in_window(sample))
        .filter_map(|sample| usable_energy_counter(sample))
        .collect::<Vec<_>>();
    if counters.windows(2).any(|pair| pair[1] < pair[0]) {
        return None;
    }
    let (a, b) = (ordered[first], ordered[last]);
    let level_span = b.battery_level? - a.battery_level?;
    let energy_added = usable_energy_counter(b)? - usable_energy_counter(a)?;
    let range_gained = finite_reading(b.rated_range_km)? - finite_reading(a.rated_range_km)?;
    if level_span < K_EFF_MIN_SPAN || energy_added <= 0.0 || range_gained <= 0.0 {
        return None;
    }
    let figure = (1000.0 * energy_added) / range_gained;
    (figure.is_finite() && figure > 0.0).then_some(figure)
}

/// The car's efficiency from its session figures, in Wh per km (C2 R64, R65,
/// R66): the median (the mean of the two middle values for an even count),
/// rounded to 0.1 Wh/km, or `None` with fewer than [`K_EFF_MIN_FIGURES`]
/// figures. The remaining errors of a session (range estimation steps,
/// temperature, stray heating) have long tails; the median rejects them
/// without a tuning parameter, where a trimmed mean needs a trim fraction the
/// Hub has no measurement for. 0.01 kWh over a span of tens of km resolves
/// about 0.1 Wh/km, so finer digits are noise that would change the stored car
/// on every recomputation.
pub(crate) fn car_efficiency_from_figures(figures: &[f64]) -> Option<f64> {
    if figures.len() < K_EFF_MIN_FIGURES {
        return None;
    }
    let mut sorted = figures.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    let median = if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    };
    Some(round_to_precision(median, 1))
}

/// Lowest per-phase input voltage the electrical rule accepts, in volts (C2
/// R33, R30). The lowest nominal supply is 100 V; 10 % supply tolerance puts
/// the floor at 90 V. Below it `V` is not a per-phase AC input (DC sessions
/// report a few volts).
const K_V_MIN: f64 = 90.0;

/// Highest per-phase input voltage the electrical rule accepts, in volts (C2
/// R33). The highest phase-to-neutral nominal supply is 277 V; 10 % tolerance
/// gives 305 V. A larger value is a phase-to-phase figure (400 V, 480 V) and
/// must not be multiplied as if it were per phase.
const K_V_MAX: f64 = 305.0;

/// Highest AC inlet current the electrical rule accepts, in amperes (C2 R33).
/// SAE AC Level 2 allows 80 A on one phase; IEC 62196-2 three-phase inlets stop
/// at 63 A.
const K_I_MAX: f64 = 80.0;

/// Largest power any standard AC inlet can carry, in kW (C2 R31). Three phases
/// at 63 A (IEC 62196-2) and the highest accepted per-phase voltage
/// `K_V_MAX`: 3 x 63 x 305 = 57 645 W; one phase at 80 A is only 24.4 kW. The
/// same tolerance-inclusive bounds as R33, so any AC input R33 accepts stays at
/// or below it. A reported power above it is DC evidence.
const K_AC_MAX_KW: f64 = 57.645;

/// Absolute tolerance when comparing `n * V * I` with the reported power, in kW
/// (C2 R34). The reported power is whole kW, rounded or truncated by the
/// source and rounded again at storage (C1 R36), so the true power lies within
/// 1 kW of it.
const K_PHASE_ABS_KW: f64 = 1.0;

/// Relative tolerance when comparing `n * V * I` with the reported power (C2
/// R34). Integer rounding of the current (at most 0.5 A in 10 A or more, 5 %),
/// integer rounding of the voltage (under 0.3 %) and a power factor down to
/// 0.95 (SAE J2894/1 summary) add up to at most about 5 %; 10 % covers the sum
/// with margin. Gated measurement G1 tests the sum on Hub data.
const K_PHASE_REL: f64 = 0.10;

/// Lowest reported power, in kW, at which a plain AC reading counts as AC
/// evidence (C2 R31). Power is whole kW, so a reading of 0 says nothing about
/// the charge point and a reading of 1 or more is a flow.
const K_AC_EVIDENCE_MIN_KW: f64 = 1.0;

/// Per-sample power and the phase count that produced it (C2 R39).
///
/// `phase_count` is `Some(n)` only when the sample itself fixed the count
/// (R35 rows 1, 2 and 5); a count taken from the session state (rows 3 and 6)
/// is not reported, so it cannot reinforce itself (R36).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SamplePower {
    pub power_kw: Option<f64>,
    pub phase_count: Option<i64>,
}

/// A finite value (C2 notation "known").
fn finite_reading(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite())
}

/// The reported power when usable: known and not negative (C2 R33 step 1). A
/// charging session draws power and never exports it.
fn usable_charger_power_kw(sample: &ProjectionChargeSample) -> Option<f64> {
    finite_reading(sample.charger_power_kw).filter(|power| *power >= 0.0)
}

/// Voltage and current when both describe a per-phase AC input: `V` within
/// `K_V_MIN..=K_V_MAX` and `I` above zero and at most `K_I_MAX` (C2 R33 step
/// 3).
fn per_phase_ac_inputs(sample: &ProjectionChargeSample) -> Option<(f64, f64)> {
    let voltage = finite_reading(sample.charger_voltage)?;
    let current = finite_reading(sample.charger_actual_current)?;
    let voltage_ok = (K_V_MIN..=K_V_MAX).contains(&voltage);
    let current_ok = current > 0.0 && current <= K_I_MAX;
    (voltage_ok && current_ok).then_some((voltage, current))
}

/// Power of `n` energised phases in kW, `n * V * I / 1000` (C2 R30), in the
/// operation order R2 fixes.
fn phase_power_kw(phases: i64, voltage: f64, current: f64) -> f64 {
    phases as f64 * voltage * current / 1000.0
}

/// The phase counts 1 to 3 whose power agrees with the reported power (C2
/// R34), in ascending order.
struct ConsistentPhases {
    counts: [i64; 3],
    len: usize,
}

impl ConsistentPhases {
    fn new(voltage: f64, current: f64, reported_kw: f64) -> Self {
        let mut set = ConsistentPhases {
            counts: [0; 3],
            len: 0,
        };
        for phases in 1..=3_i64 {
            let predicted = phase_power_kw(phases, voltage, current);
            if (predicted - reported_kw).abs() <= K_PHASE_ABS_KW + K_PHASE_REL * predicted {
                set.counts[set.len] = phases;
                set.len += 1;
            }
        }
        set
    }

    fn contains(&self, phases: i64) -> bool {
        self.counts[..self.len].contains(&phases)
    }

    /// The only member, when there is exactly one.
    fn only(&self) -> Option<i64> {
        (self.len == 1).then_some(self.counts[0])
    }
}

/// What one sample says about the kind of the session (C2 R31, R39):
/// `Some(true)` DC, `Some(false)` AC, `None` nothing.
///
/// DC evidence is a Supercharger or other DC type; or the charger-present flag
/// true with a type that is not Tesla AC; or a power above `K_AC_MAX_KW`. AC
/// evidence counts only when there is no DC evidence: a Tesla AC type; or the
/// flag false with a reading of at least 1 kW; or exactly one consistent phase
/// count (R34) with a reading of at least 1 kW. DC wins because its evidence
/// is specific while an AC flag can lag at the start of a session.
pub(crate) fn sample_kind_evidence(sample: &ProjectionChargeSample) -> Option<bool> {
    let kind = fast_charger_kind(sample.fast_charger_type.as_deref());
    let power = finite_reading(sample.charger_power_kw);
    let dc = matches!(
        kind,
        FastChargerKind::Supercharger | FastChargerKind::OtherDc
    ) || (sample.fast_charger_present == Some(true) && kind != FastChargerKind::TeslaAc)
        || power.is_some_and(|power| power > K_AC_MAX_KW);
    if dc {
        return Some(true);
    }
    let flow = power.filter(|power| *power >= K_AC_EVIDENCE_MIN_KW);
    let ac = kind == FastChargerKind::TeslaAc
        || (sample.fast_charger_present == Some(false) && flow.is_some())
        || flow
            .zip(per_phase_ac_inputs(sample))
            .is_some_and(|(reported, (voltage, current))| {
                ConsistentPhases::new(voltage, current, reported)
                    .only()
                    .is_some()
            });
    ac.then_some(false)
}

/// The session kind after one sample's evidence (C2 R31 table). `Some(true)`
/// is final: a session cannot change between AC and DC without an unplug,
/// which closes it (C1 R63), and DC evidence is specific. AC evidence only
/// fills an unset kind; no evidence changes nothing.
fn session_kind_after(is_dc: Option<bool>, evidence: Option<bool>) -> Option<bool> {
    match (is_dc, evidence) {
        (Some(true), _) => Some(true),
        (_, Some(true)) => Some(true),
        (None, Some(false)) => Some(false),
        (current, _) => current,
    }
}

/// The session charger type after one sample (C2 R32). The first sample whose
/// type is not "no information" sets it, as its trimmed text; later values do
/// not change it, because one session has one charge point and the type can
/// read `<invalid>` until the car has identified the charger.
fn session_charger_type_after(
    current: Option<String>,
    sample: &ProjectionChargeSample,
) -> Option<String> {
    if current.is_some() {
        return current;
    }
    let text = sample.fast_charger_type.as_deref()?;
    match fast_charger_kind(Some(text)) {
        FastChargerKind::NoInformation => None,
        _ => Some(text.trim().to_owned()),
    }
}

/// Power of one sample in kW and the phase count behind it (C2 R33 to R35,
/// R39). A session count outside 1 to 3 (an undecodable or hand-edited state)
/// is ignored, as R30 allows only 1 to 3 energised phases. `session_is_dc` is the session kind after this sample's own evidence
/// (R31); `session_phase_count` is the session's last determined count (R36).
///
/// DC reads the reported power. AC with a per-phase voltage and a current uses
/// `n * V * I` because it resolves power finely (a 120 V, 12 A charge is
/// 1.44 kW where the reported power reads 1); `n` is the phase count whose
/// power agrees with the reported power (R34), chosen by the R35 table: the
/// measurement outranks the label because the label states availability, and
/// the label or the session count only breaks a tie they are consistent with.
/// Anything else reads the reported power, or nothing (R37). Pure (R2); the
/// sample is never changed and no phase value reaches storage (R38).
pub(crate) fn sample_grid_power_kw(
    sample: &ProjectionChargeSample,
    session_is_dc: Option<bool>,
    session_phase_count: Option<i64>,
) -> SamplePower {
    let reported = usable_charger_power_kw(sample);
    let reported_only = SamplePower {
        power_kw: reported,
        phase_count: None,
    };
    if session_is_dc == Some(true) {
        return reported_only;
    }
    let Some((voltage, current)) = per_phase_ac_inputs(sample) else {
        return reported_only;
    };
    let label = sample
        .charger_phases
        .filter(|phases| (1..=3).contains(phases));
    let session = session_phase_count.filter(|phases| (1..=3).contains(phases));
    // (count, whether the sample itself fixed it)
    let chosen = match reported {
        Some(reported) => {
            let consistent = ConsistentPhases::new(voltage, current, reported);
            if let Some(only) = consistent.only() {
                Some((only, true))
            } else if let Some(label) = label.filter(|label| consistent.contains(*label)) {
                Some((label, true))
            } else {
                session
                    .filter(|session| consistent.contains(*session))
                    .map(|session| (session, false))
            }
        }
        None => label
            .map(|label| (label, true))
            .or_else(|| session.map(|session| (session, false))),
    };
    match chosen {
        Some((phases, fixed_by_sample)) => SamplePower {
            power_kw: Some(phase_power_kw(phases, voltage, current)),
            phase_count: fixed_by_sample.then_some(phases),
        },
        None => reported_only,
    }
}

/// Canonical names of Tesla's shift and charging enums (C1 R40a). Shift:
/// `P`, `R`, `N`, `D` plus the no-value names `Unknown`, `Invalid`, `SNA`.
/// Charging: `Charging`, `Starting`, `Stopped`, `NoPower`, `Complete`,
/// `Disconnected` plus `Unknown`. Source: Tesla's `vehicle_data.proto` and the
/// public Owner API.
const CANONICAL_ENUM_NAMES: [&str; 13] = [
    "P",
    "R",
    "N",
    "D",
    "Charging",
    "Starting",
    "Stopped",
    "NoPower",
    "Complete",
    "Disconnected",
    "Unknown",
    "Invalid",
    "SNA",
];

/// Type prefixes that Fleet Telemetry enum value names carry (C1 R40a), longest
/// first so `DetailedChargeState` is never mistaken for a shorter prefix.
const ENUM_TYPE_PREFIXES: [&str; 3] = ["DetailedChargeState", "ChargeState", "ShiftState"];

/// Normalise a shift or charging enum text to its canonical name (C1 R40a):
/// trim, strip at most one type prefix, then compare ASCII case-insensitively.
/// The Owner API sends `D` and `Charging`, Fleet Telemetry sends `ShiftStateD`
/// and `DetailedChargeStateCharging`; one rule for both enums means no source
/// can match one enum and miss the other.
pub(crate) fn normalise_enum_text(text: &str) -> Option<&'static str> {
    let mut rest = text.trim();
    for prefix in ENUM_TYPE_PREFIXES {
        if rest
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        {
            rest = &rest[prefix.len()..];
            break;
        }
    }
    CANONICAL_ENUM_NAMES
        .iter()
        .copied()
        .find(|name| name.eq_ignore_ascii_case(rest))
}

/// Whether a shift state is a drive gear, D, R or N (C1 R23, R40). Production
/// reads the gear through [`classify_drive`]; this is R23's helper, kept as the
/// test oracle of the gear rule.
#[cfg(test)]
fn is_drive_shift(shift_state: Option<&str>) -> bool {
    matches!(
        shift_state.and_then(normalise_enum_text),
        Some("D" | "R" | "N")
    )
}

/// Gear named by a shift state text: only P, R, N and D are gears (C1 R40).
fn gear_of(shift_state: Option<&str>) -> Option<&'static str> {
    shift_state
        .and_then(normalise_enum_text)
        .filter(|name| matches!(*name, "P" | "R" | "N" | "D"))
}

/// What one observation says about driving (C1 R40).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriveEvidence {
    /// Gear D, R or N, or no gear and a measured speed above zero.
    Motion,
    /// Gear P, or (full source only) a present-but-null gear with no speed.
    Park,
    /// No drive information, including a stale motion family.
    NoInfo,
}

/// Classify drive evidence (C1 R40). `motion_fresh` is the freshness of the
/// motion family after the imported gate; a stale family is NoInfo.
///
/// Reasons: Tesla's gear enum is P/R/N/D plus Invalid/SNA/Unknown meaning "no
/// value"; a car moving at a measured speed is not parked; Tesla's parked
/// `vehicle_data` example has `shift_state: null, speed: null`; a stream or
/// sparse source omits keys, so an omitted or null gear there is no statement
/// (R38, R76); neutral counts as a drive gear because the car can roll.
fn classify_drive(parsed: &ParsedSample, motion_fresh: bool) -> DriveEvidence {
    if !motion_fresh {
        return DriveEvidence::NoInfo;
    }
    let speed = parsed.speed;
    match gear_of(parsed.shift_state.as_deref()) {
        Some("P") => DriveEvidence::Park,
        Some(_) => DriveEvidence::Motion,
        None if speed.is_some_and(|value| value > 0) => DriveEvidence::Motion,
        None if parsed.kind == SourceKind::Full
            && parsed.shift_state_key_present
            && speed.is_none_or(|value| value == 0) =>
        {
            DriveEvidence::Park
        }
        None => DriveEvidence::NoInfo,
    }
}

/// What one observation says about charging (C1 R60). Classes: Active
/// (`Active`), Hold (`Hold`), End (`Complete`, `Disconnected`), None.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChargeEvidence {
    /// `Charging`: the only state in which energy is flowing.
    Active,
    /// `Starting`, `Stopped`, `NoPower`: a negotiation or a pause with the
    /// cable connected.
    Hold,
    /// `Complete`: Tesla's statement that the charge limit was reached, cable
    /// still connected. End class, plugged.
    Complete,
    /// `Disconnected`: the cable is out. End class, not plugged.
    Disconnected,
    /// Absent, null, `Unknown`, anything else, or a plugged class discarded by
    /// R22.
    None,
}

impl ChargeEvidence {
    /// Plugged evidence: Active, Hold or `Complete` (C1 R22, R60).
    fn is_plugged(self) -> bool {
        matches!(self, Self::Active | Self::Hold | Self::Complete)
    }

    /// End class: `Complete` or `Disconnected` (C1 R60, R63).
    fn is_end(self) -> bool {
        matches!(self, Self::Complete | Self::Disconnected)
    }
}

/// Classify charge evidence (C1 R60) from a fresh charge_session family.
fn classify_charge(parsed: &ParsedSample, charge_session_fresh: bool) -> ChargeEvidence {
    if !charge_session_fresh {
        return ChargeEvidence::None;
    }
    match parsed
        .charging_state
        .as_deref()
        .and_then(normalise_enum_text)
    {
        Some("Charging") => ChargeEvidence::Active,
        Some("Starting" | "Stopped" | "NoPower") => ChargeEvidence::Hold,
        Some("Complete") => ChargeEvidence::Complete,
        Some("Disconnected") => ChargeEvidence::Disconnected,
        _ => ChargeEvidence::None,
    }
}

// The hooks below are the drive and charge steps of the observation skeleton
// (C1 R8). Parts D and C fill them in; until then each one compiles and emits
// nothing, so the phase, version, service-mode and state-interval steps of
// part P run on their own.

/// Whether this observation carries a fresh motion or position family: the
/// drive facts that fold into an open drive and judge its continuity (C1 R42,
/// R43).
fn has_fresh_drive_facts(observation: &Observation<'_>) -> bool {
    observation.fresh.motion || observation.fresh.position
}

/// Allocate the next position id (C1 R10).
fn next_position_id(state: &mut OpenSessionState) -> Result<i64, LifecycleError> {
    let id = state.next_position_id;
    state.next_position_id = id
        .checked_add(1)
        .ok_or(LifecycleError::IdentifierExhausted)?;
    Ok(id)
}

/// The point this observation folds into an open drive (C1 R42), from the
/// fresh families only (R6a).
fn drive_point(observation: &Observation<'_>) -> DrivePoint {
    let parsed = observation.parsed;
    let fresh = observation.fresh;
    DrivePoint {
        at_ms: parsed.drive_timestamp_ms,
        coordinates: if fresh.position {
            parsed.latitude.zip(parsed.longitude)
        } else {
            None
        },
        speed: if fresh.motion { parsed.speed } else { None },
        power: if fresh.motion { parsed.power } else { None },
        battery_level: if fresh.battery {
            parsed.battery_level
        } else {
            None
        },
        ideal_range_km: if fresh.battery {
            parsed.ideal_range_km
        } else {
            None
        },
        rated_range_km: if fresh.battery {
            parsed.rated_range_km
        } else {
            None
        },
        odometer_km: if fresh.odometer {
            parsed.odometer
        } else {
            None
        },
        odometer_coarse: parsed.kind != SourceKind::Full,
        outside_temp: if fresh.climate {
            parsed.outside_temp
        } else {
            None
        },
        inside_temp: if fresh.climate {
            parsed.inside_temp
        } else {
            None
        },
        charge_energy_added: if fresh.charge_session {
            parsed.charge_energy_added
        } else {
            None
        },
        evidence: observation.drive,
    }
}

/// Fold this observation into the open drive once (C1 R42) and return the
/// position row it stores, if any. The caller routes the row: it belongs to
/// the open drive unless the drive is discarded in the same step (R48).
///
/// A position is stored when the position family is fresh, its coordinates are
/// valid and its time is later than the drive's last stored position, so every
/// fresh drive position is kept and none is downsampled.
fn fold_into_open_drive(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
) -> Result<Option<ProjectionPosition>, LifecycleError> {
    let parsed = observation.parsed;
    let Some(open) = state.open_drive.as_ref() else {
        return Ok(None);
    };
    let stores_position = observation.fresh.position
        && parsed.latitude.is_some()
        && parsed.longitude.is_some()
        && open
            .last_position_date_ms
            .is_none_or(|last| parsed.drive_timestamp_ms > last);
    let drive_id = open.id;
    let position = if stores_position {
        let id = next_position_id(state)?;
        position_from_sample(id, Some(drive_id), observation.car_id, parsed)?
    } else {
        None
    };
    let point = drive_point(observation);
    if let Some(open) = state.open_drive.as_mut() {
        observe_drive_point(open, &point, position.as_ref());
    }
    Ok(position)
}

/// Close the open drive (C1 R43, R44, R47 to R49, R53, R69).
///
/// `loe_end` closes for lack of evidence: the end is the still checkpoint when
/// one exists (the first fold with no drive information after the last
/// motion), else the last fold, so the stationary tail is not counted in the
/// duration, distance or averages. Otherwise the observation was folded just
/// before and the end is that fold. A kept drive becomes a row and its closing
/// position stays with it; a discarded drive's id goes to
/// `discarded_drive_ids` and its closing position becomes a standalone
/// position (R48); storage places earlier children by the same rule.
///
/// `explicit_end` is the Park or plugged-in fact's own time when one closes the
/// drive (R43 items 6 and 7, R44): a position folded earlier but dated later
/// stays a child dated after the end (R48a).
///
/// The close also leaves the parked snapshot (R69) and the references of the
/// standalone rule (R53) at the drive's latest emitted position.
fn close_open_drive(
    state: &mut OpenSessionState,
    delta: &mut LifecycleDelta,
    loe_end: bool,
    closing_position: Option<ProjectionPosition>,
    explicit_end: Option<i64>,
) -> Result<(), LifecycleError> {
    let Some(mut open) = state.open_drive.take() else {
        return Ok(());
    };
    record_drive_close_references(state, &open);
    let imported = state
        .imported_open
        .as_ref()
        .and_then(|refs| refs.drive_source_row_id)
        == Some(open.id);
    let drive_id = open.id;
    let checkpoint = open.still_checkpoint.take();
    let mut source = match checkpoint {
        Some(checkpoint) if loe_end => *checkpoint,
        _ => open,
    };
    // Children kept in memory by pure tests travel in the delta like any
    // other child (R11); the unit itself never fills the buffer.
    let buffered = std::mem::take(&mut source.positions);
    match finalize_drive(source, imported, explicit_end)? {
        Some(closed) => {
            delta.drives.push(closed.drive);
            delta.open_drive_positions.extend(buffered);
            delta.open_drive_positions.extend(closing_position);
        }
        None => {
            delta.discarded_drive_ids.push(drive_id);
            for mut position in buffered.into_iter().chain(closing_position) {
                position.drive_id = None;
                delta.positions.push(position);
            }
        }
    }
    Ok(())
}

/// What a closing drive leaves behind (C1 R53, R69), taken from the open drive
/// itself and not from its still checkpoint, because later still values are
/// newer facts about the same parked car.
///
/// The standalone rule continues from the drive's latest emitted position
/// whether that position stays with the drive or becomes standalone, so no
/// second row is stored for the spot the drive ended at; a drive without a
/// stored position leaves the references unchanged. The parked snapshot holds
/// the car's last known values for the energy baseline and the after-gap
/// charge (R61a, R70): the drive is over, so a counter seen with it belongs to
/// a session that has ended (`after_drive`).
fn record_drive_close_references(state: &mut OpenSessionState, open: &OpenDrive) {
    if let (Some(at_ms), Some(latitude), Some(longitude)) = (
        open.last_position_date_ms,
        open.last_latitude,
        open.last_longitude,
    ) {
        state.last_stationary_position_at_ms = Some(at_ms);
        state.last_standalone_latitude = Some(latitude);
        state.last_standalone_longitude = Some(longitude);
    }
    let folded_at = open.effective_last_folded_ms();
    state.parked_snapshot = Some(ParkedSnapshot {
        at_ms: Some(folded_at),
        seen_at_ms: folded_at,
        battery_level: open.last_soc,
        ideal_range_km: open.end_ideal_range_km,
        rated_range_km: open.last_rated_range_km,
        odometer_km: open.last_odometer,
        odometer_coarse: open.odometer_coarse,
        latitude: open.last_latitude.or(open.start_latitude),
        longitude: open.last_longitude.or(open.start_longitude),
        energy_added_kwh: open.last_charge_energy_added,
        after_drive: true,
        unplugged: false,
    });
}

/// Step 4, drive close checks in R43's order. The first that applies closes
/// the drive; after a close by items 1 to 5 the observation continues as one
/// with no open drive (its drive facts are not folded into the closed drive),
/// after items 6 and 7 it has been folded into the closed drive.
///
/// The unreachability, service and gap checks come first because they say the
/// drive ended at its last evidence, before anything in this observation;
/// folding this observation first would stretch the drive across the gap. Park
/// is the car's own end-of-drive state, and that observation carries the parked
/// position the end geofence label needs. A plugged-in car cannot be driving.
fn drive_close_step(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    let Some(open) = state.open_drive.as_ref() else {
        return Ok(());
    };
    let drive_facts = has_fresh_drive_facts(observation);
    let t_drive = observation.parsed.drive_timestamp_ms;
    let last_folded = open.effective_last_folded_ms();
    let last_motion = open.effective_last_motion_ms();
    let gap = observation.t_gap_ms;

    // Items 1 to 5: lack-of-evidence closes.
    let loe = observation.reach == Some(Reach::Asleep)
        || state.service_mode == Some(true)
        || (drive_facts && t_drive.saturating_sub(last_folded) > gap)
        || (!drive_facts && observation.t_obs().saturating_sub(last_folded) > gap)
        || observation.event() == Some(HubEventKind::NoData)
        || (drive_facts
            && observation.drive == DriveEvidence::NoInfo
            && t_drive.saturating_sub(last_motion) > gap);
    if loe {
        close_open_drive(state, delta, true, None, None)?;
        return Ok(());
    }
    // Item 6: Park in a fresh motion family. Item 7: plugged evidence.
    let parked = observation.drive == DriveEvidence::Park;
    let plugged = observation.charge.is_plugged();
    if parked || plugged {
        if drive_facts {
            let position = fold_into_open_drive(state, observation)?;
            close_open_drive(state, delta, false, position, Some(t_drive))?;
        } else {
            close_open_drive(state, delta, true, None, None)?;
        }
    }
    Ok(())
}

impl OpenCharge {
    /// A charge with nothing sampled yet (C1 R61). The start values, the
    /// energy baseline and the last odometer are set by the caller.
    fn empty(id: i64, car_id: i64, start_date_ms: i64) -> Self {
        Self {
            id,
            car_id,
            start_date_ms,
            start_battery_level: None,
            start_ideal_range_km: None,
            start_rated_range_km: None,
            start_latitude: None,
            start_longitude: None,
            is_dc: None,
            fast_charger_type: None,
            max_charger_power_kw: None,
            outside_temp_sum: 0.0,
            outside_temp_count: 0,
            first_energy_added: None,
            max_energy_added: None,
            last_energy_added: None,
            last_battery_level: None,
            last_ideal_range_km: None,
            last_rated_range_km: None,
            sample_count: 0,
            energy_used_kwh: None,
            last_sample_timestamp_ms: None,
            last_sample_power_kw: None,
            energy_added_carry_kwh: 0.0,
            prev_energy_added_kwh: None,
            ratio_used_kwh: 0.0,
            ratio_added_kwh: 0.0,
            fully_powered: false,
            pre_sample_kwh: None,
            phase_count: None,
            last_odometer_km: None,
            last_odometer_coarse: None,
            samples: Vec::new(),
        }
    }

    /// The time of the newest sample, or the start of a charge that has none
    /// yet. It is the end of a charge closed for lack of evidence (C1 R64) and
    /// the reference of the gap rule (R65).
    fn last_evidence_ms(&self) -> i64 {
        self.last_sample_timestamp_ms
            .unwrap_or(self.start_date_ms)
            .max(self.start_date_ms)
    }
}

/// Allocate the next charge id (C1 R10).
fn allocate_charge_id(state: &mut OpenSessionState) -> Result<i64, LifecycleError> {
    let id = state.next_charge_id;
    state.next_charge_id = id
        .checked_add(1)
        .ok_or(LifecycleError::IdentifierExhausted)?;
    Ok(id)
}

/// Allocate the next charge-sample id (C1 R10).
fn allocate_charge_sample_id(state: &mut OpenSessionState) -> Result<i64, LifecycleError> {
    let id = state.next_charge_sample_id;
    state.next_charge_sample_id = id
        .checked_add(1)
        .ok_or(LifecycleError::IdentifierExhausted)?;
    Ok(id)
}

/// Whether this observation anchors a charge sample (C1 R62): its
/// charge_session family is fresh, it carries `charging_state` or
/// `charge_energy_added`, and its time is later than the newest sample of the
/// open charge and than the imported charge watermark.
///
/// A charging state or a counter is what makes a sample interpretable within
/// the session's accounting. An observation whose charge facts are battery-only
/// (a stream frame) or carry neither creates no sample and judges no
/// continuity: parked battery values are covered by the standalone rule
/// (R53), and the stream carries no charge state to judge a gap by.
fn anchors_charge_sample(state: &OpenSessionState, observation: &Observation<'_>) -> bool {
    let parsed = observation.parsed;
    if !observation.fresh.charge_session
        || (parsed.charging_state.is_none() && parsed.charge_energy_added.is_none())
    {
        return false;
    }
    let t_charge = parsed.charge_timestamp_ms;
    later_than(t_charge, state.imported_charge_watermark_ms)
        && state
            .open_charge
            .as_ref()
            .and_then(|open| open.last_sample_timestamp_ms)
            .is_none_or(|last| t_charge > last)
}

/// Whether an open charge continues across a gap longer than `T_gap` (C1 R65).
///
/// All three must hold: the class is Active, Hold or End; the counter is known
/// now and was known before, and did not fall; and the car is unmoved, that is
/// this observation carries an odometer, the charge knows its last one, and the
/// reading rose by less than [`K_DMIN_KM`] (a fall from mixed-source rounding
/// counts as unmoved). Within one Tesla session the counter does not fall
/// (R61a), and an unmoved odometer shows the car stayed where it was plugged,
/// so the observation is consistent with one plug-in across any connectivity or
/// Hub gap. It does not prove it: an unplug and replug at the same spot during
/// the gap whose new counter already exceeds the old value cannot be told apart
/// (known limit 1). The last known counter and odometer are compared, not only
/// the last sample's, so a sample without a counter or a stream frame does not
/// break the comparison.
fn charge_continues_across_gap(open: &OpenCharge, observation: &Observation<'_>) -> bool {
    let parsed = observation.parsed;
    let counter_held = matches!(
        (parsed.charge_energy_added, open.last_energy_added),
        (Some(now), Some(last)) if now >= last
    );
    observation.charge != ChargeEvidence::None
        && counter_held
        && movement_between(open.last_odometer_km, parsed.odometer) == Movement::Unmoved
}

/// Step 4, charge close checks in R64's order. Closes for lack of evidence end
/// at the newest sample, never earlier than the start.
///
/// Triggers, in order: reach `asleep` (R26); service mode (R25, including the
/// `service_refused` event); Motion drive evidence that survived R22, because
/// a car cannot drive with the cable connected; for a sample-anchoring
/// observation, a counter below the open session's (R66, a new Tesla session
/// began); and a sample-anchoring observation after a gap that fails
/// continuity (R65). An observation without a sample-anchoring charge fact (a
/// discovery, a stream frame, drive-only data, a `no_data` event) closes a
/// charge only by the first three triggers, however late it comes: a charge,
/// unlike a drive, can be shown continuous by its next sample, and a wake-up
/// discovery or a battery-only frame arriving before that sample must not cut
/// a no-signal garage charge in two.
fn charge_close_step(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    let Some(open) = state.open_charge.as_ref() else {
        return Ok(());
    };
    let parsed = observation.parsed;
    let lost_evidence = observation.reach == Some(Reach::Asleep)
        || state.service_mode == Some(true)
        || observation.drive == DriveEvidence::Motion;
    let close = lost_evidence
        || (anchors_charge_sample(state, observation) && {
            let counter_fell = matches!(
                (parsed.charge_energy_added, open.last_energy_added),
                (Some(now), Some(last)) if now < last
            );
            let gap = parsed
                .charge_timestamp_ms
                .saturating_sub(open.last_evidence_ms())
                > observation.t_gap_ms;
            counter_fell || (gap && !charge_continues_across_gap(open, observation))
        });
    if close {
        let end_ms = open.last_evidence_ms();
        close_open_charge(state, delta, end_ms, false)?;
    }
    Ok(())
}

/// Close the open charge (C1 R63, R64, R69, R71).
///
/// The end is never earlier than the start, so the closed row always passes
/// the timeline check of `finalize_charge`. The row is assembled by the
/// retained `finalize_charge`; the unit adds one `(charge id, latitude,
/// longitude)` entry when the start coordinates are known, which storage uses
/// to label the charge with the one geofence rule (R11, R81). Children travel
/// in the delta (R11), so a buffer kept in memory by a pure test is forwarded
/// like any other child and is empty in production. The close leaves the
/// parked snapshot behind (R69), with `unplugged` set when a `Disconnected`
/// sample ended the session.
fn close_open_charge(
    state: &mut OpenSessionState,
    delta: &mut LifecycleDelta,
    end_ms: i64,
    unplugged: bool,
) -> Result<(), LifecycleError> {
    let Some(open) = state.open_charge.take() else {
        return Ok(());
    };
    let end_ms = end_ms.max(open.start_date_ms);
    state.parked_snapshot = Some(snapshot_after_charge(&open, end_ms, unplugged));
    let charge_id = open.id;
    let start_coordinates = open.start_latitude.zip(open.start_longitude);
    let closed = finalize_charge(open, Some(end_ms))?;
    delta.charges.push(closed.charge);
    if let Some((latitude, longitude)) = start_coordinates {
        delta
            .charge_start_coordinates
            .push((charge_id, latitude, longitude));
    }
    delta.open_charge_samples.extend(closed.samples);
    Ok(())
}

/// Step 6, drive open (C1 R41, R41a, R9).
///
/// Motion evidence (after R22) with no open drive and the car not in service
/// opens a drive at `t_drive`; motion is the only direct evidence that a trip
/// began. The start is never earlier than the end of a session closed in step
/// 4 of this observation (R9). When the parked snapshot is no older than
/// `T_gap`, the drive takes its start values from it where it knows them: the
/// odometer reading taken at the origin counts the distance driven before the
/// first Motion detection (up to one parked polling interval), and the start
/// coordinates name where the car was parked. Within `T_gap` no other trip can
/// have happened unobserved (R45), so the lead is sound; the start time is not
/// moved, because the Hub does not know when within that interval the car began
/// to move. The snapshot no longer describes a parked car and is cleared.
fn drive_open_step(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    if observation.drive != DriveEvidence::Motion
        || state.open_drive.is_some()
        || state.service_mode == Some(true)
    {
        return Ok(());
    }
    let t_drive = observation.parsed.drive_timestamp_ms;
    let after_closed = delta
        .drives
        .iter()
        .map(|drive| drive.end_date_ms)
        .chain(delta.charges.iter().filter_map(|charge| charge.end_date_ms))
        .max();
    let start = after_closed.map_or(t_drive, |end| end.max(t_drive));
    let id = state.next_drive_id;
    state.next_drive_id = id
        .checked_add(1)
        .ok_or(LifecycleError::IdentifierExhausted)?;
    let mut open = OpenDrive::empty(id, observation.car_id, start);
    if let Some(snapshot) = state.parked_snapshot.as_ref()
        && t_drive.saturating_sub(snapshot.seen_at_ms) <= observation.t_gap_ms
    {
        if let Some(odometer) = snapshot.odometer_km {
            open.first_odometer = Some(odometer);
            open.last_odometer = Some(odometer);
            open.odometer_coarse = snapshot.odometer_coarse;
        }
        if let (Some(latitude), Some(longitude)) = (snapshot.latitude, snapshot.longitude) {
            open.start_latitude = Some(latitude);
            open.start_longitude = Some(longitude);
        }
        open.start_soc = snapshot.battery_level;
        open.start_ideal_range_km = snapshot.ideal_range_km;
        open.start_rated_range_km = snapshot.rated_range_km;
    }
    state.parked_snapshot = None;
    state.open_drive = Some(open);
    Ok(())
}

/// Step 6, charge open (C1 R61) and after-gap charge (R70, R9).
///
/// With no open charge and no open drive after the closes, and the car not in
/// service, a sample-anchoring observation opens a charge in two cases:
///
/// - Active evidence (R61): only flowing energy starts a session, so failed
///   starts (`Starting` then `NoPower`) never create empty sessions and no
///   discard path is needed. The charge starts at `t_charge`; a session first
///   observed in progress starts at its first sample, an upper bound of the
///   true start (known limit 3), and its earlier energy is still counted
///   through the baseline. Reaching back to the last parked data instead would
///   attach hours of parked, possibly asleep, time to a session that is
///   observable only from now on. Start values are this observation's own
///   (R6a exception), coordinates fall back to the parked snapshot when the car
///   is unmoved against it.
/// - After-gap evidence (R70): the counter proves energy flowed that no
///   session recorded, and the class says the charge has ended or is held.
///   The charge starts at the time the snapshot accounts energy up to, so its
///   start and end are true bounds of the delivery.
///
/// Either way the energy baseline is preset by R61a and this observation is
/// the first sample (step 7). A session opened here starts no earlier than the
/// end of any session closed in step 4 of the same observation (R9).
fn charge_open_step(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    if state.open_charge.is_some()
        || state.open_drive.is_some()
        || state.service_mode == Some(true)
        || !anchors_charge_sample(state, observation)
    {
        return Ok(());
    }
    let parsed = observation.parsed;
    let t_charge = parsed.charge_timestamp_ms;
    let snapshot = state.parked_snapshot.as_ref();
    let unmoved = snapshot.is_some_and(|snapshot| {
        movement_between(snapshot.odometer_km, parsed.odometer) == Movement::Unmoved
    });
    let after_closed = delta
        .drives
        .iter()
        .map(|drive| drive.end_date_ms)
        .chain(delta.charges.iter().filter_map(|charge| charge.end_date_ms))
        .max();
    // The observation's own coordinates, else the snapshot's when unmoved.
    let observed_coordinates = valid_coordinates(parsed.latitude, parsed.longitude);
    let snapshot_coordinates = snapshot
        .filter(|_| unmoved)
        .and_then(|snapshot| valid_coordinates(snapshot.latitude, snapshot.longitude));
    let coordinates = observed_coordinates.or(snapshot_coordinates);

    let (start, start_battery, start_ideal, start_rated) = match observation.charge {
        ChargeEvidence::Active => (
            t_charge,
            parsed.battery_level,
            parsed.ideal_range_km,
            parsed.rated_range_km,
        ),
        ChargeEvidence::Hold | ChargeEvidence::Complete | ChargeEvidence::Disconnected => {
            let Some(snapshot) = snapshot else {
                return Ok(());
            };
            let Some(after_gap) = after_gap_charge(snapshot, parsed, observation.charge) else {
                return Ok(());
            };
            // The snapshot's time can lead the charge group's time by the
            // seconds two groups of one response may differ; the start never
            // follows the first sample.
            let start = after_gap.start_ms.min(t_charge);
            if after_gap.unmoved {
                (
                    start,
                    snapshot.battery_level,
                    snapshot.ideal_range_km,
                    snapshot.rated_range_km,
                )
            } else {
                (start, None, None, None)
            }
        }
        ChargeEvidence::None => return Ok(()),
    };
    let baseline = charge_energy_baseline(snapshot, parsed.charge_energy_added, parsed.odometer);
    let start = after_closed.map_or(start, |end| end.max(start));
    let id = allocate_charge_id(state)?;
    let mut open = OpenCharge::empty(id, observation.car_id, start);
    open.start_battery_level = start_battery;
    open.start_ideal_range_km = start_ideal;
    open.start_rated_range_km = start_rated;
    if let Some((latitude, longitude)) = coordinates {
        open.start_latitude = Some(latitude);
        open.start_longitude = Some(longitude);
    }
    open.last_odometer_km = parsed.odometer;
    open.last_odometer_coarse = parsed.odometer.map(|_| parsed.kind != SourceKind::Full);
    open.first_energy_added = Some(baseline);
    state.open_charge = Some(open);
    Ok(())
}

/// Step 7, fold into the open drive (C1 R42). Each observation with a fresh
/// motion or position family while a drive is open, including the opening and
/// the closing observation, is folded once; a drive that this observation
/// closed in step 4 is no longer open, so it is not folded again.
fn drive_fold_step(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    if state.open_drive.is_none() || !has_fresh_drive_facts(observation) {
        return Ok(());
    }
    if let Some(position) = fold_into_open_drive(state, observation)? {
        delta.open_drive_positions.push(position);
    }
    Ok(())
}

/// The charge-sample row of one observation (C1 R36, R6a). The row takes every
/// field the observation carries, from any group and whatever that family's
/// freshness: a cached value that Tesla re-serves with an old timestamp is
/// still the car's latest statement of that field at the anchor time, and
/// dropping it would empty the battery and temperature columns of most rows.
/// No row moves time backwards, because its timestamp is its anchor's
/// `t_charge`. Fields the observation does not carry stay `None` or follow the
/// retained carry-forward of the thermal flags (R39).
fn charge_sample_from(id: i64, charge_id: i64, parsed: &ParsedSample) -> ProjectionChargeSample {
    ProjectionChargeSample {
        id,
        charge_process_id: charge_id,
        timestamp_ms: parsed.charge_timestamp_ms,
        battery_level: parsed.battery_level,
        usable_battery_level: parsed.usable_battery_level,
        charge_energy_added_kwh: parsed.charge_energy_added,
        charger_power_kw: parsed.charger_power_kw,
        charger_voltage: parsed.charger_voltage,
        charger_actual_current: parsed.charger_actual_current,
        charger_pilot_current: parsed.charger_pilot_current,
        charger_phases: parsed.charger_phases,
        ideal_range_km: parsed.ideal_range_km,
        rated_range_km: parsed.rated_range_km,
        outside_temp_c: parsed.outside_temp,
        battery_heater_on: parsed.battery_heater_on,
        battery_heater: parsed.battery_heater,
        battery_heater_no_power: parsed.battery_heater_no_power,
        not_enough_power_to_heat: parsed.not_enough_power_to_heat,
        fast_charger_present: parsed.fast_charger_present,
        fast_charger_brand: parsed.fast_charger_brand.clone(),
        fast_charger_type: parsed.fast_charger_type.clone(),
        charge_cable: parsed.charge_cable.clone(),
    }
}

/// Step 7, charge sample (C1 R62), last odometer (R62) and the close on
/// explicit end evidence (R63).
///
/// A sample-anchoring observation becomes one sample row in `delta`, in this
/// order: build the row, `observe_charge_aggregate`, `observe_charge_sample`,
/// set `last_sample_timestamp_ms`, push. The unit never writes the C2
/// aggregates or `first_energy_added` here (R80); the baseline was preset at
/// the open (R61a). Every fresh odometer family while the charge is open
/// updates `last_odometer_km`, the reading the next gap judges movement by
/// (R65).
///
/// `Complete` and `Disconnected` are the car's own end statements and carry the
/// final energy counter, so the observation is taken as the last sample and the
/// charge closes at `t_charge`. `Starting`, `Stopped` and `NoPower` keep it
/// open, so a load-managed or user-paused charge is one row. A session that
/// pauses and is later unplugged ends at the `Disconnected` sample, so its
/// `duration_min` includes the idle connected time (known limit 5).
fn charge_sample_step(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    if state.open_charge.is_none() {
        return Ok(());
    }
    let parsed = observation.parsed;
    let anchors = anchors_charge_sample(state, observation);
    if anchors && let Some(charge_id) = state.open_charge.as_ref().map(|open| open.id) {
        let id = allocate_charge_sample_id(state)?;
        let sample = charge_sample_from(id, charge_id, parsed);
        if let Some(open) = state.open_charge.as_mut() {
            observe_charge_aggregate(open, parsed);
            observe_charge_sample(open, &sample);
            open.last_sample_timestamp_ms = Some(sample.timestamp_ms);
        }
        delta.open_charge_samples.push(sample);
    }
    if observation.fresh.odometer
        && let (Some(open), Some(odometer)) = (state.open_charge.as_mut(), parsed.odometer)
    {
        open.last_odometer_km = Some(odometer);
        open.last_odometer_coarse = Some(parsed.kind != SourceKind::Full);
    }
    if anchors && observation.charge.is_end() {
        let unplugged = observation.charge == ChargeEvidence::Disconnected;
        close_open_charge(state, delta, parsed.charge_timestamp_ms, unplugged)?;
    }
    Ok(())
}

/// Step 8, standalone position (C1 R53, R74).
///
/// With no drive open after step 6 and a fresh position family with a valid
/// coordinate pair, later than the last stored standalone position and the
/// imported drive watermark, one row is stored when the car has not had one
/// for [`K_PARK_INTERVAL`], or has moved at least [`K_DMIN_KM`] from the last
/// stored spot, or is in service mode with Motion evidence (movement without a
/// drive: a tow, a ferry, service). The `t_drive >` check also keeps a second
/// row from being stored for an observation whose position the drive close
/// already stored. An open charge does not suppress it, because samples carry
/// no coordinates.
///
/// This is the brief's sanctioned exception to "never downsampled" for parked
/// observations: the per-position fields of parked observations that produce
/// neither a standalone position nor a charge sample are not kept as rows; the
/// current-state view still shows the latest values. Positions of open drives
/// and charge samples are never thinned.
fn standalone_position_step(
    state: &mut OpenSessionState,
    observation: &Observation<'_>,
    delta: &mut LifecycleDelta,
) -> Result<(), LifecycleError> {
    if state.open_drive.is_some() || !observation.fresh.position {
        return Ok(());
    }
    let parsed = observation.parsed;
    let (Some(latitude), Some(longitude)) = (parsed.latitude, parsed.longitude) else {
        return Ok(());
    };
    let t_drive = parsed.drive_timestamp_ms;
    if state
        .last_stationary_position_at_ms
        .is_some_and(|last| t_drive <= last)
        || state
            .imported_drive_watermark_ms
            .is_some_and(|watermark| t_drive <= watermark)
    {
        return Ok(());
    }
    let interval_elapsed = state
        .last_stationary_position_at_ms
        .is_none_or(|last| t_drive.saturating_sub(last) >= K_PARK_INTERVAL_MS);
    let moved = match (
        state.last_standalone_latitude,
        state.last_standalone_longitude,
    ) {
        (Some(last_latitude), Some(last_longitude)) => {
            match (
                crate::location::Wgs84Point::new(last_latitude, last_longitude),
                crate::location::Wgs84Point::new(latitude, longitude),
            ) {
                (Ok(from), Ok(to)) => {
                    crate::location::great_circle_distance_m(from, to) >= K_DMIN_KM * 1000.0
                }
                _ => false,
            }
        }
        _ => false,
    };
    let service_motion =
        state.service_mode == Some(true) && observation.drive == DriveEvidence::Motion;
    if !(interval_elapsed || moved || service_motion) {
        return Ok(());
    }
    let id = next_position_id(state)?;
    if let Some(position) = position_from_sample(id, None, observation.car_id, parsed)? {
        delta.positions.push(position);
        state.last_stationary_position_at_ms = Some(t_drive);
        state.last_standalone_latitude = Some(latitude);
        state.last_standalone_longitude = Some(longitude);
    }
    Ok(())
}

/// Step 8, parked snapshot update (C1 R69), the part the drive lead needs.
///
/// While no drive and no charge is open after step 7, each fresh family
/// updates the fields it covers: battery (level and ranges), odometer (value
/// and coarseness), position (coordinates) and charge_session (the counter
/// when carried; a `Disconnected` class sets `unplugged`). `seen_at_ms` is the
/// latest fact time folded in; `at_ms` advances only for a sample-anchoring
/// charge fact, because only a charging state or counter states the energy at
/// that time. With no snapshot the first such update creates one with `at_ms`
/// unknown unless it is a sample-anchoring charge fact. The snapshot is built
/// at the close of a drive by [`record_drive_close_references`]; the one at the
/// close of a charge belongs to the charge part.
fn parked_snapshot_step(state: &mut OpenSessionState, observation: &Observation<'_>) {
    if state.open_drive.is_some() || state.open_charge.is_some() {
        return;
    }
    let parsed = observation.parsed;
    let fresh = observation.fresh;
    if !(fresh.battery || fresh.odometer || fresh.position || fresh.charge_session) {
        return;
    }
    let anchors_charge = fresh.charge_session
        && (parsed.charging_state.is_some() || parsed.charge_energy_added.is_some());
    let snapshot = state.parked_snapshot.get_or_insert_with(|| ParkedSnapshot {
        at_ms: None,
        seen_at_ms: i64::MIN,
        ..ParkedSnapshot::default()
    });
    let mut seen_at_ms = snapshot.seen_at_ms;
    if fresh.battery || fresh.charge_session {
        seen_at_ms = seen_at_ms.max(parsed.charge_timestamp_ms);
    }
    if fresh.odometer {
        seen_at_ms = seen_at_ms.max(parsed.vehicle_timestamp_ms);
    }
    if fresh.position {
        seen_at_ms = seen_at_ms.max(parsed.drive_timestamp_ms);
    }
    snapshot.seen_at_ms = seen_at_ms;
    if fresh.battery {
        if let Some(level) = parsed.battery_level {
            snapshot.battery_level = Some(level);
        }
        if let Some(ideal) = parsed.ideal_range_km {
            snapshot.ideal_range_km = Some(ideal);
        }
        if let Some(rated) = parsed.rated_range_km {
            snapshot.rated_range_km = Some(rated);
        }
    }
    if fresh.odometer
        && let Some(odometer) = parsed.odometer
    {
        snapshot.odometer_km = Some(odometer);
        snapshot.odometer_coarse = parsed.kind != SourceKind::Full;
    }
    if fresh.position
        && let (Some(latitude), Some(longitude)) = (parsed.latitude, parsed.longitude)
    {
        snapshot.latitude = Some(latitude);
        snapshot.longitude = Some(longitude);
    }
    if fresh.charge_session {
        if let Some(energy) = parsed.charge_energy_added {
            snapshot.energy_added_kwh = Some(energy);
        }
        if observation.charge == ChargeEvidence::Disconnected {
            snapshot.unplugged = true;
        }
    }
    if anchors_charge {
        snapshot.at_ms = Some(snapshot.at_ms.map_or(parsed.charge_timestamp_ms, |at| {
            at.max(parsed.charge_timestamp_ms)
        }));
    }
}
