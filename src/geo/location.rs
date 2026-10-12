// SPDX-License-Identifier: AGPL-3.0-only

//! WGS84 point type, location errors and the one great-circle distance every
//! lifecycle distance, boundary and overlap rule uses (C1 R55).

use std::fmt;

/// TeslaMate-compatible upper bound. TeslaMate requires the radius to be
/// strictly less than five kilometres.
pub const MAX_GEOFENCE_RADIUS_METRES: f64 = 5_000.0;

/// Mean Earth radius R1 in metres, the IUGG value 6,371,008.8 m (C1 R55).
///
/// The spherical model is accurate enough on the scales the Hub measures,
/// metres to kilometres: its error against the WGS84 ellipsoid is below 0.5 %,
/// which is under a metre at 100 m and below GNSS accuracy. One radius keeps
/// every site (drive keep rule, standalone-position movement, parked-car
/// movement, geofence boundary) on the same number.
pub const MEAN_EARTH_RADIUS_M: f64 = 6_371_008.8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wgs84Point {
    pub latitude: f64,
    pub longitude: f64,
}

impl Wgs84Point {
    pub fn new(latitude: f64, longitude: f64) -> Result<Self, LocationError> {
        if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
            return Err(LocationError::InvalidLatitude);
        }
        if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
            return Err(LocationError::InvalidLongitude);
        }
        Ok(Self {
            latitude,
            longitude,
        })
    }
}

/// Great-circle distance between two points in metres (C1 R55): the haversine
/// formula on a sphere of radius [`MEAN_EARTH_RADIUS_M`].
///
/// `h = sin^2(dphi/2) + cos(phi_a) cos(phi_b) sin^2(dlambda/2)`, clamped to
/// `[0, 1]`, then `d = 2 R asin(sqrt(h))`. Haversine stays accurate at short
/// distances, where the spherical law of cosines loses precision. The
/// `sin^2(dlambda/2)` term is periodic in longitude, so the antimeridian needs
/// no special case, and at the poles `cos(phi) = 0` removes longitude.
pub fn great_circle_distance_m(a: Wgs84Point, b: Wgs84Point) -> f64 {
    let phi_a = a.latitude.to_radians();
    let phi_b = b.latitude.to_radians();
    let delta_phi = (b.latitude - a.latitude).to_radians();
    let delta_lambda = (b.longitude - a.longitude).to_radians();
    let half_phi = (delta_phi / 2.0).sin();
    let half_lambda = (delta_lambda / 2.0).sin();
    let h = (half_phi * half_phi + phi_a.cos() * phi_b.cos() * half_lambda * half_lambda)
        .clamp(0.0, 1.0);
    2.0 * MEAN_EARTH_RADIUS_M * h.sqrt().asin()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationError {
    InvalidId,
    EmptyName,
    InvalidLatitude,
    InvalidLongitude,
    InvalidRadius,
}

impl fmt::Display for LocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidId => "geofence id must be positive",
            Self::EmptyName => "geofence name must not be empty",
            Self::InvalidLatitude => "latitude must be finite and between -90 and 90 degrees",
            Self::InvalidLongitude => "longitude must be finite and between -180 and 180 degrees",
            Self::InvalidRadius => "radius must be finite, positive, and below 5000 metres",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for LocationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_points_are_rejected() {
        assert_eq!(
            Wgs84Point::new(f64::NAN, 0.0),
            Err(LocationError::InvalidLatitude)
        );
        assert_eq!(
            Wgs84Point::new(91.0, 0.0),
            Err(LocationError::InvalidLatitude)
        );
        assert_eq!(
            Wgs84Point::new(0.0, f64::INFINITY),
            Err(LocationError::InvalidLongitude)
        );
        assert_eq!(
            Wgs84Point::new(0.0, 181.0),
            Err(LocationError::InvalidLongitude)
        );
    }

    fn point(latitude: f64, longitude: f64) -> Wgs84Point {
        Wgs84Point::new(latitude, longitude).expect("valid point")
    }

    #[test]
    fn distance_of_one_millidegree_of_latitude_is_111_195_metres() {
        let distance = great_circle_distance_m(point(10.0, 20.0), point(10.001, 20.0));
        assert!((distance - 111.195).abs() < 0.001, "{distance}");
    }

    #[test]
    fn distance_is_symmetric_and_zero_for_the_same_point() {
        let a = point(51.5, -0.1);
        let b = point(48.85, 2.35);
        assert_eq!(great_circle_distance_m(a, b), great_circle_distance_m(b, a));
        assert_eq!(great_circle_distance_m(a, a), 0.0);
    }

    #[test]
    fn distance_wraps_at_the_antimeridian_and_ignores_longitude_at_the_poles() {
        let across = great_circle_distance_m(point(0.0, 179.9999), point(0.0, -179.9999));
        assert!((across - 22.239).abs() < 0.01, "{across}");
        let at_pole = great_circle_distance_m(point(90.0, 0.0), point(90.0, 135.0));
        assert!(at_pole.abs() < 1e-6, "{at_pole}");
        let half_circle = great_circle_distance_m(point(0.0, 0.0), point(0.0, 180.0));
        assert!((half_circle - std::f64::consts::PI * MEAN_EARTH_RADIUS_M).abs() < 1e-6);
    }
}
