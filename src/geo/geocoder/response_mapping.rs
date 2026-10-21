// SPDX-License-Identifier: AGPL-3.0-only

//! Reverse-geocoder response to address mapping (C7 part B, R20-R50).
//!
//! The mapping is pure: it reads no clock, environment, storage or network and
//! logs nothing, so equal bytes give equal results. It works on a
//! `serde_json::Value`, never on a derived wire struct, and returns only the
//! three payload-free errors of R50.
//!
//! Only general category Cc is refused by storage, so format characters
//! (U+200D, U+200E, U+200F, U+FEFF) are accepted and kept; [`storage_refuses`]
//! is the one place that states the predicate (see [`clean_text`]).

use super::{GeocodedAddress, GeocoderError};
use serde_json::{Map, Value};

/// K-TEXT: storage limit for every optional text column and `name`.
const TEXT_MAX_BYTES: usize = 256;
/// K-DISPLAY: storage limit for `display_name`.
const DISPLAY_MAX_BYTES: usize = 256;
/// K-LICENCE: storage limit for the licence text.
const LICENCE_MAX_BYTES: usize = crate::db::MAX_ADDRESS_LICENCE_BYTES;
/// K-RAW: the smaller of the kept raw-body constant and storage's 64 KiB.
pub(super) const RAW_MAX_BYTES: usize = if super::MAX_RAW_JSON_BYTES < 65_536 {
    super::MAX_RAW_JSON_BYTES
} else {
    65_536
};
/// K-TYPE: longest `osm_type`.
const TYPE_MAX_BYTES: usize = 32;
/// Longest decimal text of an `i64`.
const ID_MAX_DIGITS: usize = 19;
/// K-LAT and K-LON, inclusive.
const LATITUDE_LIMIT: f64 = 90.0;
const LONGITUDE_LIMIT: f64 = 180.0;
/// K-CITYSTATE: sovereign city-states, compared ASCII-case-insensitively.
const CITY_STATE_CODES: [&str; 3] = ["sg", "mc", "va"];
const DISPLAY_SEPARATOR: &str = ", ";

// Section 3.3 key lists, in the specified order of preference.
const HOUSE_NUMBER_KEYS: [&str; 1] = ["house_number"];
const ROAD_KEYS: [&str; 1] = ["road"];
const NEIGHBOURHOOD_KEYS: [&str; 6] = [
    "neighbourhood",
    "allotments",
    "quarter",
    "suburb",
    "city_district",
    "borough",
];
const CITY_KEYS: [&str; 5] = ["city", "town", "village", "municipality", "hamlet"];
const COUNTY_KEYS: [&str; 1] = ["county"];
const POSTCODE_KEYS: [&str; 1] = ["postcode"];
const STATE_KEYS: [&str; 2] = ["state", "region"];
const STATE_DISTRICT_KEYS: [&str; 1] = ["state_district"];
const COUNTRY_KEYS: [&str; 1] = ["country"];
const HOUSE_NAME_KEYS: [&str; 1] = ["house_name"];
/// `addresstype` labels that are codes, not names (R43).
const CODE_LABELS: [&str; 3] = ["house_number", "postcode", "country_code"];
const ISO_LABEL_PREFIX: &str = "ISO3166-2";

/// Map one reverse-geocoding body to an address, checks in the order of R49.
pub(crate) fn parse_response(body: &[u8]) -> Result<GeocodedAddress, GeocoderError> {
    let document: Value = serde_json::from_slice(body).map_err(|_| GeocoderError::ResponseRead)?;
    let Value::Object(top) = &document else {
        return Err(GeocoderError::ResponseRead);
    };
    if top.get("error").is_some_and(|member| !member.is_null()) {
        return Err(GeocoderError::NoResult);
    }

    let (osm_type, osm_id) = read_identity(top)?;

    let address = top.get("address").and_then(Value::as_object);
    let house_number = first_text(address, &HOUSE_NUMBER_KEYS);
    let road = first_text(address, &ROAD_KEYS);
    let neighbourhood = first_text(address, &NEIGHBOURHOOD_KEYS);
    let city = read_city(address);
    let county = first_text(address, &COUNTY_KEYS);
    let postcode = first_text(address, &POSTCODE_KEYS);
    let state = first_text(address, &STATE_KEYS);
    let state_district = first_text(address, &STATE_DISTRICT_KEYS);
    let country = first_text(address, &COUNTRY_KEYS);
    let name = read_name(top, address);

    let display_name = bounded_display_name(top.get("display_name"))
        .or_else(|| {
            compose_display_name(&[
                name.as_deref(),
                house_number.as_deref(),
                road.as_deref(),
                neighbourhood.as_deref(),
                city.as_deref(),
                county.as_deref(),
                state_district.as_deref(),
                state.as_deref(),
                postcode.as_deref(),
                country.as_deref(),
            ])
        })
        .ok_or(GeocoderError::MalformedResponse)?;

    Ok(GeocodedAddress {
        osm_type,
        osm_id,
        display_name,
        name,
        latitude: read_coordinate(top.get("lat"), LATITUDE_LIMIT),
        longitude: read_coordinate(top.get("lon"), LONGITUDE_LIMIT),
        house_number,
        road,
        neighbourhood,
        city,
        county,
        postcode,
        state,
        state_district,
        country,
        raw_json: retained_body(body),
        licence: read_licence(top.get("licence")),
    })
}

/// The one statement of storage's text predicate (R27): what it refuses.
fn storage_refuses(character: char) -> bool {
    character.is_control()
}

/// R27. Control characters become spaces, any other character storage refuses
/// is deleted, whitespace runs collapse to one space and both ends are
/// trimmed. `None` when nothing is left.
///
/// Retained storage predicate (kept code, `storage/db/projection_helpers.rs`,
/// `validate_address_cache_record` and `invalid_address_text`; limits from
/// `storage/db.rs` (256-byte text and display name, 64 KiB raw body) and
/// `storage/db/access_models.rs` (512-byte licence)), re-read for this unit and
/// equal to the record in R27:
/// - `osm_type`: 1 to 32 bytes, no `char::is_control` character;
/// - `osm_id`: greater than zero;
/// - `display_name`: not blank after `trim`, at most 256 bytes; storage does
///   not test it for control characters, so only this module's cleaning keeps
///   them out of it (the kept `complete_address_enrichment` refuses them in the
///   completed address);
/// - every optional text column and `name`: at most 256 bytes, no
///   `char::is_control` character (a longer value is refused as a whole);
/// - coordinates: finite and within WGS84 range;
/// - `raw_json`: at most 64 KiB and valid JSON;
/// - `licence`: not empty, at most 512 bytes, no `char::is_control` character.
fn clean_text(text: &str) -> Option<String> {
    let spaced = text.chars().map(|character| {
        if character.is_control() {
            ' '
        } else {
            character
        }
    });
    let kept: String = spaced
        .filter(|character| character.is_whitespace() || !storage_refuses(*character))
        .collect();
    let words: Vec<&str> = kept.split_whitespace().collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// R28: the cleaned text, cut to the longest prefix of at most `max_bytes`
/// that ends on a character boundary, with trailing whitespace removed.
fn bounded_text(text: &str, max_bytes: usize) -> Option<String> {
    let cleaned = clean_text(text)?;
    if cleaned.len() <= max_bytes {
        return Some(cleaned);
    }
    let mut end = max_bytes;
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = cleaned[..end].trim_end();
    (!prefix.is_empty()).then(|| prefix.to_owned())
}

/// R33: the bounded value of the first key whose value is a non-blank text.
fn first_text(address: Option<&Map<String, Value>>, keys: &[&str]) -> Option<String> {
    let address = address?;
    keys.iter().find_map(|key| {
        address
            .get(*key)
            .and_then(Value::as_str)
            .and_then(|text| bounded_text(text, TEXT_MAX_BYTES))
    })
}

/// R38 and R39: settlement keys, then the country name of a city-state.
fn read_city(address: Option<&Map<String, Value>>) -> Option<String> {
    first_text(address, &CITY_KEYS).or_else(|| {
        let members = address?;
        let code = members.get("country_code")?.as_str()?;
        CITY_STATE_CODES
            .iter()
            .any(|state| code.eq_ignore_ascii_case(state))
            .then(|| first_text(address, &COUNTRY_KEYS))
            .flatten()
    })
}

/// R43: the object's own name, else the address entry under its type label,
/// else the house name.
fn read_name(top: &Map<String, Value>, address: Option<&Map<String, Value>>) -> Option<String> {
    if let Some(own) = top
        .get("name")
        .and_then(Value::as_str)
        .and_then(|text| bounded_text(text, TEXT_MAX_BYTES))
    {
        return Some(own);
    }
    if let Some(members) = address
        && let Some(label) = top.get("addresstype").and_then(Value::as_str)
        && clean_text(label).is_some()
        && !is_code_label(label)
        && let Some(listed) = members
            .get(label)
            .and_then(Value::as_str)
            .and_then(|text| bounded_text(text, TEXT_MAX_BYTES))
    {
        return Some(listed);
    }
    first_text(address, &HOUSE_NAME_KEYS)
}

fn is_code_label(label: &str) -> bool {
    CODE_LABELS.contains(&label) || label.starts_with(ISO_LABEL_PREFIX)
}

/// R24 to R26: both identity members are required and valid.
fn read_identity(top: &Map<String, Value>) -> Result<(String, i64), GeocoderError> {
    let kind = read_object_kind(top.get("osm_type"));
    let id = read_object_id(top.get("osm_id"));
    match (kind, id) {
        (Some(kind), Some(id)) => Ok((kind, id)),
        _ => Err(GeocoderError::MalformedResponse),
    }
}

/// R24: trimmed, lower-cased, single letters spelled out, printable ASCII.
fn read_object_kind(value: Option<&Value>) -> Option<String> {
    let trimmed = value?.as_str()?.trim_ascii();
    if trimmed.is_empty() {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    let spelled = match lowered.as_str() {
        "n" => "node",
        "w" => "way",
        "r" => "relation",
        other => other,
    };
    let printable = spelled.bytes().all(|byte| (0x21..=0x7e).contains(&byte));
    (spelled.len() <= TYPE_MAX_BYTES && printable).then(|| spelled.to_owned())
}

/// R25: a JSON integer, or digits only, in 1 to `i64::MAX`.
fn read_object_id(value: Option<&Value>) -> Option<i64> {
    let id = match value? {
        Value::Number(number) => number.as_i64()?,
        Value::String(text) => {
            let digits = !text.is_empty()
                && text.len() <= ID_MAX_DIGITS
                && text.bytes().all(|byte| byte.is_ascii_digit());
            if !digits {
                return None;
            }
            text.parse::<i64>().ok()?
        }
        _ => return None,
    };
    (id >= 1).then_some(id)
}

/// R44: a number or a text of one, finite and in range; otherwise `None`.
fn read_coordinate(value: Option<&Value>, limit: f64) -> Option<f64> {
    let parsed = match value? {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim_ascii().parse::<f64>().ok()?,
        _ => return None,
    };
    (parsed.is_finite() && (-limit..=limit).contains(&parsed)).then_some(parsed)
}

/// R45: the licence text unchanged when it is non-blank, within its bound and
/// free of characters storage refuses; otherwise `None`.
fn read_licence(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let usable = clean_text(text).is_some()
        && text.len() <= LICENCE_MAX_BYTES
        && !text.chars().any(storage_refuses);
    usable.then(|| text.to_owned())
}

/// R46: the whole body as text when it is within the cap, else nothing.
fn retained_body(body: &[u8]) -> Option<String> {
    if body.len() > RAW_MAX_BYTES {
        return None;
    }
    std::str::from_utf8(body).ok().map(str::to_owned)
}

/// R29: the provider's display text, shortened by whole pieces when needed.
fn bounded_display_name(value: Option<&Value>) -> Option<String> {
    let cleaned = clean_text(value?.as_str()?)?;
    fit_display_name(&cleaned)
}

/// R30: the display text composed from the mapped values, in the order given,
/// skipping missing values and repeats.
fn compose_display_name(parts: &[Option<&str>]) -> Option<String> {
    let mut taken: Vec<&str> = Vec::new();
    for part in parts.iter().flatten() {
        if !taken.contains(part) {
            taken.push(part);
        }
    }
    if taken.is_empty() {
        return None;
    }
    fit_display_name(&taken.join(DISPLAY_SEPARATOR))
}

/// The length rule of R29 for text that is already clean.
fn fit_display_name(text: &str) -> Option<String> {
    if text.len() <= DISPLAY_MAX_BYTES {
        return Some(text.to_owned());
    }
    let mut pieces = text
        .split(',')
        .map(str::trim)
        .filter(|piece| !piece.is_empty());
    let first = pieces.next()?;
    if first.len() > DISPLAY_MAX_BYTES {
        return bounded_text(first, DISPLAY_MAX_BYTES);
    }
    let mut kept = first.to_owned();
    for piece in pieces {
        if kept.len() + DISPLAY_SEPARATOR.len() + piece.len() > DISPLAY_MAX_BYTES {
            break;
        }
        kept.push_str(DISPLAY_SEPARATOR);
        kept.push_str(piece);
    }
    Some(kept)
}
