// SPDX-License-Identifier: AGPL-3.0-only

//! WGS84 point type, location errors and the one great-circle distance every
//! lifecycle distance, boundary and overlap rule uses (C1 R55).

use std::fmt;

/// Mean Earth radius R1 in metres, the IUGG value 6,371,008.8 m (C1 R55).
///
/// The spherical model is accurate enough on the scales the Hub measures,
/// metres to kilometres: its error against the WGS84 ellipsoid is below 0.5 %,
/// which is under a metre at 100 m and below GNSS accuracy. One radius keeps
/// every site (drive keep rule, standalone-position movement, parked-car
/// movement, geofence boundary) on the same number.
pub const MEAN_EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Smallest radius a Hub save may give a fence, 10 m (C8 R6, section 1.1).
///
/// A fence is tested against a position carrying a receiver error of about
/// 4.9 m under open sky, and its centre is usually a map pin or a past
/// position with an error of the same size, so the two add to about 9.8 m: a
/// smaller radius would miss a car standing on the intended spot. 10 m still
/// separates a garage from the drive in front of it, so nested fences stay
/// possible. It applies to radii a user types, not to kept or copied ones
/// (C8 R7, C8 R13).
pub const MIN_SAVED_GEOFENCE_RADIUS_METRES: f64 = 10.0;

/// Largest radius a Hub save may give a fence, 30,000 m (C8 R6, section 1.1).
///
/// The largest place one label usefully names is a town or city taken as a
/// whole; a 30 km circle holds about 2,827 km2. A smaller fence inside a
/// larger one wins (C1 R57), so the bound is not needed for specificity: its
/// job is to catch a slip of an extra digit or two. Used for Hub saves only.
pub const MAX_GEOFENCE_RADIUS_METRES: f64 = 30_000.0;

/// Largest radius import activation accepts, 32,767 m (C8 R13, section 1.1):
/// the largest value of the import source's `smallint` radius column. Every
/// positive radius that column can hold is a usable circle (C1 R58), so no
/// imported fence is refused for size. It is also the stored radius bound,
/// which therefore holds every radius an import activates (C8 R12).
pub const MAX_IMPORTED_GEOFENCE_RADIUS_METRES: f64 = 32_767.0;

/// Radius a created fence takes when the save gives none, 50 m (C8 R7).
///
/// A new fence is placed by a map pin on a building while the car stands on a
/// drive or a bay tens of metres away; 50 m covers a house with its drive and
/// stays inside one property or site in most places.
pub const DEFAULT_GEOFENCE_RADIUS_METRES: f64 = 50.0;

/// First id the Hub allocates to a fence it creates, 2^31 (C8 R3). The import
/// source's id column is a 32-bit integer, and import activation refuses ids
/// from this value, so an import can never overwrite a fence created in the
/// Hub.
pub const FIRST_HUB_GEOFENCE_ID: i64 = 2_147_483_648;

/// Largest id import activation accepts, 2^31 - 1 (C8 R13).
pub const MAX_IMPORTED_GEOFENCE_ID: i64 = FIRST_HUB_GEOFENCE_ID - 1;

/// Longest fence name in characters (Unicode scalar values), 255 (C8 R4): the
/// import source's name column counts characters, so one limit means a Hub
/// name fits wherever an imported name fits.
pub const MAX_GEOFENCE_NAME_CHARS: usize = 255;

/// Longest stored fence name in bytes, 1,020 (C8 section 1.1; the store check
/// only): 255 characters at the longest UTF-8 encoding of 4 bytes.
pub const MAX_GEOFENCE_NAME_BYTES: usize = 4 * MAX_GEOFENCE_NAME_CHARS;

/// Decimal places a fence centre is stored at, 6 (C8 R5): the scale C1 R35
/// stores positions at.
pub const GEOFENCE_COORDINATE_DECIMALS: u32 = 6;

/// Decimal places of a fence cost per unit given in a Hub save, 4 (C8 R8).
pub const GEOFENCE_COST_DECIMALS: u32 = 4;

/// Largest cost per unit a Hub save may give, 99,999.9999 (C8 R8).
pub const MAX_GEOFENCE_COST_PER_UNIT: f64 = 99999.9999;

/// Decimal places of a fence session fee given in a Hub save, 2 (C8 R8).
pub const GEOFENCE_SESSION_FEE_DECIMALS: u32 = 2;

/// Largest session fee a Hub save may give, 999,999,999,999.99 (C8 R8).
pub const MAX_GEOFENCE_SESSION_FEE: f64 = 999999999999.99;

/// Two fence centres closer than this, 0.01 m, are the same point (C8 R9): far
/// below receiver error and the 0.111 m step of the coordinate grid, and it
/// catches representations that differ only in floating-point residue, such as
/// longitudes 180 and -180.
pub const SAME_PLACE_METRES: f64 = 0.01;

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
