// SPDX-License-Identifier: AGPL-3.0-only

//! Fence save validation and import activation vocabulary (C8 R1 to C8 R14).
//!
//! [`validate_geofence_save`] is the one pure function every Hub write of a
//! fence goes through: it reads no clock, storage or environment, checks the
//! fields in the order name, centre, radius, tariff and returns the first
//! failure. Its error text never carries an input value (C8 R11).

use thiserror::Error;

use super::location::{
    DEFAULT_GEOFENCE_RADIUS_METRES, GEOFENCE_COORDINATE_DECIMALS, GEOFENCE_COST_DECIMALS,
    GEOFENCE_SESSION_FEE_DECIMALS, MAX_GEOFENCE_COST_PER_UNIT, MAX_GEOFENCE_NAME_CHARS,
    MAX_GEOFENCE_RADIUS_METRES, MAX_GEOFENCE_SESSION_FEE, MIN_SAVED_GEOFENCE_RADIUS_METRES,
};
use crate::hub_pack::GeofenceBillingType;
use crate::lifecycle::round_to_precision;
use crate::teslamate_projection::TeslaMateGeofence;

/// The values a validated save stores (C8 section 1.2).
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedGeofence {
    /// Trimmed, 1 to 255 characters, none of the characters C8 R4 refuses.
    pub name: String,
    /// Six decimals, zero normalised.
    pub latitude: f64,
    /// Six decimals, zero normalised.
    pub longitude: f64,
    /// Whole metres when given; the stored value when kept (C8 R7).
    pub radius_m: f64,
    /// Resolved by C8 table 5.2.
    pub billing_type: Option<GeofenceBillingType>,
    /// Four decimals when given; the stored value when kept.
    pub cost_per_unit: Option<f64>,
    /// Two decimals when given; the stored value when kept.
    pub session_fee: Option<f64>,
}

/// Why a Hub save is refused (C8 R11). The display text carries no input
/// value, name or coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GeofenceRejection {
    #[error("geofence name is empty after trimming")]
    NameEmpty,
    #[error("geofence name is longer than 255 characters")]
    NameTooLong,
    #[error("geofence name contains a control or invisible formatting character")]
    NameControlCharacter,
    #[error("geofence latitude must be a finite number from -90 to 90 degrees")]
    LatitudeInvalid,
    #[error("geofence longitude must be a finite number from -180 to 180 degrees")]
    LongitudeInvalid,
    #[error("geofence radius must be a finite number of metres")]
    RadiusNotFinite,
    #[error("geofence radius must be a whole number of metres")]
    RadiusNotWholeMetres,
    #[error("geofence radius must be at least 10 metres")]
    RadiusBelowMinimum,
    #[error("geofence radius must be at most 30000 metres")]
    RadiusAboveMaximum,
    #[error("geofence billing type needs a cost per unit")]
    BillingTypeWithoutCost,
    #[error("geofence cost per unit must be a finite number from 0 to 99999.9999")]
    CostPerUnitInvalid,
    #[error("geofence session fee must be a finite number from 0 to 999999999999.99")]
    SessionFeeInvalid,
    #[error("geofence {existing_id} already has this centre and radius")]
    DuplicatePlace { existing_id: i64 },
    #[error("no geofence id is left for this vehicle")]
    IdsExhausted,
}

/// Why an imported fence row is not activated (C8 R13). The codes are stable:
/// import reports list rows by them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GeofenceInactiveReason {
    /// Id below 1 or above 2,147,483,647.
    IdOutOfRange,
    /// Empty name, or a name of White_Space characters only.
    NameEmpty,
    /// Latitude or longitude absent.
    CentreMissing,
    /// Centre not finite or outside the WGS84 ranges.
    CentreOutOfRange,
    /// Radius absent.
    RadiusMissing,
    /// Radius not finite, or above 32,767.
    RadiusOutOfRange,
    /// Radius finite and at most 0.
    RadiusNotPositive,
}

impl GeofenceInactiveReason {
    /// The stable report code of the reason.
    pub const fn code(self) -> &'static str {
        match self {
            Self::IdOutOfRange => "id_out_of_range",
            Self::NameEmpty => "name_empty",
            Self::CentreMissing => "centre_missing",
            Self::CentreOutOfRange => "centre_out_of_range",
            Self::RadiusMissing => "radius_missing",
            Self::RadiusOutOfRange => "radius_out_of_range",
            Self::RadiusNotPositive => "radius_not_positive",
        }
    }
}

/// Validate one fence save (C8 R1 to C8 R8, tables 5.1 and 5.2).
///
/// `existing` is the stored fence an edit targets, `None` for a create;
/// `clear_tariff` drops the stored billing type, cost and fee from the base
/// the save resolves against. Fields the save does not give keep the stored
/// value unchecked (C8 R7, C8 R8), or take the default radius on a create.
///
/// `existing` must be a stored row, which always carries a centre, a radius and
/// a tariff that may be empty; a record without a radius passed as `existing`
/// resolves an omitted radius to the create default, so a caller that builds
/// `existing` itself must copy every geometry field of the row it read.
pub(crate) fn validate_geofence_save(
    input: &TeslaMateGeofence,
    existing: Option<&TeslaMateGeofence>,
    clear_tariff: bool,
) -> Result<ValidatedGeofence, GeofenceRejection> {
    let name = validated_name(&input.name)?;
    let (latitude, longitude) = validated_centre(input.latitude, input.longitude)?;
    let radius_m = resolved_radius(input.radius_m, existing)?;
    let (billing_type, cost_per_unit, session_fee) =
        resolved_tariff(input, existing, clear_tariff)?;
    Ok(ValidatedGeofence {
        name,
        latitude,
        longitude,
        radius_m,
        billing_type,
        cost_per_unit,
        session_fee,
    })
}

/// C8 R4: trimmed, 1 to 255 characters, none of the refused characters.
/// Rejections are checked in the order empty, too long, control character.
fn validated_name(name: &str) -> Result<String, GeofenceRejection> {
    let trimmed = name.trim();
    let characters = trimmed.chars().count();
    if characters == 0 {
        return Err(GeofenceRejection::NameEmpty);
    }
    if characters > MAX_GEOFENCE_NAME_CHARS {
        return Err(GeofenceRejection::NameTooLong);
    }
    if trimmed.chars().any(is_refused_name_character) {
        return Err(GeofenceRejection::NameControlCharacter);
    }
    Ok(trimmed.to_owned())
}

/// Characters a fence name may not hold (C8 R4): general category Cc, line and
/// paragraph separators, the bidirectional embedding, override and isolate
/// controls, and the two invisible spaces. Other format characters stay
/// allowed because names need them to display correctly.
fn is_refused_name_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
                | '\u{200B}'
                | '\u{FEFF}'
        )
}

/// Round half away from zero to `decimals` places; a zero result is +0.0.
fn rounded(value: f64, decimals: u32) -> f64 {
    let value = round_to_precision(value, decimals);
    if value == 0.0 { 0.0 } else { value }
}

/// C8 R5: present, finite and in range on the value as given, then rounded to
/// six decimals with zero normalised.
fn validated_centre(
    latitude: Option<f64>,
    longitude: Option<f64>,
) -> Result<(f64, f64), GeofenceRejection> {
    let latitude = latitude
        .filter(|value| value.is_finite() && (-90.0..=90.0).contains(value))
        .ok_or(GeofenceRejection::LatitudeInvalid)?;
    let longitude = longitude
        .filter(|value| value.is_finite() && (-180.0..=180.0).contains(value))
        .ok_or(GeofenceRejection::LongitudeInvalid)?;
    Ok((
        rounded(latitude, GEOFENCE_COORDINATE_DECIMALS),
        rounded(longitude, GEOFENCE_COORDINATE_DECIMALS),
    ))
}

/// C8 R6 and C8 R7, table 5.2: a given radius is checked and stored as given; an
/// omitted one keeps the stored radius unchecked on an edit and is the default
/// on a create.
fn resolved_radius(
    given: Option<f64>,
    existing: Option<&TeslaMateGeofence>,
) -> Result<f64, GeofenceRejection> {
    let Some(radius) = given else {
        return Ok(existing
            .and_then(|stored| stored.radius_m)
            .unwrap_or(DEFAULT_GEOFENCE_RADIUS_METRES));
    };
    if !radius.is_finite() {
        return Err(GeofenceRejection::RadiusNotFinite);
    }
    if radius.fract() != 0.0 {
        return Err(GeofenceRejection::RadiusNotWholeMetres);
    }
    if radius < MIN_SAVED_GEOFENCE_RADIUS_METRES {
        return Err(GeofenceRejection::RadiusBelowMinimum);
    }
    if radius > MAX_GEOFENCE_RADIUS_METRES {
        return Err(GeofenceRejection::RadiusAboveMaximum);
    }
    Ok(radius)
}

/// A given price: finite, rounded, zero normalised, then within `0..=maximum`.
fn validated_price(
    value: f64,
    decimals: u32,
    maximum: f64,
    rejection: GeofenceRejection,
) -> Result<f64, GeofenceRejection> {
    if !value.is_finite() {
        return Err(rejection);
    }
    let value = rounded(value, decimals);
    if (0.0..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(rejection)
    }
}

type ResolvedTariff = (Option<GeofenceBillingType>, Option<f64>, Option<f64>);

/// C8 R8 and table 5.2. The base tariff is the stored one on an edit without
/// `clear_tariff`, nothing on a create or with it. Given values are checked in
/// the order cost, fee, billing type; kept values are not.
fn resolved_tariff(
    input: &TeslaMateGeofence,
    existing: Option<&TeslaMateGeofence>,
    clear_tariff: bool,
) -> Result<ResolvedTariff, GeofenceRejection> {
    let base = existing.filter(|_| !clear_tariff);
    let base_type = base.and_then(|stored| stored.billing_type);
    let cost_per_unit = match input.cost_per_unit {
        Some(value) => Some(validated_price(
            value,
            GEOFENCE_COST_DECIMALS,
            MAX_GEOFENCE_COST_PER_UNIT,
            GeofenceRejection::CostPerUnitInvalid,
        )?),
        None => base.and_then(|stored| stored.cost_per_unit),
    };
    let session_fee = match input.session_fee {
        Some(value) => Some(validated_price(
            value,
            GEOFENCE_SESSION_FEE_DECIMALS,
            MAX_GEOFENCE_SESSION_FEE,
            GeofenceRejection::SessionFeeInvalid,
        )?),
        None => base.and_then(|stored| stored.session_fee),
    };
    let billing_type = match input.billing_type {
        Some(given) if cost_per_unit.is_some() || Some(given) == base_type => Some(given),
        Some(_) => return Err(GeofenceRejection::BillingTypeWithoutCost),
        None if base_type.is_some() => base_type,
        None if input.cost_per_unit.is_some() => Some(GeofenceBillingType::PerKwh),
        None => None,
    };
    Ok((billing_type, cost_per_unit, session_fee))
}
