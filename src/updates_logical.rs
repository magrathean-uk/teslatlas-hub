// SPDX-License-Identifier: AGPL-3.0-only

//! Canonical TeslaMate `updates` logical-row stream and three-layer receipt.
//!
//! Counts, bounds, and SHA-256 are derived from the same
//! `teslatlas-updates-logical-row-v1` bytes. Null/unknown values stay tagged
//! and are never coerced to zero or empty.

use sha2::{Digest, Sha256};

use crate::teslamate_projection::TeslaMateUpdatePhysicalV2_2 as UpdateRow;

pub const LOGICAL_STREAM_HEADER: &[u8] = b"teslatlas-updates-logical-row-v1\n";
pub const LOGICAL_STREAM_SCHEMA: &str = "teslatlas-updates-logical-row-v1";
pub const PINNED_TESLAMATE_REVISION: &str = "7054517c10475f39f480edeae8f90c6f717985a3";
pub const PINNED_CANONICAL_SHA256: &str =
    "0c74cec3c0a5fb956d53600b5cb9bad76479e3234f011a415e47d3b0f4bdabf4";
pub const PINNED_CANONICAL_BYTES: usize = 218;
pub const PINNED_SELECTED_CAR_ID: i16 = -32768;
pub const UPDATES_FIELD_COUNT: u64 = 5;
pub const APP_SCHEMA_VERSION: &str = "teslatlas-app-car-updates-physical-v25";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalUpdatesStream {
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub rows: Vec<UpdateRow>,
    pub summary: LogicalUpdatesSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogicalUpdatesSummary {
    pub row_count: u64,
    pub completed_row_count: u64,
    pub open_row_count: u64,
    pub null_version_row_count: u64,
    pub empty_version_row_count: u64,
    pub start_min_pg_us: Option<i64>,
    pub start_max_pg_us: Option<i64>,
    pub end_min_pg_us: Option<i64>,
    pub end_max_pg_us: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatesLogicalError {
    pub message: String,
}

impl std::fmt::Display for UpdatesLogicalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UpdatesLogicalError {}

fn reject(message: impl Into<String>) -> UpdatesLogicalError {
    UpdatesLogicalError {
        message: message.into(),
    }
}

/// Encode selected-car update rows into the canonical logical-row stream.
///
/// Rows are ordered by `(start_date_pg_us, id)` numeric ascending. Version
/// bytes are written verbatim; null stays a tag, and empty string stays a
/// length-zero payload.
pub fn encode_updates_logical_stream(
    rows: &[UpdateRow],
) -> Result<LogicalUpdatesStream, UpdatesLogicalError> {
    let mut ordered = rows.to_vec();
    ordered.sort_by(|left, right| {
        left.start_date_pg_us
            .cmp(&right.start_date_pg_us)
            .then(left.id.cmp(&right.id))
    });
    let mut bytes = LOGICAL_STREAM_HEADER.to_vec();
    for row in &ordered {
        bytes.extend_from_slice(&row.id.to_be_bytes());
        bytes.extend_from_slice(&row.car_id.to_be_bytes());
        bytes.extend_from_slice(&row.start_date_pg_us.to_be_bytes());
        match row.end_date_pg_us {
            None => bytes.push(0x00),
            Some(end) => {
                bytes.push(0x01);
                bytes.extend_from_slice(&end.to_be_bytes());
            }
        }
        match row.version.as_deref() {
            None => bytes.push(0x00),
            Some(version) => {
                let utf8 = version.as_bytes();
                let length = u32::try_from(utf8.len()).map_err(|_| {
                    reject("version UTF-8 length exceeds the logical-row u32 domain")
                })?;
                bytes.push(0x01);
                bytes.extend_from_slice(&length.to_be_bytes());
                bytes.extend_from_slice(utf8);
            }
        }
    }
    let summary = summary_from_ordered_rows(&ordered)?;
    Ok(LogicalUpdatesStream {
        sha256: hex_sha256(&bytes),
        bytes,
        rows: ordered,
        summary,
    })
}

/// Decode a canonical stream and recompute the summary from those exact bytes.
pub fn decode_updates_logical_stream(
    bytes: &[u8],
) -> Result<LogicalUpdatesStream, UpdatesLogicalError> {
    if !bytes.starts_with(LOGICAL_STREAM_HEADER) {
        return Err(reject(
            "logical stream header is not teslatlas-updates-logical-row-v1",
        ));
    }
    let mut cursor = LOGICAL_STREAM_HEADER.len();
    let mut rows = Vec::new();
    while cursor < bytes.len() {
        let id = read_i32(bytes, &mut cursor, "id")?;
        let car_id = read_i16(bytes, &mut cursor, "car_id")?;
        let start_date_pg_us = read_i64(bytes, &mut cursor, "start_date_pg_us")?;
        let end_date_pg_us = match read_tag(bytes, &mut cursor, "end_date")? {
            0x00 => None,
            0x01 => Some(read_i64(bytes, &mut cursor, "end_date_pg_us")?),
            _ => return Err(reject("end_date tag is not 0x00 or 0x01")),
        };
        let version = match read_tag(bytes, &mut cursor, "version")? {
            0x00 => None,
            0x01 => {
                let length = read_u32(bytes, &mut cursor, "version length")?;
                let start = cursor;
                let end = start
                    .checked_add(length as usize)
                    .ok_or_else(|| reject("version length overflow"))?;
                if end > bytes.len() {
                    return Err(reject("version payload overruns the stream"));
                }
                cursor = end;
                Some(
                    std::str::from_utf8(&bytes[start..end])
                        .map_err(|_| reject("version is not exact UTF-8"))?
                        .to_owned(),
                )
            }
            _ => return Err(reject("version tag is not 0x00 or 0x01")),
        };
        rows.push(UpdateRow {
            id,
            car_id,
            start_date_pg_us,
            end_date_pg_us,
            version,
        });
    }
    let summary = summary_from_ordered_rows(&rows)?;
    Ok(LogicalUpdatesStream {
        sha256: hex_sha256(bytes),
        bytes: bytes.to_vec(),
        rows,
        summary,
    })
}

fn summary_from_ordered_rows(
    rows: &[UpdateRow],
) -> Result<LogicalUpdatesSummary, UpdatesLogicalError> {
    let row_count = u64::try_from(rows.len()).map_err(|_| reject("row count overflow"))?;
    let mut completed = 0_u64;
    let mut open = 0_u64;
    let mut null_version = 0_u64;
    let mut empty_version = 0_u64;
    let mut start_min = None;
    let mut start_max = None;
    let mut end_min = None;
    let mut end_max = None;
    for row in rows {
        match row.end_date_pg_us {
            None => {
                open = open
                    .checked_add(1)
                    .ok_or_else(|| reject("open row count overflow"))?;
            }
            Some(end) => {
                completed = completed
                    .checked_add(1)
                    .ok_or_else(|| reject("completed row count overflow"))?;
                end_min = Some(end_min.map_or(end, |current: i64| current.min(end)));
                end_max = Some(end_max.map_or(end, |current: i64| current.max(end)));
            }
        }
        match row.version.as_deref() {
            None => {
                null_version = null_version
                    .checked_add(1)
                    .ok_or_else(|| reject("null-version row count overflow"))?;
            }
            Some("") => {
                empty_version = empty_version
                    .checked_add(1)
                    .ok_or_else(|| reject("empty-version row count overflow"))?;
            }
            Some(_) => {}
        }
        start_min = Some(start_min.map_or(row.start_date_pg_us, |current: i64| {
            current.min(row.start_date_pg_us)
        }));
        start_max = Some(start_max.map_or(row.start_date_pg_us, |current: i64| {
            current.max(row.start_date_pg_us)
        }));
    }
    if row_count == 0 {
        return Ok(LogicalUpdatesSummary {
            row_count: 0,
            completed_row_count: 0,
            open_row_count: 0,
            null_version_row_count: 0,
            empty_version_row_count: 0,
            start_min_pg_us: None,
            start_max_pg_us: None,
            end_min_pg_us: None,
            end_max_pg_us: None,
        });
    }
    if completed.saturating_add(open) != row_count {
        return Err(reject(
            "completed plus open row counts do not equal the stream row count",
        ));
    }
    if null_version > row_count {
        return Err(reject("null-version count exceeds the stream row count"));
    }
    Ok(LogicalUpdatesSummary {
        row_count,
        completed_row_count: completed,
        open_row_count: open,
        null_version_row_count: null_version,
        empty_version_row_count: empty_version,
        start_min_pg_us: start_min,
        start_max_pg_us: start_max,
        end_min_pg_us: end_min,
        end_max_pg_us: end_max,
    })
}

pub fn hex_sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_tag(bytes: &[u8], cursor: &mut usize, label: &str) -> Result<u8, UpdatesLogicalError> {
    let value = *bytes
        .get(*cursor)
        .ok_or_else(|| reject(format!("{label} tag is truncated")))?;
    *cursor += 1;
    Ok(value)
}

fn read_i16(bytes: &[u8], cursor: &mut usize, label: &str) -> Result<i16, UpdatesLogicalError> {
    Ok(i16::from_be_bytes(read_array(bytes, cursor, label)?))
}

fn read_i32(bytes: &[u8], cursor: &mut usize, label: &str) -> Result<i32, UpdatesLogicalError> {
    Ok(i32::from_be_bytes(read_array(bytes, cursor, label)?))
}

fn read_i64(bytes: &[u8], cursor: &mut usize, label: &str) -> Result<i64, UpdatesLogicalError> {
    Ok(i64::from_be_bytes(read_array(bytes, cursor, label)?))
}

fn read_u32(bytes: &[u8], cursor: &mut usize, label: &str) -> Result<u32, UpdatesLogicalError> {
    Ok(u32::from_be_bytes(read_array(bytes, cursor, label)?))
}

fn read_array<const N: usize>(
    bytes: &[u8],
    cursor: &mut usize,
    label: &str,
) -> Result<[u8; N], UpdatesLogicalError> {
    let end = cursor
        .checked_add(N)
        .ok_or_else(|| reject(format!("{label} length overflow")))?;
    let slice = bytes
        .get(*cursor..end)
        .ok_or_else(|| reject(format!("{label} is truncated")))?;
    *cursor = end;
    slice
        .try_into()
        .map_err(|_| reject(format!("{label} is truncated")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip_preserves_null_empty_and_unicode() {
        let rows = vec![
            UpdateRow {
                id: 0,
                car_id: PINNED_SELECTED_CAR_ID,
                start_date_pg_us: 1,
                end_date_pg_us: None,
                version: None,
            },
            UpdateRow {
                id: -1,
                car_id: PINNED_SELECTED_CAR_ID,
                start_date_pg_us: 1,
                end_date_pg_us: Some(2),
                version: Some(String::new()),
            },
            UpdateRow {
                id: 2,
                car_id: PINNED_SELECTED_CAR_ID,
                start_date_pg_us: 0,
                end_date_pg_us: Some(3),
                version: Some("  βeta 🚗  ".into()),
            },
        ];
        let encoded = encode_updates_logical_stream(&rows).expect("encode");
        assert_eq!(encoded.summary.row_count, 3);
        assert_eq!(encoded.summary.open_row_count, 1);
        assert_eq!(encoded.summary.null_version_row_count, 1);
        assert_eq!(encoded.summary.empty_version_row_count, 1);
        assert_eq!(encoded.rows[0].id, 2);
        let decoded = decode_updates_logical_stream(&encoded.bytes).expect("decode");
        assert_eq!(decoded.rows, encoded.rows);
        assert_eq!(decoded.sha256, encoded.sha256);
        assert_eq!(decoded.summary, encoded.summary);
    }
}
