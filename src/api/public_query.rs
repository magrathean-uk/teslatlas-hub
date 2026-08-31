// SPDX-License-Identifier: AGPL-3.0-only

use uuid::Uuid;

use crate::{hub_pack::ProjectionDrive, protocol::CursorKey};

pub(crate) const DEFAULT_DRIVE_PAGE_LIMIT: u32 = 100;
pub(crate) const MAX_DRIVE_PAGE_LIMIT: u32 = 500;

const DRIVE_CURSOR_PREFIX: &str = "tqd1";
const DRIVE_CURSOR_PAYLOAD_BYTES: usize = 49;
const DRIVE_CURSOR_TAG_BYTES: usize = 32;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DriveQueryParameters {
    pub(crate) from_ms: Option<i64>,
    pub(crate) to_ms: Option<i64>,
    pub(crate) limit: Option<u32>,
    pub(crate) cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DriveQuery {
    pub(crate) from_ms: i64,
    pub(crate) to_ms: i64,
    pub(crate) limit: u32,
    pub(crate) after: Option<(i64, i64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DriveQueryError {
    TimeRange,
    Limit,
    Cursor,
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

impl DriveQuery {
    pub(crate) fn parse(
        cursor_key: &CursorKey,
        vehicle_id: Uuid,
        parameters: DriveQueryParameters,
    ) -> Result<Self, DriveQueryError> {
        let from_ms = parameters.from_ms.unwrap_or(0);
        let to_ms = parameters.to_ms.unwrap_or(i64::MAX);
        if from_ms < 0 || to_ms < 0 || from_ms >= to_ms {
            return Err(DriveQueryError::TimeRange);
        }
        let limit = parameters.limit.unwrap_or(DEFAULT_DRIVE_PAGE_LIMIT);
        if limit == 0 || limit > MAX_DRIVE_PAGE_LIMIT {
            return Err(DriveQueryError::Limit);
        }
        let after = parameters
            .cursor
            .as_deref()
            .map(|cursor| decode_drive_cursor(cursor_key, cursor, vehicle_id, from_ms, to_ms))
            .transpose()?;
        Ok(Self {
            from_ms,
            to_ms,
            limit,
            after,
        })
    }

    pub(crate) fn next_cursor(
        self,
        cursor_key: &CursorKey,
        vehicle_id: Uuid,
        last: (i64, i64),
    ) -> String {
        encode_drive_cursor(cursor_key, vehicle_id, self.from_ms, self.to_ms, last)
    }
}

fn encode_drive_cursor(
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    from_ms: i64,
    to_ms: i64,
    last: (i64, i64),
) -> String {
    let mut payload = [0_u8; DRIVE_CURSOR_PAYLOAD_BYTES];
    payload[0] = 1;
    payload[1..17].copy_from_slice(vehicle_id.as_bytes());
    payload[17..25].copy_from_slice(&from_ms.to_be_bytes());
    payload[25..33].copy_from_slice(&to_ms.to_be_bytes());
    payload[33..41].copy_from_slice(&last.0.to_be_bytes());
    payload[41..49].copy_from_slice(&last.1.to_be_bytes());
    let tag = cursor_key.public_query_cursor_tag(&payload);
    format!(
        "{DRIVE_CURSOR_PREFIX}.{}.{}",
        hex::encode(payload),
        hex::encode(tag)
    )
}

fn decode_drive_cursor(
    cursor_key: &CursorKey,
    cursor: &str,
    expected_vehicle_id: Uuid,
    expected_from_ms: i64,
    expected_to_ms: i64,
) -> Result<(i64, i64), DriveQueryError> {
    let mut parts = cursor.split('.');
    let prefix = parts.next().ok_or(DriveQueryError::Cursor)?;
    let payload_hex = parts.next().ok_or(DriveQueryError::Cursor)?;
    let tag_hex = parts.next().ok_or(DriveQueryError::Cursor)?;
    if prefix != DRIVE_CURSOR_PREFIX
        || parts.next().is_some()
        || payload_hex.len() != DRIVE_CURSOR_PAYLOAD_BYTES * 2
        || tag_hex.len() != DRIVE_CURSOR_TAG_BYTES * 2
        || !lowercase_hex(payload_hex)
        || !lowercase_hex(tag_hex)
    {
        return Err(DriveQueryError::Cursor);
    }
    let mut payload = [0_u8; DRIVE_CURSOR_PAYLOAD_BYTES];
    let mut tag = [0_u8; DRIVE_CURSOR_TAG_BYTES];
    hex::decode_to_slice(payload_hex, &mut payload).map_err(|_| DriveQueryError::Cursor)?;
    hex::decode_to_slice(tag_hex, &mut tag).map_err(|_| DriveQueryError::Cursor)?;
    if !cursor_key.verifies_public_query_cursor_tag(&payload, &tag) || payload[0] != 1 {
        return Err(DriveQueryError::Cursor);
    }
    let vehicle_id = Uuid::from_slice(&payload[1..17]).map_err(|_| DriveQueryError::Cursor)?;
    let from_ms = i64::from_be_bytes(
        payload[17..25]
            .try_into()
            .map_err(|_| DriveQueryError::Cursor)?,
    );
    let to_ms = i64::from_be_bytes(
        payload[25..33]
            .try_into()
            .map_err(|_| DriveQueryError::Cursor)?,
    );
    let last_start_ms = i64::from_be_bytes(
        payload[33..41]
            .try_into()
            .map_err(|_| DriveQueryError::Cursor)?,
    );
    let last_drive_id = i64::from_be_bytes(
        payload[41..49]
            .try_into()
            .map_err(|_| DriveQueryError::Cursor)?,
    );
    if vehicle_id != expected_vehicle_id
        || from_ms != expected_from_ms
        || to_ms != expected_to_ms
        || last_start_ms < from_ms
        || last_start_ms >= to_ms
        || last_drive_id <= 0
    {
        return Err(DriveQueryError::Cursor);
    }
    Ok((last_start_ms, last_drive_id))
}

fn lowercase_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
