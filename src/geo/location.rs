// SPDX-License-Identifier: AGPL-3.0-only

//! Pure WGS84 geofence validation and matching.
//!
//! This module deliberately has no persistence, lifecycle, or network
//! concerns. Distances use the Earth radius used by PostgreSQL's
//! `earthdistance` extension and are returned in metres.

use std::fmt;

/// Earth radius used by PostgreSQL `earthdistance`, in metres.
pub const EARTH_RADIUS_METRES: f64 = 6_378_168.0;

/// TeslaMate-compatible upper bound. TeslaMate requires the radius to be
/// strictly less than five kilometres.
pub const MAX_GEOFENCE_RADIUS_METRES: f64 = 5_000.0;

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

#[derive(Debug, Clone, PartialEq)]
pub struct Geofence {
    pub id: i64,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub radius_metres: f64,
}

impl Geofence {
    pub fn new(
        id: i64,
        name: impl Into<String>,
        latitude: f64,
        longitude: f64,
        radius_metres: f64,
    ) -> Result<Self, LocationError> {
        if id <= 0 {
            return Err(LocationError::InvalidId);
        }
        let name = name.into();
        if name.trim().is_empty() {
            return Err(LocationError::EmptyName);
        }
        let centre = Wgs84Point::new(latitude, longitude)?;
        if !radius_metres.is_finite()
            || radius_metres <= 0.0
            || radius_metres >= MAX_GEOFENCE_RADIUS_METRES
        {
            return Err(LocationError::InvalidRadius);
        }
        Ok(Self {
            id,
            name: name.trim().to_owned(),
            latitude: centre.latitude,
            longitude: centre.longitude,
            radius_metres,
        })
    }

    pub fn centre(&self) -> Wgs84Point {
        Wgs84Point {
            latitude: self.latitude,
            longitude: self.longitude,
        }
    }

    pub fn distance_metres(&self, point: Wgs84Point) -> f64 {
        great_circle_distance_metres(self.centre(), point)
    }

    /// TeslaMate uses strict containment: a point exactly on the circle is
    /// outside the fence.
    pub fn contains(&self, point: Wgs84Point) -> bool {
        self.distance_metres(point) < self.radius_metres
    }
}

/// Return the containing fence nearest to the point's centre.
///
/// The stable positive ID resolves equal-distance ties. Input fences are
/// assumed to have been created through [`Geofence::new`].
pub fn nearest_containing<'a>(
    point: Wgs84Point,
    fences: impl IntoIterator<Item = &'a Geofence>,
) -> Option<&'a Geofence> {
    fences
        .into_iter()
        .filter_map(|fence| {
            let distance = fence.distance_metres(point);
            fence.contains(point).then_some((fence, distance))
        })
        .min_by(|(left, left_distance), (right, right_distance)| {
            left_distance
                .total_cmp(right_distance)
                .then_with(|| left.id.cmp(&right.id))
        })
        .map(|(fence, _)| fence)
}

fn great_circle_distance_metres(a: Wgs84Point, b: Wgs84Point) -> f64 {
    let latitude_delta = (b.latitude - a.latitude).to_radians();
    let longitude_delta = (b.longitude - a.longitude).to_radians();
    let a_latitude = (latitude_delta / 2.0).sin();
    let a_longitude = (longitude_delta / 2.0).sin();
    let cosine_product = a.latitude.to_radians().cos() * b.latitude.to_radians().cos();
    let haversine =
        (a_latitude * a_latitude + cosine_product * a_longitude * a_longitude).clamp(0.0, 1.0);
    EARTH_RADIUS_METRES * 2.0 * haversine.sqrt().atan2((1.0 - haversine).sqrt())
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

    fn point(latitude: f64, longitude: f64) -> Wgs84Point {
        Wgs84Point::new(latitude, longitude).unwrap()
    }

    fn fence(id: i64, latitude: f64, longitude: f64, radius_metres: f64) -> Geofence {
        Geofence::new(
            id,
            format!("fence-{id}"),
            latitude,
            longitude,
            radius_metres,
        )
        .unwrap()
    }

    #[test]
    fn exact_boundary_is_outside_and_just_inside_is_inside() {
        let probe = point(0.0, 0.01);
        let radius = great_circle_distance_metres(point(0.0, 0.0), probe);
        let boundary = fence(1, 0.0, 0.0, radius);
        assert!(!boundary.contains(probe));

        let inside = point(0.0, 0.01 - 0.000001);
        assert!(boundary.contains(inside));
    }

    #[test]
    fn antimeridian_distance_wraps_correctly() {
        let fence = fence(1, 0.0, 179.999, 500.0);
        assert!(fence.contains(point(0.0, -179.999)));
        assert!(fence.distance_metres(point(0.0, -179.999)) < 250.0);
    }

    #[test]
    fn polar_distance_is_valid() {
        let fence = fence(1, 90.0, 0.0, 200.0);
        assert!(fence.contains(point(89.999, 123.0)));
        assert!(!fence.contains(point(89.997, 123.0)));
    }

    #[test]
    fn overlapping_fences_choose_nearest_then_stable_id() {
        let fences = [fence(20, 0.0, 0.001, 400.0), fence(10, 0.0, -0.001, 400.0)];
        assert_eq!(nearest_containing(point(0.0, 0.0), &fences).unwrap().id, 10);

        let tied = [fence(20, 0.0, 0.001, 400.0), fence(10, 0.0, 0.001, 400.0)];
        assert_eq!(nearest_containing(point(0.0, 0.0), &tied).unwrap().id, 10);
    }

    #[test]
    fn invalid_points_and_fences_are_rejected() {
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
        assert_eq!(
            Geofence::new(0, "bad", 0.0, 0.0, 10.0),
            Err(LocationError::InvalidId)
        );
        assert_eq!(
            Geofence::new(1, "  ", 0.0, 0.0, 10.0),
            Err(LocationError::EmptyName)
        );
        assert_eq!(
            Geofence::new(1, "bad", 0.0, 0.0, f64::NAN),
            Err(LocationError::InvalidRadius)
        );
        assert_eq!(
            Geofence::new(1, "bad", 0.0, 0.0, 5_000.0),
            Err(LocationError::InvalidRadius)
        );
    }
}
