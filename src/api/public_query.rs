// SPDX-License-Identifier: AGPL-3.0-only

use serde::Serialize;
use uuid::Uuid;

use crate::{
    db::PositionTerrainLookup,
    hub_pack::{
        GeofenceBillingType, ProjectionCharge, ProjectionChargeSample, ProjectionDrive,
        ProjectionPosition,
    },
    protocol::CursorKey,
};

pub(crate) const DEFAULT_DRIVE_PAGE_LIMIT: u32 = 100;
pub(crate) const MAX_DRIVE_PAGE_LIMIT: u32 = 500;
pub(crate) const DEFAULT_CHARGE_PAGE_LIMIT: u32 = 100;
pub(crate) const MAX_CHARGE_PAGE_LIMIT: u32 = 500;
pub(crate) const DEFAULT_POSITION_PAGE_LIMIT: u32 = 1000;
pub(crate) const MAX_POSITION_PAGE_LIMIT: u32 = 2048;
pub(crate) const DEFAULT_CHARGE_SAMPLE_PAGE_LIMIT: u32 = 1000;
pub(crate) const MAX_CHARGE_SAMPLE_PAGE_LIMIT: u32 = 2048;

/// One serialised JSON page, including its envelope, never exceeds this size.
pub(crate) const MAX_PUBLIC_PAGE_BYTES: usize = 1024 * 1024;

/// Page cursor `tqp1` of hub-http-v1@1.2.0 (`cursor.schema.json`): version,
/// kind, vehicle UUID, scope id, from_ms, to_ms, last key A and last key B.
const PAGE_CURSOR_PREFIX: &str = "tqp1";
const PAGE_CURSOR_VERSION: u8 = 1;
const PAGE_CURSOR_PAYLOAD_BYTES: usize = 58;
const PAGE_CURSOR_TAG_BYTES: usize = 32;

/// Legacy drives-only cursor `tqd1`: accepted for one release, never issued.
const DRIVE_CURSOR_PREFIX: &str = "tqd1";
const DRIVE_CURSOR_PAYLOAD_BYTES: usize = 49;
const DRIVE_CURSOR_TAG_BYTES: usize = 32;

/// The four page routes. The discriminant is the cursor kind byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageKind {
    Drives,
    Charges,
    Positions,
    ChargeSamples,
}

impl PageKind {
    const fn cursor_code(self) -> u8 {
        match self {
            Self::Drives => 1,
            Self::Charges => 2,
            Self::Positions => 3,
            Self::ChargeSamples => 4,
        }
    }

    pub(crate) const fn default_limit(self) -> u32 {
        match self {
            Self::Drives => DEFAULT_DRIVE_PAGE_LIMIT,
            Self::Charges => DEFAULT_CHARGE_PAGE_LIMIT,
            Self::Positions => DEFAULT_POSITION_PAGE_LIMIT,
            Self::ChargeSamples => DEFAULT_CHARGE_SAMPLE_PAGE_LIMIT,
        }
    }

    pub(crate) const fn max_limit(self) -> u32 {
        match self {
            Self::Drives => MAX_DRIVE_PAGE_LIMIT,
            Self::Charges => MAX_CHARGE_PAGE_LIMIT,
            Self::Positions => MAX_POSITION_PAGE_LIMIT,
            Self::ChargeSamples => MAX_CHARGE_SAMPLE_PAGE_LIMIT,
        }
    }
}

/// Query string shared by every page route. A malformed numeric value (not a
/// decimal integer in range) fails deserialisation and is `invalid_query`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageQueryParameters {
    pub(crate) from_ms: Option<i64>,
    pub(crate) to_ms: Option<i64>,
    pub(crate) limit: Option<u32>,
    pub(crate) cursor: Option<String>,
}

/// A validated page request. `after` is the (key A, key B) of the last item of
/// the previous page; the keys are `(start_date_ms, id)` for drives and
/// charges, `(date_ms, id)` for positions and `(timestamp_ms, id)` for samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageQuery {
    pub(crate) kind: PageKind,
    pub(crate) vehicle_id: Uuid,
    /// The drive or charge id of a nested page, otherwise 0.
    pub(crate) scope_id: i64,
    pub(crate) from_ms: i64,
    pub(crate) to_ms: i64,
    pub(crate) limit: u32,
    pub(crate) after: Option<(i64, i64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageQueryError {
    TimeRange,
    Limit,
    Cursor,
}

/// The ordering key of a page item, used to cut a cursor after it.
pub(crate) trait PageItem {
    fn page_key(&self) -> (i64, i64);
}

/// Source-neutral public shape for one materialised drive. Internal source
/// identifiers and projection-maintenance timestamps are deliberately absent.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PublicDrive {
    pub(crate) id: i64,
    pub(crate) vehicle_id: Uuid,
    pub(crate) start_date_ms: i64,
    pub(crate) end_date_ms: i64,
    pub(crate) distance_km: Option<f64>,
    pub(crate) duration_min: Option<i64>,
    pub(crate) efficiency: Option<f64>,
    pub(crate) outside_temp_avg: Option<f64>,
    pub(crate) inside_temp_avg: Option<f64>,
    pub(crate) speed_max: Option<i64>,
    pub(crate) power_max: Option<f64>,
    pub(crate) power_min: Option<f64>,
    pub(crate) start_ideal_range_km: Option<f64>,
    pub(crate) end_ideal_range_km: Option<f64>,
    pub(crate) start_address: Option<String>,
    pub(crate) end_address: Option<String>,
    pub(crate) start_geofence: Option<String>,
    pub(crate) end_geofence: Option<String>,
    pub(crate) start_latitude: Option<f64>,
    pub(crate) start_longitude: Option<f64>,
    pub(crate) end_latitude: Option<f64>,
    pub(crate) end_longitude: Option<f64>,
    pub(crate) start_soc: Option<i64>,
    pub(crate) end_soc: Option<i64>,
    pub(crate) start_rated_range_km: Option<f64>,
    pub(crate) end_rated_range_km: Option<f64>,
    pub(crate) ascent: Option<i64>,
    pub(crate) descent: Option<i64>,
}

impl PublicDrive {
    pub(crate) fn from_projection(vehicle_id: Uuid, drive: ProjectionDrive) -> Self {
        Self {
            id: drive.id,
            vehicle_id,
            start_date_ms: drive.start_date_ms,
            end_date_ms: drive.end_date_ms,
            distance_km: drive.distance_km,
            duration_min: drive.duration_min,
            efficiency: drive.efficiency,
            outside_temp_avg: drive.outside_temp_avg,
            inside_temp_avg: drive.inside_temp_avg,
            speed_max: drive.speed_max,
            power_max: drive.power_max,
            power_min: drive.power_min,
            start_ideal_range_km: drive.start_ideal_range_km,
            end_ideal_range_km: drive.end_ideal_range_km,
            start_address: drive.start_address,
            end_address: drive.end_address,
            start_geofence: drive.start_geofence,
            end_geofence: drive.end_geofence,
            start_latitude: drive.start_latitude,
            start_longitude: drive.start_longitude,
            end_latitude: drive.end_latitude,
            end_longitude: drive.end_longitude,
            start_soc: drive.start_soc,
            end_soc: drive.end_soc,
            start_rated_range_km: drive.start_rated_range_km,
            end_rated_range_km: drive.end_rated_range_km,
            ascent: drive.ascent,
            descent: drive.descent,
        }
    }
}

impl PageItem for PublicDrive {
    fn page_key(&self) -> (i64, i64) {
        (self.start_date_ms, self.id)
    }
}

/// `elevation_provenance` of hub-http-v1@1.2.0. The Hub only holds a truthful
/// record for terrain-derived elevation, so `source` is always `terrain`.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PublicElevationProvenance {
    pub(crate) source: &'static str,
    pub(crate) dataset: Option<String>,
    pub(crate) dataset_version: Option<String>,
    pub(crate) tile: Option<String>,
}

/// One drive position as hub-http-v1@1.2.0 `position`. `power` is a number
/// (f64 projection), not an integer.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PublicPosition {
    pub(crate) id: i64,
    pub(crate) drive_id: i64,
    pub(crate) date_ms: i64,
    pub(crate) latitude: f64,
    pub(crate) longitude: f64,
    pub(crate) speed: Option<i64>,
    pub(crate) power: Option<f64>,
    pub(crate) battery_level: Option<i64>,
    pub(crate) usable_battery_level: Option<i64>,
    pub(crate) elevation: Option<i64>,
    pub(crate) odometer: Option<f64>,
    pub(crate) ideal_battery_range_km: Option<f64>,
    pub(crate) est_battery_range_km: Option<f64>,
    pub(crate) rated_battery_range_km: Option<f64>,
    pub(crate) inside_temp: Option<f64>,
    pub(crate) outside_temp: Option<f64>,
    pub(crate) is_climate_on: Option<bool>,
    pub(crate) fan_status: Option<i64>,
    pub(crate) driver_temp_setting: Option<f64>,
    pub(crate) passenger_temp_setting: Option<f64>,
    pub(crate) battery_heater: Option<bool>,
    pub(crate) battery_heater_on: Option<bool>,
    pub(crate) tpms_pressure_fl: Option<f64>,
    pub(crate) tpms_pressure_fr: Option<f64>,
    pub(crate) tpms_pressure_rl: Option<f64>,
    pub(crate) tpms_pressure_rr: Option<f64>,
    pub(crate) elevation_provenance: Option<PublicElevationProvenance>,
}

impl PublicPosition {
    /// `drive_id` is the drive the page belongs to (the store has already
    /// checked that the stored row and its JSON agree). Terrain provenance is
    /// claimed only when the terrain lookup succeeded and an elevation is
    /// actually served; there is no per-row record for any other source.
    pub(crate) fn from_projection(
        drive_id: i64,
        position: ProjectionPosition,
        terrain: Option<PositionTerrainLookup>,
    ) -> Self {
        let elevation_provenance =
            terrain
                .filter(|_| position.elevation.is_some())
                .map(|terrain| PublicElevationProvenance {
                    source: "terrain",
                    dataset: terrain.dataset,
                    dataset_version: terrain.dataset_version,
                    tile: terrain.tile,
                });
        Self {
            id: position.id,
            drive_id,
            date_ms: position.date_ms,
            latitude: position.latitude,
            longitude: position.longitude,
            speed: position.speed,
            power: position.power,
            battery_level: position.battery_level,
            usable_battery_level: position.usable_battery_level,
            elevation: position.elevation,
            odometer: position.odometer,
            ideal_battery_range_km: position.ideal_battery_range_km,
            est_battery_range_km: position.est_battery_range_km,
            rated_battery_range_km: position.rated_battery_range_km,
            inside_temp: position.inside_temp,
            outside_temp: position.outside_temp,
            is_climate_on: position.is_climate_on,
            fan_status: position.fan_status,
            driver_temp_setting: position.driver_temp_setting,
            passenger_temp_setting: position.passenger_temp_setting,
            battery_heater: position.battery_heater,
            battery_heater_on: position.battery_heater_on,
            tpms_pressure_fl: position.tpms_pressure_fl,
            tpms_pressure_fr: position.tpms_pressure_fr,
            tpms_pressure_rl: position.tpms_pressure_rl,
            tpms_pressure_rr: position.tpms_pressure_rr,
            elevation_provenance,
        }
    }
}

impl PageItem for PublicPosition {
    fn page_key(&self) -> (i64, i64) {
        (self.date_ms, self.id)
    }
}

/// One closed charge session as hub-http-v1@1.2.0 `charge`. The address
/// provenance member is not emitted: the Hub keeps no truthful record yet.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PublicCharge {
    pub(crate) id: i64,
    pub(crate) vehicle_id: Uuid,
    pub(crate) start_date_ms: i64,
    pub(crate) end_date_ms: i64,
    pub(crate) duration_min: Option<i64>,
    pub(crate) charge_energy_added: Option<f64>,
    pub(crate) charge_energy_used_kwh: Option<f64>,
    pub(crate) start_battery_level: Option<i64>,
    pub(crate) end_battery_level: Option<i64>,
    pub(crate) start_ideal_range_km: Option<f64>,
    pub(crate) end_ideal_range_km: Option<f64>,
    pub(crate) start_rated_range_km: Option<f64>,
    pub(crate) end_rated_range_km: Option<f64>,
    pub(crate) start_latitude: Option<f64>,
    pub(crate) start_longitude: Option<f64>,
    pub(crate) address: Option<String>,
    pub(crate) location_name: Option<String>,
    pub(crate) geofence: Option<String>,
    pub(crate) fast_charger_type: Option<String>,
    pub(crate) is_dc: Option<bool>,
    pub(crate) charge_rate_km_per_hour: Option<f64>,
    pub(crate) max_charger_power_kw: Option<f64>,
    pub(crate) outside_temp_avg: Option<f64>,
    pub(crate) cost: Option<f64>,
    pub(crate) billing_type: Option<GeofenceBillingType>,
    pub(crate) cost_per_unit: Option<f64>,
    pub(crate) session_fee: Option<f64>,
}

impl PublicCharge {
    /// `None` for a session without an end date: those are never served.
    pub(crate) fn from_projection(vehicle_id: Uuid, charge: ProjectionCharge) -> Option<Self> {
        Some(Self {
            id: charge.id,
            vehicle_id,
            start_date_ms: charge.start_date_ms,
            end_date_ms: charge.end_date_ms?,
            duration_min: charge.duration_min,
            charge_energy_added: charge.charge_energy_added,
            charge_energy_used_kwh: charge.charge_energy_used_kwh,
            start_battery_level: charge.start_battery_level,
            end_battery_level: charge.end_battery_level,
            start_ideal_range_km: charge.start_ideal_range_km,
            end_ideal_range_km: charge.end_ideal_range_km,
            start_rated_range_km: charge.start_rated_range_km,
            end_rated_range_km: charge.end_rated_range_km,
            start_latitude: charge.start_latitude,
            start_longitude: charge.start_longitude,
            address: charge.address,
            location_name: charge.location_name,
            geofence: charge.geofence,
            fast_charger_type: charge.fast_charger_type,
            is_dc: charge.is_dc,
            charge_rate_km_per_hour: charge.charge_rate_km_per_hour,
            max_charger_power_kw: charge.max_charger_power_kw,
            outside_temp_avg: charge.outside_temp_avg,
            cost: charge.cost,
            billing_type: charge.billing_type,
            cost_per_unit: charge.cost_per_unit,
            session_fee: charge.session_fee,
        })
    }
}

impl PageItem for PublicCharge {
    fn page_key(&self) -> (i64, i64) {
        (self.start_date_ms, self.id)
    }
}

/// One charge sample as hub-http-v1@1.2.0 `charge_sample`. The charger power,
/// voltage and current fields are numbers (f64 projections), not integers.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PublicChargeSample {
    pub(crate) id: i64,
    pub(crate) charge_id: i64,
    pub(crate) timestamp_ms: i64,
    pub(crate) battery_level: Option<i64>,
    pub(crate) usable_battery_level: Option<i64>,
    pub(crate) charge_energy_added_kwh: Option<f64>,
    pub(crate) charger_power_kw: Option<f64>,
    pub(crate) charger_voltage: Option<f64>,
    pub(crate) charger_actual_current: Option<f64>,
    pub(crate) charger_pilot_current: Option<f64>,
    pub(crate) charger_phases: Option<i64>,
    pub(crate) ideal_range_km: Option<f64>,
    pub(crate) rated_range_km: Option<f64>,
    pub(crate) outside_temp_c: Option<f64>,
    pub(crate) battery_heater_on: Option<bool>,
    pub(crate) fast_charger_present: Option<bool>,
    pub(crate) fast_charger_brand: Option<String>,
    pub(crate) fast_charger_type: Option<String>,
    pub(crate) charge_cable: Option<String>,
}

impl PublicChargeSample {
    pub(crate) fn from_projection(sample: ProjectionChargeSample) -> Self {
        Self {
            id: sample.id,
            charge_id: sample.charge_process_id,
            timestamp_ms: sample.timestamp_ms,
            battery_level: sample.battery_level,
            usable_battery_level: sample.usable_battery_level,
            charge_energy_added_kwh: sample.charge_energy_added_kwh,
            charger_power_kw: sample.charger_power_kw,
            charger_voltage: sample.charger_voltage,
            charger_actual_current: sample.charger_actual_current,
            charger_pilot_current: sample.charger_pilot_current,
            charger_phases: sample.charger_phases,
            ideal_range_km: sample.ideal_range_km,
            rated_range_km: sample.rated_range_km,
            outside_temp_c: sample.outside_temp_c,
            battery_heater_on: sample.battery_heater_on,
            fast_charger_present: sample.fast_charger_present,
            fast_charger_brand: sample.fast_charger_brand,
            fast_charger_type: sample.fast_charger_type,
            charge_cable: sample.charge_cable,
        }
    }
}

impl PageItem for PublicChargeSample {
    fn page_key(&self) -> (i64, i64) {
        (self.timestamp_ms, self.id)
    }
}

/// The JSON envelope of every page route.
#[derive(Debug, Serialize)]
pub(crate) struct PublicPage<T> {
    pub(crate) items: Vec<T>,
    pub(crate) next_cursor: Option<String>,
}

/// Shared page assembly. Sizes the exact serialised items and envelope once
/// per candidate, stops before the item that would push the body over
/// `MAX_PUBLIC_PAGE_BYTES` without ever splitting an item, and cuts the cursor
/// after the last included item (also when the byte cap shortens a page whose
/// rows were all fetched). `Ok(None)` means the first item alone is over the
/// cap, which the caller answers with 503.
pub(crate) fn bounded_public_page<T: Serialize + PageItem>(
    mut items: Vec<T>,
    fetched_has_more: bool,
    next_cursor: impl Fn((i64, i64)) -> String,
) -> Result<Option<PublicPage<T>>, serde_json::Error> {
    let mut row_bytes = 0;
    let mut retained = 0;
    let mut cursor_after_retained = None;
    for (index, item) in items.iter().enumerate() {
        let candidate_bytes = row_bytes + usize::from(index > 0) + serde_json::to_vec(item)?.len();
        let cursor =
            (fetched_has_more || index + 1 < items.len()).then(|| next_cursor(item.page_key()));
        let envelope_bytes = serde_json::to_vec(&PublicPage::<T> {
            items: Vec::new(),
            next_cursor: cursor.clone(),
        })?
        .len();
        if candidate_bytes + envelope_bytes > MAX_PUBLIC_PAGE_BYTES {
            break;
        }
        row_bytes = candidate_bytes;
        retained = index + 1;
        cursor_after_retained = cursor;
    }
    if retained == 0 && !items.is_empty() {
        return Ok(None);
    }
    items.truncate(retained);
    Ok(Some(PublicPage {
        items,
        next_cursor: cursor_after_retained,
    }))
}

impl PageQuery {
    /// Parse the validated query for one page. The caller has already
    /// authenticated the device and checked the vehicle and any parent id.
    pub(crate) fn parse(
        cursor_key: &CursorKey,
        kind: PageKind,
        vehicle_id: Uuid,
        scope_id: i64,
        parameters: PageQueryParameters,
    ) -> Result<Self, PageQueryError> {
        let from_ms = parameters.from_ms.unwrap_or(0);
        let to_ms = parameters.to_ms.unwrap_or(i64::MAX);
        if from_ms < 0 || to_ms < 0 || from_ms >= to_ms {
            return Err(PageQueryError::TimeRange);
        }
        let limit = parameters.limit.unwrap_or(kind.default_limit());
        if limit == 0 || limit > kind.max_limit() {
            return Err(PageQueryError::Limit);
        }
        let mut query = Self {
            kind,
            vehicle_id,
            scope_id,
            from_ms,
            to_ms,
            limit,
            after: None,
        };
        query.after = parameters
            .cursor
            .as_deref()
            .map(|cursor| query.decode_cursor(cursor_key, cursor))
            .transpose()?;
        Ok(query)
    }

    pub(crate) fn next_cursor(self, cursor_key: &CursorKey, last: (i64, i64)) -> String {
        let mut payload = [0_u8; PAGE_CURSOR_PAYLOAD_BYTES];
        payload[0] = PAGE_CURSOR_VERSION;
        payload[1] = self.kind.cursor_code();
        payload[2..18].copy_from_slice(self.vehicle_id.as_bytes());
        payload[18..26].copy_from_slice(&self.scope_id.to_be_bytes());
        payload[26..34].copy_from_slice(&self.from_ms.to_be_bytes());
        payload[34..42].copy_from_slice(&self.to_ms.to_be_bytes());
        payload[42..50].copy_from_slice(&last.0.to_be_bytes());
        payload[50..58].copy_from_slice(&last.1.to_be_bytes());
        let tag = cursor_key.public_query_cursor_tag(&payload);
        format!(
            "{PAGE_CURSOR_PREFIX}.{}.{}",
            hex::encode(payload),
            hex::encode(tag)
        )
    }

    fn decode_cursor(
        &self,
        cursor_key: &CursorKey,
        cursor: &str,
    ) -> Result<(i64, i64), PageQueryError> {
        if cursor.starts_with(DRIVE_CURSOR_PREFIX) && self.kind == PageKind::Drives {
            return self.decode_legacy_drive_cursor(cursor_key, cursor);
        }
        let (payload, tag) = split_cursor::<PAGE_CURSOR_PAYLOAD_BYTES, PAGE_CURSOR_TAG_BYTES>(
            cursor,
            PAGE_CURSOR_PREFIX,
        )
        .ok_or(PageQueryError::Cursor)?;
        if !cursor_key.verifies_public_query_cursor_tag(&payload, &tag)
            || payload[0] != PAGE_CURSOR_VERSION
        {
            return Err(PageQueryError::Cursor);
        }
        let vehicle_id = Uuid::from_slice(&payload[2..18]).map_err(|_| PageQueryError::Cursor)?;
        let scope_id = i64_at(&payload, 18)?;
        let from_ms = i64_at(&payload, 26)?;
        let to_ms = i64_at(&payload, 34)?;
        let last_key_a = i64_at(&payload, 42)?;
        let last_key_b = i64_at(&payload, 50)?;
        if payload[1] != self.kind.cursor_code()
            || vehicle_id != self.vehicle_id
            || scope_id != self.scope_id
            || from_ms != self.from_ms
            || to_ms != self.to_ms
            || last_key_a < from_ms
            || last_key_a >= to_ms
            || last_key_b <= 0
        {
            return Err(PageQueryError::Cursor);
        }
        Ok((last_key_a, last_key_b))
    }

    fn decode_legacy_drive_cursor(
        &self,
        cursor_key: &CursorKey,
        cursor: &str,
    ) -> Result<(i64, i64), PageQueryError> {
        let (payload, tag) = split_cursor::<DRIVE_CURSOR_PAYLOAD_BYTES, DRIVE_CURSOR_TAG_BYTES>(
            cursor,
            DRIVE_CURSOR_PREFIX,
        )
        .ok_or(PageQueryError::Cursor)?;
        if !cursor_key.verifies_public_query_cursor_tag(&payload, &tag) || payload[0] != 1 {
            return Err(PageQueryError::Cursor);
        }
        let vehicle_id = Uuid::from_slice(&payload[1..17]).map_err(|_| PageQueryError::Cursor)?;
        let from_ms = i64_at(&payload, 17)?;
        let to_ms = i64_at(&payload, 25)?;
        let last_start_ms = i64_at(&payload, 33)?;
        let last_drive_id = i64_at(&payload, 41)?;
        if vehicle_id != self.vehicle_id
            || from_ms != self.from_ms
            || to_ms != self.to_ms
            || last_start_ms < from_ms
            || last_start_ms >= to_ms
            || last_drive_id <= 0
        {
            return Err(PageQueryError::Cursor);
        }
        Ok((last_start_ms, last_drive_id))
    }
}

/// Split `prefix.<payload hex>.<tag hex>` into its raw bytes. Only the exact
/// lengths and lowercase hexadecimal are accepted; the tag is not checked here.
fn split_cursor<const PAYLOAD: usize, const TAG: usize>(
    cursor: &str,
    prefix: &str,
) -> Option<([u8; PAYLOAD], [u8; TAG])> {
    let mut parts = cursor.split('.');
    let found_prefix = parts.next()?;
    let payload_hex = parts.next()?;
    let tag_hex = parts.next()?;
    if found_prefix != prefix
        || parts.next().is_some()
        || payload_hex.len() != PAYLOAD * 2
        || tag_hex.len() != TAG * 2
        || !lowercase_hex(payload_hex)
        || !lowercase_hex(tag_hex)
    {
        return None;
    }
    let mut payload = [0_u8; PAYLOAD];
    let mut tag = [0_u8; TAG];
    hex::decode_to_slice(payload_hex, &mut payload).ok()?;
    hex::decode_to_slice(tag_hex, &mut tag).ok()?;
    Some((payload, tag))
}

fn i64_at(payload: &[u8], offset: usize) -> Result<i64, PageQueryError> {
    payload
        .get(offset..offset + 8)
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .map(i64::from_be_bytes)
        .ok_or(PageQueryError::Cursor)
}

fn lowercase_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn key() -> CursorKey {
        CursorKey::from_bytes([7; 32])
    }

    fn vehicle() -> Uuid {
        Uuid::from_u128(0x1111_1111_1111_4111_8111_1111_1111_1111)
    }

    fn query(kind: PageKind, scope_id: i64) -> PageQuery {
        PageQuery {
            kind,
            vehicle_id: vehicle(),
            scope_id,
            from_ms: 10,
            to_ms: 1_000,
            limit: 1,
            after: None,
        }
    }

    fn parameters(cursor: Option<String>) -> PageQueryParameters {
        PageQueryParameters {
            from_ms: Some(10),
            to_ms: Some(1_000),
            limit: None,
            cursor,
        }
    }

    fn parse(kind: PageKind, scope_id: i64, cursor: &str) -> Result<PageQuery, PageQueryError> {
        PageQuery::parse(
            &key(),
            kind,
            vehicle(),
            scope_id,
            parameters(Some(cursor.to_owned())),
        )
    }

    #[test]
    fn tqp1_is_the_contract_layout_of_fifty_eight_bytes_and_a_tag() {
        let cursor =
            query(PageKind::Positions, 0x0102_0304_0506_0708).next_cursor(&key(), (500, 77));
        let parts: Vec<&str> = cursor.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "tqp1");
        assert_eq!(cursor.len(), 186);
        let payload = hex::decode(parts[1]).unwrap();
        let tag = hex::decode(parts[2]).unwrap();
        assert_eq!(payload.len(), 58);
        assert_eq!(payload[0], 1, "version");
        assert_eq!(payload[1], 3, "kind: positions");
        assert_eq!(&payload[2..18], vehicle().as_bytes());
        assert_eq!(&payload[18..26], &0x0102_0304_0506_0708_i64.to_be_bytes());
        assert_eq!(&payload[26..34], &10_i64.to_be_bytes());
        assert_eq!(&payload[34..42], &1_000_i64.to_be_bytes());
        assert_eq!(&payload[42..50], &500_i64.to_be_bytes());
        assert_eq!(&payload[50..58], &77_i64.to_be_bytes());
        assert_eq!(tag, key().public_query_cursor_tag(&payload));
        for (kind, code) in [
            (PageKind::Drives, 1),
            (PageKind::Charges, 2),
            (PageKind::Positions, 3),
            (PageKind::ChargeSamples, 4),
        ] {
            let cursor = query(kind, 0).next_cursor(&key(), (500, 77));
            let payload = hex::decode(cursor.split('.').nth(1).unwrap()).unwrap();
            assert_eq!(payload[1], code);
        }
    }

    #[test]
    fn tqp1_matches_the_committed_contract_examples_byte_for_byte() {
        // The examples are signed with the documented example key; no Hub uses it.
        let mut example_key = [0_u8; 32];
        for (index, byte) in example_key.iter_mut().enumerate() {
            *byte = index as u8;
        }
        let example_key = CursorKey::from_bytes(example_key);
        for (file, kind) in [
            ("drives", PageKind::Drives),
            ("charges", PageKind::Charges),
            ("positions", PageKind::Positions),
        ] {
            let path = format!(
                "{}/protocol/profiles/hub-http-v1/1.2.0/examples/{file}.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let page: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(path).expect("contract example"))
                    .expect("example JSON");
            let cursor = page["next_cursor"].as_str().expect("example cursor");
            let payload = hex::decode(cursor.split('.').nth(1).unwrap()).unwrap();
            let number =
                |offset: usize| i64::from_be_bytes(payload[offset..offset + 8].try_into().unwrap());
            let vehicle_id = Uuid::from_slice(&payload[2..18]).unwrap();
            let (scope_id, from_ms, to_ms, key_a, key_b) =
                (number(18), number(26), number(34), number(42), number(50));
            assert_eq!(payload[1], kind.cursor_code(), "{file} kind");
            let expected = PageQuery {
                kind,
                vehicle_id,
                scope_id,
                from_ms,
                to_ms,
                limit: 1,
                after: None,
            };
            // Our encoder reproduces the contract's bytes and tag exactly,
            assert_eq!(
                expected.next_cursor(&example_key, (key_a, key_b)),
                cursor,
                "{file}"
            );
            // and our decoder accepts them for the matching request.
            let parsed = PageQuery::parse(
                &example_key,
                kind,
                vehicle_id,
                scope_id,
                PageQueryParameters {
                    from_ms: Some(from_ms),
                    to_ms: Some(to_ms),
                    limit: None,
                    cursor: Some(cursor.to_owned()),
                },
            )
            .expect("example cursor decodes");
            assert_eq!(parsed.after, Some((key_a, key_b)), "{file}");
            // The Hub's own key does not verify the example's tag.
            assert!(
                PageQuery::parse(
                    &key(),
                    kind,
                    vehicle_id,
                    scope_id,
                    PageQueryParameters {
                        from_ms: Some(from_ms),
                        to_ms: Some(to_ms),
                        limit: None,
                        cursor: Some(cursor.to_owned()),
                    },
                )
                .is_err()
            );
        }
    }

    #[test]
    fn a_cursor_is_accepted_only_for_its_own_kind_vehicle_scope_and_window() {
        let cursor = query(PageKind::ChargeSamples, 9).next_cursor(&key(), (500, 77));
        let accepted = parse(PageKind::ChargeSamples, 9, &cursor).expect("same request");
        assert_eq!(accepted.after, Some((500, 77)));
        for kind in [PageKind::Drives, PageKind::Charges, PageKind::Positions] {
            assert_eq!(
                parse(kind, 9, &cursor),
                Err(PageQueryError::Cursor),
                "{kind:?}"
            );
        }
        assert_eq!(
            parse(PageKind::ChargeSamples, 8, &cursor),
            Err(PageQueryError::Cursor),
            "scope"
        );
        let other_vehicle = PageQuery::parse(
            &key(),
            PageKind::ChargeSamples,
            Uuid::from_u128(1),
            9,
            parameters(Some(cursor.clone())),
        );
        assert_eq!(other_vehicle, Err(PageQueryError::Cursor));
        for (from_ms, to_ms) in [(Some(11), Some(1_000)), (Some(10), Some(999)), (None, None)] {
            let other_window = PageQuery::parse(
                &key(),
                PageKind::ChargeSamples,
                vehicle(),
                9,
                PageQueryParameters {
                    from_ms,
                    to_ms,
                    limit: None,
                    cursor: Some(cursor.clone()),
                },
            );
            assert_eq!(
                other_window,
                Err(PageQueryError::Cursor),
                "{from_ms:?} {to_ms:?}"
            );
        }
        let other_key = PageQuery::parse(
            &CursorKey::from_bytes([8; 32]),
            PageKind::ChargeSamples,
            vehicle(),
            9,
            parameters(Some(cursor)),
        );
        assert_eq!(other_key, Err(PageQueryError::Cursor));
    }

    #[test]
    fn a_cursor_with_an_unsupported_version_or_out_of_window_keys_is_rejected_even_when_signed() {
        let signed = |edit: &dyn Fn(&mut [u8; 58])| {
            let cursor = query(PageKind::Positions, 3).next_cursor(&key(), (500, 77));
            let mut payload = [0_u8; 58];
            hex::decode_to_slice(cursor.split('.').nth(1).unwrap(), &mut payload).unwrap();
            edit(&mut payload);
            format!(
                "tqp1.{}.{}",
                hex::encode(payload),
                hex::encode(key().public_query_cursor_tag(&payload))
            )
        };
        assert!(parse(PageKind::Positions, 3, &signed(&|_| {})).is_ok());
        for (name, edit) in [
            (
                "version 0",
                (&|payload: &mut [u8; 58]| payload[0] = 0) as &dyn Fn(&mut [u8; 58]),
            ),
            ("version 2", &|payload: &mut [u8; 58]| payload[0] = 2),
            ("kind 0", &|payload: &mut [u8; 58]| payload[1] = 0),
            ("kind 5", &|payload: &mut [u8; 58]| payload[1] = 5),
            ("key A below from", &|payload: &mut [u8; 58]| {
                payload[42..50].copy_from_slice(&9_i64.to_be_bytes());
            }),
            ("key A at to", &|payload: &mut [u8; 58]| {
                payload[42..50].copy_from_slice(&1_000_i64.to_be_bytes());
            }),
            ("key B zero", &|payload: &mut [u8; 58]| {
                payload[50..58].copy_from_slice(&0_i64.to_be_bytes());
            }),
            ("key B negative", &|payload: &mut [u8; 58]| {
                payload[50..58].copy_from_slice(&(-1_i64).to_be_bytes());
            }),
        ] {
            assert_eq!(
                parse(PageKind::Positions, 3, &signed(edit)),
                Err(PageQueryError::Cursor),
                "{name}"
            );
        }
    }

    #[test]
    fn malformed_cursor_text_is_rejected() {
        let cursor = query(PageKind::Drives, 0).next_cursor(&key(), (500, 77));
        let mut parts: Vec<String> = cursor.split('.').map(str::to_owned).collect();
        let mut cases = vec![
            String::new(),
            "tqp1".to_owned(),
            "tqp1..".to_owned(),
            format!("{cursor}."),
            format!("x{cursor}"),
            cursor.replacen("tqp1", "TQP1", 1),
            cursor[..cursor.len() - 1].to_owned(),
            format!("{cursor}0"),
        ];
        parts[1] = parts[1].to_uppercase();
        cases.push(parts.join("."));
        let mut parts: Vec<String> = cursor.split('.').map(str::to_owned).collect();
        parts[2] = parts[2].to_uppercase();
        cases.push(parts.join("."));
        let mut parts: Vec<String> = cursor.split('.').map(str::to_owned).collect();
        parts[2] = format!("g{}", &parts[2][1..]);
        cases.push(parts.join("."));
        for case in cases {
            assert_eq!(
                parse(PageKind::Drives, 0, &case),
                Err(PageQueryError::Cursor),
                "{case}"
            );
        }
    }

    #[test]
    fn the_legacy_tqd1_cursor_is_accepted_on_drives_only_and_stays_bound() {
        let signed = |version: u8,
                      signer: &CursorKey,
                      vehicle_id: Uuid,
                      from_ms: i64,
                      to_ms: i64,
                      last: (i64, i64)| {
            let mut payload = [0_u8; 49];
            payload[0] = version;
            payload[1..17].copy_from_slice(vehicle_id.as_bytes());
            payload[17..25].copy_from_slice(&from_ms.to_be_bytes());
            payload[25..33].copy_from_slice(&to_ms.to_be_bytes());
            payload[33..41].copy_from_slice(&last.0.to_be_bytes());
            payload[41..49].copy_from_slice(&last.1.to_be_bytes());
            format!(
                "tqd1.{}.{}",
                hex::encode(payload),
                hex::encode(signer.public_query_cursor_tag(&payload))
            )
        };
        let legacy = |vehicle_id: Uuid, from_ms: i64, to_ms: i64, last: (i64, i64)| {
            signed(1, &key(), vehicle_id, from_ms, to_ms, last)
        };
        let cursor = legacy(vehicle(), 10, 1_000, (500, 77));
        assert_eq!(cursor.len(), 168);
        assert_eq!(
            parse(PageKind::Drives, 0, &cursor)
                .expect("legacy drives cursor")
                .after,
            Some((500, 77))
        );
        for kind in [
            PageKind::Charges,
            PageKind::Positions,
            PageKind::ChargeSamples,
        ] {
            assert_eq!(
                parse(kind, 0, &cursor),
                Err(PageQueryError::Cursor),
                "{kind:?}"
            );
        }
        for stale in [
            legacy(Uuid::from_u128(1), 10, 1_000, (500, 77)),
            legacy(vehicle(), 11, 1_000, (500, 77)),
            legacy(vehicle(), 10, 999, (500, 77)),
            legacy(vehicle(), 10, 1_000, (9, 77)),
            legacy(vehicle(), 10, 1_000, (1_000, 77)),
            legacy(vehicle(), 10, 1_000, (500, 0)),
        ] {
            assert_eq!(
                parse(PageKind::Drives, 0, &stale),
                Err(PageQueryError::Cursor)
            );
        }
        // The HMAC tag and the version byte of a tqd1 cursor are still checked:
        // the contract binds every cursor to this Hub's cursor key.
        let mut last_flipped = cursor.clone();
        let final_hex = last_flipped.pop().expect("tag ends in a hex digit");
        last_flipped.push(if final_hex == '0' { '1' } else { '0' });
        let mut first_flipped = cursor.clone();
        let tag_start = first_flipped.rfind('.').expect("tag separator") + 1;
        let first_hex = first_flipped.remove(tag_start);
        first_flipped.insert(tag_start, if first_hex == '0' { '1' } else { '0' });
        let other_key = CursorKey::from_bytes([8; 32]);
        for refused in [
            last_flipped,
            first_flipped,
            signed(1, &other_key, vehicle(), 10, 1_000, (500, 77)),
            signed(0, &key(), vehicle(), 10, 1_000, (500, 77)),
            signed(2, &key(), vehicle(), 10, 1_000, (500, 77)),
            // A valid payload with the tag of another payload.
            {
                let other = legacy(vehicle(), 10, 1_000, (501, 77));
                let tag = other.rsplit('.').next().expect("tag").to_owned();
                let head = cursor.rsplit_once('.').expect("tag separator").0;
                format!("{head}.{tag}")
            },
        ] {
            assert_eq!(
                parse(PageKind::Drives, 0, &refused),
                Err(PageQueryError::Cursor),
                "{refused}"
            );
        }
        // A tqp1 cursor on a drive request is the normal form.
        let current = query(PageKind::Drives, 0).next_cursor(&key(), (500, 77));
        assert!(current.starts_with("tqp1."));
        assert!(parse(PageKind::Drives, 0, &current).is_ok());
    }

    #[test]
    fn query_validation_reports_the_time_range_before_the_limit_before_the_cursor() {
        let parse_with = |from_ms, to_ms, limit, cursor: Option<&str>, kind: PageKind| {
            PageQuery::parse(
                &key(),
                kind,
                vehicle(),
                0,
                PageQueryParameters {
                    from_ms,
                    to_ms,
                    limit,
                    cursor: cursor.map(str::to_owned),
                },
            )
        };
        assert_eq!(
            parse_with(Some(5), Some(5), Some(0), Some("bad"), PageKind::Drives),
            Err(PageQueryError::TimeRange)
        );
        assert_eq!(
            parse_with(Some(-1), None, None, None, PageKind::Drives),
            Err(PageQueryError::TimeRange)
        );
        assert_eq!(
            parse_with(None, Some(-1), None, None, PageKind::Drives),
            Err(PageQueryError::TimeRange)
        );
        assert_eq!(
            parse_with(None, None, Some(0), Some("bad"), PageKind::Drives),
            Err(PageQueryError::Limit)
        );
        assert_eq!(
            parse_with(None, None, None, Some("bad"), PageKind::Drives),
            Err(PageQueryError::Cursor)
        );
        // Defaults and maxima per route.
        for (kind, default, maximum) in [
            (PageKind::Drives, 100, 500),
            (PageKind::Charges, 100, 500),
            (PageKind::Positions, 1000, 2048),
            (PageKind::ChargeSamples, 1000, 2048),
        ] {
            let defaults = parse_with(None, None, None, None, kind).unwrap();
            assert_eq!(
                (
                    defaults.limit,
                    defaults.from_ms,
                    defaults.to_ms,
                    defaults.after
                ),
                (default, 0, i64::MAX, None)
            );
            assert_eq!(
                parse_with(None, None, Some(maximum), None, kind)
                    .unwrap()
                    .limit,
                maximum
            );
            assert_eq!(
                parse_with(None, None, Some(maximum + 1), None, kind),
                Err(PageQueryError::Limit)
            );
            assert_eq!(
                parse_with(None, None, Some(1), None, kind).unwrap().limit,
                1
            );
        }
    }

    #[derive(serde::Serialize)]
    struct Row {
        id: i64,
        pad: String,
    }

    impl PageItem for Row {
        fn page_key(&self) -> (i64, i64) {
            (self.id * 10, self.id)
        }
    }

    fn row(id: i64, pad: usize) -> Row {
        Row {
            id,
            pad: "x".repeat(pad),
        }
    }

    fn cursor_text(key: (i64, i64)) -> String {
        format!("after-{}-{}", key.0, key.1)
    }

    #[test]
    fn page_assembly_keeps_order_and_cuts_the_cursor_after_the_last_item() {
        let page = bounded_public_page(Vec::<Row>::new(), false, cursor_text)
            .unwrap()
            .unwrap();
        assert!(page.items.is_empty() && page.next_cursor.is_none());
        // Even when the store says more exists, an empty list has no last item.
        let page = bounded_public_page(Vec::<Row>::new(), true, cursor_text)
            .unwrap()
            .unwrap();
        assert!(page.items.is_empty() && page.next_cursor.is_none());
        let rows = || vec![row(1, 3), row(2, 3), row(3, 3)];
        let page = bounded_public_page(rows(), false, cursor_text)
            .unwrap()
            .unwrap();
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.next_cursor, None);
        let page = bounded_public_page(rows(), true, cursor_text)
            .unwrap()
            .unwrap();
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.next_cursor.as_deref(), Some("after-30-3"));
        // The body is exactly the serialised envelope.
        assert_eq!(
            serde_json::to_string(&page).unwrap(),
            r#"{"items":[{"id":1,"pad":"xxx"},{"id":2,"pad":"xxx"},{"id":3,"pad":"xxx"}],"next_cursor":"after-30-3"}"#
        );
    }

    #[test]
    fn page_assembly_stops_before_the_item_that_would_exceed_the_cap() {
        // Three rows of about 400 000 bytes: two fit, the third does not.
        let rows = vec![row(1, 400_000), row(2, 400_000), row(3, 400_000)];
        let page = bounded_public_page(rows, false, cursor_text)
            .unwrap()
            .unwrap();
        assert_eq!(
            page.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            page.next_cursor.as_deref(),
            Some("after-20-2"),
            "the cursor names the last INCLUDED item even though no row remained to fetch"
        );
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_PUBLIC_PAGE_BYTES);
        // With rows to spare the cursor is the same.
        let rows = vec![row(1, 400_000), row(2, 400_000), row(3, 400_000)];
        let page = bounded_public_page(rows, true, cursor_text)
            .unwrap()
            .unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.next_cursor.as_deref(), Some("after-20-2"));
    }

    #[test]
    fn page_assembly_is_exact_at_the_cap_and_counts_the_envelope_and_cursor() {
        let base = serde_json::to_vec(&row(1, 0)).unwrap().len();
        // An item and the bare envelope `{"items":[],"next_cursor":null}`.
        let envelope = serde_json::to_vec(&PublicPage::<Row> {
            items: Vec::new(),
            next_cursor: None,
        })
        .unwrap()
        .len();
        let exact = MAX_PUBLIC_PAGE_BYTES - envelope - base;
        let fits = bounded_public_page(vec![row(1, exact)], false, cursor_text)
            .unwrap()
            .expect("a page of exactly the cap is served");
        assert_eq!(
            serde_json::to_vec(&fits).unwrap().len(),
            MAX_PUBLIC_PAGE_BYTES
        );
        assert!(
            bounded_public_page(vec![row(1, exact + 1)], false, cursor_text)
                .unwrap()
                .is_none(),
            "one byte over the cap alone is not served"
        );
        // With a cursor the envelope is longer and the item must be smaller.
        let cursor_len = cursor_text((10, 1)).len();
        let with_cursor = exact - (cursor_len + 2 - "null".len());
        let page = bounded_public_page(vec![row(1, with_cursor)], true, cursor_text)
            .unwrap()
            .expect("fits with its cursor");
        assert_eq!(
            serde_json::to_vec(&page).unwrap().len(),
            MAX_PUBLIC_PAGE_BYTES
        );
        assert!(
            bounded_public_page(vec![row(1, with_cursor + 1)], true, cursor_text)
                .unwrap()
                .is_none()
        );
        // The comma between items counts too: two items whose sizes plus the
        // envelope are exactly the cap WITHOUT the comma must not both be served.
        let spare = MAX_PUBLIC_PAGE_BYTES - envelope - 2 * base;
        let rows = vec![row(1, spare / 2), row(2, spare - spare / 2)];
        let page = bounded_public_page(rows, false, cursor_text)
            .unwrap()
            .unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_PUBLIC_PAGE_BYTES);
        assert_eq!(
            page.items.len(),
            1,
            "two items plus the comma exceed the cap"
        );
        assert_eq!(page.next_cursor.as_deref(), Some("after-10-1"));
    }

    #[test]
    fn a_position_claims_terrain_provenance_only_with_a_successful_lookup_and_an_elevation() {
        let position = |elevation| {
            serde_json::from_value::<ProjectionPosition>(serde_json::json!({
                "id": 1, "drive_id": 7, "car_id": 1, "date_ms": 5,
                "latitude": 0.0, "longitude": 0.0, "speed": null, "power": 12.0,
                "battery_level": null, "usable_battery_level": null,
                "elevation": elevation, "odometer": null,
                "ideal_battery_range_km": null
            }))
            .unwrap()
        };
        let lookup = || PositionTerrainLookup {
            dataset: Some("set".to_owned()),
            dataset_version: None,
            tile: Some("tile".to_owned()),
        };
        let claimed = PublicPosition::from_projection(7, position(Some(12)), Some(lookup()));
        assert_eq!(
            claimed.elevation_provenance,
            Some(PublicElevationProvenance {
                source: "terrain",
                dataset: Some("set".to_owned()),
                dataset_version: None,
                tile: Some("tile".to_owned()),
            })
        );
        assert!(
            PublicPosition::from_projection(7, position(None), Some(lookup()))
                .elevation_provenance
                .is_none(),
            "no served elevation, nothing to attribute"
        );
        assert!(
            PublicPosition::from_projection(7, position(Some(12)), None)
                .elevation_provenance
                .is_none(),
            "no terrain lookup, no terrain claim"
        );
        // The page's drive id is served, and power is a number.
        let json = serde_json::to_value(&claimed).unwrap();
        assert_eq!(json["drive_id"], 7);
        assert!(json["power"].is_f64());
        assert!(json["elevation_provenance"].is_object());
    }

    #[test]
    fn a_charge_without_an_end_date_is_not_a_public_charge() {
        let charge = |end_date_ms: Option<i64>| {
            serde_json::from_value::<ProjectionCharge>(serde_json::json!({
                "id": 1, "car_id": 1, "start_date_ms": 5, "end_date_ms": end_date_ms,
                "charge_energy_added": null, "start_battery_level": null,
                "end_battery_level": null, "duration_min": null, "address": null,
                "location_name": null, "geofence": null, "is_dc": null,
                "charge_rate_km_per_hour": null, "max_charger_power_kw": null,
                "outside_temp_avg": null, "start_rated_range_km": null,
                "end_rated_range_km": null
            }))
            .unwrap()
        };
        assert!(PublicCharge::from_projection(vehicle(), charge(None)).is_none());
        let closed = PublicCharge::from_projection(vehicle(), charge(Some(9))).unwrap();
        assert_eq!(closed.end_date_ms, 9);
        assert_eq!(closed.vehicle_id, vehicle());
    }
}
