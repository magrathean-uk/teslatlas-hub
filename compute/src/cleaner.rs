// SPDX-License-Identifier: Apache-2.0
//! App-equivalent raw GPS cleaning before deterministic tile generation.

/// Prepared eligibility bound. An oversized drive stays on the App's local
/// cleaner; callers must not publish a partially cleaned prepared month.
pub const MAX_RAW_MAP_POSITIONS_PER_DRIVE: usize = 64 * 1024;

/// One raw position, ordered by `(date_ms, source_position_id)` by the caller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawMapPosition {
    pub date_ms: i64,
    pub latitude: f64,
    pub longitude: f64,
    pub speed_kmh: Option<i16>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanMapError {
    /// The drive cannot use a prepared map; generate it on the App instead.
    OversizedDrive,
}

impl std::fmt::Display for CleanMapError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OversizedDrive => {
                formatter.write_str("raw map drive exceeds prepared eligibility bound")
            }
        }
    }
}

impl std::error::Error for CleanMapError {}

/// Match the App's `prepare_positions_for_tile_rendering`: reject invalid GPS,
/// remove bridgeable outliers and parked drift, then cast the retained f64
/// coordinates to f32 exactly once. There is no smoothing or interpolation.
///
/// Cleaning is drive-scoped and cannot be applied independently to fragments.
/// A drive over the bound is explicitly unavailable for Hub preparation so the
/// App's existing full-route local path can handle it without parity claims.
pub fn prepare_positions_for_tile_rendering_v1(
    positions: &[RawMapPosition],
) -> Result<Vec<(f32, f32)>, CleanMapError> {
    if positions.len() > MAX_RAW_MAP_POSITIONS_PER_DRIVE {
        return Err(CleanMapError::OversizedDrive);
    }
    let valid: Vec<RawMapPosition> = positions
        .iter()
        .copied()
        .filter(|position| {
            position.latitude.is_finite()
                && position.longitude.is_finite()
                && (-90.0..=90.0).contains(&position.latitude)
                && (-180.0..=180.0).contains(&position.longitude)
                && !(position.latitude == 0.0 && position.longitude == 0.0)
        })
        .collect();
    let cleaned = if valid.len() < 3 {
        valid
    } else {
        remove_stationary_drift(&remove_outliers(&valid))
    };
    Ok(cleaned
        .into_iter()
        .map(|position| (position.latitude as f32, position.longitude as f32))
        .collect())
}

fn remove_outliers(positions: &[RawMapPosition]) -> Vec<RawMapPosition> {
    let mut result = Vec::with_capacity(positions.len());
    result.push(positions[0]);
    for index in 1..positions.len() {
        if index == positions.len() - 1 {
            if result
                .last()
                .is_none_or(|last| last.date_ms != positions[index].date_ms)
            {
                result.push(positions[index]);
            }
            continue;
        }
        let previous = result.last().unwrap_or(&positions[index - 1]);
        if is_bridgeable_outlier(previous, &positions[index], &positions[index + 1]) {
            continue;
        }
        result.push(positions[index]);
    }
    result
}

fn is_bridgeable_outlier(
    previous: &RawMapPosition,
    current: &RawMapPosition,
    next: &RawMapPosition,
) -> bool {
    let Some(bridge_speed) = segment_speed_kmh(previous, next) else {
        return false;
    };
    if bridge_speed > 300.0 {
        return false;
    }
    let before = segment_speed_kmh(previous, current);
    let after = segment_speed_kmh(current, next);
    if before.is_some_and(|speed| speed > 300.0) || after.is_some_and(|speed| speed > 300.0) {
        return true;
    }
    match (before, after) {
        (Some(before), Some(after)) => {
            let before_seconds = (current.date_ms - previous.date_ms) as f64 / 1000.0;
            let after_seconds = (next.date_ms - current.date_ms) as f64 / 1000.0;
            if before_seconds <= 0.0 || after_seconds <= 0.0 {
                return false;
            }
            let acceleration = ((after - before) / 3.6) / ((before_seconds + after_seconds) / 2.0);
            acceleration.abs() > 10.0
        }
        _ => false,
    }
}

fn segment_speed_kmh(start: &RawMapPosition, end: &RawMapPosition) -> Option<f64> {
    let seconds = (end.date_ms - start.date_ms) as f64 / 1000.0;
    if seconds <= 0.0 {
        return None;
    }
    Some(
        haversine_distance_m(start.latitude, start.longitude, end.latitude, end.longitude)
            / seconds
            * 3.6,
    )
}

fn remove_stationary_drift(positions: &[RawMapPosition]) -> Vec<RawMapPosition> {
    if positions.len() < 2 {
        return positions.to_vec();
    }
    let mut result = Vec::with_capacity(positions.len());
    result.push(positions[0]);
    for &current in positions.iter().skip(1) {
        let Some(previous) = result.last() else {
            continue;
        };
        let distance = haversine_distance_m(
            previous.latitude,
            previous.longitude,
            current.latitude,
            current.longitude,
        );
        let minimum = if current.speed_kmh.is_some_and(|speed| speed < 5) {
            10.0
        } else {
            5.0
        };
        if distance >= minimum {
            result.push(current);
        }
    }
    result
}

fn haversine_distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let lat1_rad = lat1.to_radians();
    let lat2_rad = lat2.to_radians();
    let delta_lat = (lat2 - lat1).to_radians();
    let delta_lon = (lon2 - lon1).to_radians();
    let a = (delta_lat / 2.0).sin().powi(2)
        + lat1_rad.cos() * lat2_rad.cos() * (delta_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    6_371.0 * c * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(
        date_ms: i64,
        latitude: f64,
        longitude: f64,
        speed_kmh: Option<i16>,
    ) -> RawMapPosition {
        RawMapPosition {
            date_ms,
            latitude,
            longitude,
            speed_kmh,
        }
    }

    #[test]
    fn invalid_and_null_island_rows_are_removed_but_near_zero_is_valid() {
        let points = [
            point(0, 0.0, 0.0, None),
            point(1_000, 0.0001, 0.0, None),
            point(2_000, f64::NAN, 10.0, None),
            point(3_000, 91.0, 10.0, None),
        ];
        assert_eq!(
            prepare_positions_for_tile_rendering_v1(&points).unwrap(),
            vec![(0.0001_f32, 0.0)]
        );
    }

    #[test]
    fn bridgeable_spike_is_removed_without_smoothing_real_geometry() {
        let points = [
            point(0, 47.5000, 19.0000, Some(50)),
            point(10_000, 47.5010, 19.0010, Some(50)),
            point(20_000, 48.5000, 20.0000, Some(50)),
            point(30_000, 47.5020, 19.0020, Some(50)),
            point(40_000, 47.5030, 19.0030, Some(50)),
        ];
        assert_eq!(
            prepare_positions_for_tile_rendering_v1(&points).unwrap(),
            vec![
                (47.5000_f32, 19.0000_f32),
                (47.5010_f32, 19.0010_f32),
                (47.5020_f32, 19.0020_f32),
                (47.5030_f32, 19.0030_f32),
            ]
        );
    }

    #[test]
    fn parked_drift_uses_current_speed_and_equal_time_final_row_is_dropped() {
        let points = [
            point(0, 51.5, -0.1, Some(0)),
            point(1_000, 51.50005, -0.1, Some(0)),
            point(2_000, 51.50008, -0.1, Some(10)),
            point(2_000, 51.50100, -0.1, Some(10)),
        ];
        assert_eq!(
            prepare_positions_for_tile_rendering_v1(&points).unwrap(),
            vec![(51.5_f32, -0.1_f32), (51.50008_f32, -0.1_f32)]
        );
    }

    #[test]
    fn oversized_drive_is_ineligible_for_preparation() {
        let at_bound = vec![point(0, 51.5, -0.1, None); MAX_RAW_MAP_POSITIONS_PER_DRIVE];
        assert!(prepare_positions_for_tile_rendering_v1(&at_bound).is_ok());
        let points = vec![point(0, 51.5, -0.1, None); MAX_RAW_MAP_POSITIONS_PER_DRIVE + 1];
        assert_eq!(
            prepare_positions_for_tile_rendering_v1(&points),
            Err(CleanMapError::OversizedDrive)
        );
    }
}
