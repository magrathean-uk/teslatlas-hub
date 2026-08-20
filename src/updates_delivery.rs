//! Shipped selected-car TeslaMate `updates` path through Hub schema 2.2.
//!
//! Read-only source decode, Hub persist/publish, signed full snapshot, signed
//! no-op, and receipt emit are separate so each step can be driven without the
//! next. Production TeslaMate is never written.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Cursor, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

use rusqlite::{Connection, OpenFlags};
use rustix::fs::{FileType, Mode, OFlags, fstat, open};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    db::{HubStore, PublicationGate, StoreError},
    hub_pack::{
        BuiltProjectionPack, ProjectionBinding, ProjectionCarSettingsV2_2, ProjectionCarV2_2,
        ProjectionGlobalSettingsV2_2, ProjectionPackError, ProjectionPackRequestV2_2,
        ProjectionPackWriter, ProjectionPreferredRangeV2_2, ProjectionSnapshotV2_2,
        ProjectionUnitOfLengthV2_2, ProjectionUnitOfPressureV2_2, ProjectionUnitOfTemperatureV2_2,
    },
    protocol::{
        CursorKey, HUB_PROJECTION_SCHEMA_V2, HUB_PROJECTION_SCHEMA_V3, OpaqueCursor,
        ProtocolLimits, SequenceRange, SyncManifest, TransportPack,
    },
    teslamate_direct::DirectUpdatesSourceV2_2,
    teslamate_projection::TeslaMateUpdatePhysicalV2_2,
    teslamate_schema::{
        MAX_VALIDATED_MIGRATION, TESLAMATE_V4_MIGRATION_COUNT, TESLAMATE_V4_MIGRATION_SET_SHA256,
        TESLAMATE_V4_SOURCE_REVISION,
    },
    updates_logical::{
        APP_SCHEMA_VERSION, LOGICAL_STREAM_SCHEMA, LogicalUpdatesStream, LogicalUpdatesSummary,
        PINNED_CANONICAL_BYTES, PINNED_CANONICAL_SHA256, PINNED_SELECTED_CAR_ID,
        PINNED_TESLAMATE_REVISION, UPDATES_FIELD_COUNT, UpdatesLogicalError,
        encode_updates_logical_stream, hex_sha256,
    },
};

const PINNED_FIXTURE_SQL: &[u8] =
    include_bytes!("../fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql");
const PINNED_FIXTURE_SQL_SHA256: &str =
    "d8eebcbbb2f7e2039caa5cc509b0ff76b4aea56e4db1631d16f27aabf86d23db";

const FIXED_PACK_ID: &str = "91126d25-b1ed-5fe3-9e38-a4ad4f54285e";
const FIXED_SNAPSHOT_ID: &str = "ea03e388-b812-5c4e-826e-be3eac371187";
const FIXED_INSTALLATION_ID: &str = "ec84d3c9-8df0-5bf0-aeb5-2fef942533ec";
const FIXED_ACCOUNT_ID: &str = "0d5d1d82-caeb-57ad-877a-0033e27a38b1";
const FIXED_VEHICLE_ID: &str = "1c43cf37-e0fb-5ff3-8a73-a43b2e2efb51";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatesDeliveryError {
    pub message: String,
}

impl std::fmt::Display for UpdatesDeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UpdatesDeliveryError {}

impl From<UpdatesLogicalError> for UpdatesDeliveryError {
    fn from(error: UpdatesLogicalError) -> Self {
        Self {
            message: error.message,
        }
    }
}

impl From<ProjectionPackError> for UpdatesDeliveryError {
    fn from(error: ProjectionPackError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl From<StoreError> for UpdatesDeliveryError {
    fn from(error: StoreError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

fn reject(message: impl Into<String>) -> UpdatesDeliveryError {
    UpdatesDeliveryError {
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUpdatesFixture {
    pub selected_car_id: i16,
    pub rows: Vec<TeslaMateUpdatePhysicalV2_2>,
    pub sql_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedNoOpState {
    pub schema: String,
    pub projection_schema: String,
    pub installation_id: Uuid,
    pub account_id: Uuid,
    pub vehicle_id: Uuid,
    pub generation: u64,
    pub snapshot_id: Uuid,
    pub head_sequence: u64,
    pub pack_sha256: String,
    pub terminal_cursor: OpaqueCursor,
    #[serde(
        default,
        rename = "sourceWitness",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_witness: Option<ProductionUpdatesSourceWitness>,
}

/// Source and reopened-Hub facts derived without leaving the exported
/// PostgreSQL snapshot. The exact JSON object is carried by the Ed25519-signed
/// no-op response and is paired to the manifest through the outer snapshot,
/// sequence, cursor, and pack digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProductionUpdatesSourceWitness {
    pub schema: String,
    pub selected_car_id: i64,
    pub postgres_snapshot_sha256: String,
    pub source_transaction: String,
    pub source_revision: String,
    pub pinned_migration_set_sha256: String,
    pub source_schema_fingerprint: String,
    pub observed_migration_version: i64,
    pub observed_migration_count: u64,
    pub source_capture_sha256: String,
    pub source_logical_sha256: String,
    pub source_row_count: u64,
    pub source_completed_row_count: u64,
    pub source_open_row_count: u64,
    pub source_null_version_row_count: u64,
    pub source_empty_version_row_count: u64,
    pub source_start_min_pg_us: Option<i64>,
    pub source_start_max_pg_us: Option<i64>,
    pub source_end_min_pg_us: Option<i64>,
    pub source_end_max_pg_us: Option<i64>,
    pub hub_logical_sha256: String,
    pub hub_row_count: u64,
    pub hub_completed_row_count: u64,
    pub hub_open_row_count: u64,
    pub hub_null_version_row_count: u64,
    pub hub_empty_version_row_count: u64,
    pub hub_start_min_pg_us: Option<i64>,
    pub hub_start_max_pg_us: Option<i64>,
    pub hub_end_min_pg_us: Option<i64>,
    pub hub_end_max_pg_us: Option<i64>,
    pub car_identity_sha256: String,
    pub pack_sha256: String,
    pub head_sequence: u64,
    pub terminal_cursor_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LayerCounts {
    pub source: u64,
    pub hub: u64,
    pub app: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LayerTimeBounds {
    pub start_min_pg_us: Option<i64>,
    pub start_max_pg_us: Option<i64>,
    pub end_min_pg_us: Option<i64>,
    pub end_max_pg_us: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdatesReceipt {
    pub schema: String,
    pub receipt_state: String,
    pub receipt_not_fabricated: bool,
    pub reference_commit: String,
    pub table: String,
    pub field_count: u64,
    pub app_schema_version: String,
    pub canonical_stream_schema: String,
    pub source_row_count: u64,
    pub selected_car_row_count: u64,
    pub hub_row_count: u64,
    pub app_row_count: u64,
    pub completed_row_counts: LayerCounts,
    pub open_row_counts: LayerCounts,
    pub null_version_row_counts: LayerCounts,
    pub skipped_reasons: serde_json::Map<String, serde_json::Value>,
    pub time_bounds: serde_json::Value,
    pub hashes: serde_json::Value,
    pub candidate: serde_json::Value,
    pub selected_car_id: i16,
    pub accepted_field_count: u64,
    pub schema_version: String,
    pub manifest_sha256: String,
    pub pack_sha256: String,
    pub source_commit: String,
    pub binary_sha256: String,
    pub config_sha256: String,
    pub toolchain_sha256: String,
}

#[derive(Debug)]
pub struct UpdatesDeliveryArtifacts {
    pub source: LogicalUpdatesStream,
    pub hub: LogicalUpdatesStream,
    pub fixture: ParsedUpdatesFixture,
    pub binding: ProjectionBinding,
    pub built: BuiltProjectionPack,
    pub manifest: SyncManifest,
    pub manifest_bytes: Vec<u8>,
    pub manifest_sha256: String,
    pub pack_bytes: Vec<u8>,
    pub noop: SignedNoOpState,
    pub noop_bytes: Vec<u8>,
    pub receipt: UpdatesReceipt,
}

/// Receipt-ready source/Hub facts from the production selected-car path. This
/// is intentionally not an App acceptance receipt: it proves only that the
/// exact exported PostgreSQL snapshot reached a manifest-last Hub pair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProductionUpdatesPublication {
    pub state: String,
    pub selected_car_id: i64,
    pub vehicle_id: Uuid,
    pub snapshot_id: Uuid,
    pub sequence: u64,
    pub reused_current_snapshot: bool,
    pub source_revision: String,
    pub source_schema_fingerprint: String,
    pub observed_migration_version: i64,
    pub observed_migration_count: usize,
    pub source_logical_sha256: String,
    pub source_capture_sha256: String,
    pub hub_logical_sha256: String,
    pub source_summary: LogicalUpdatesSummary,
    pub hub_summary: LogicalUpdatesSummary,
    pub manifest_sha256: String,
    pub pack_sha256: String,
    pub noop_sha256: String,
    pub source_witness: ProductionUpdatesSourceWitness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProductionUpdatesHead {
    snapshot_id: Uuid,
    head_sequence: u64,
    manifest_sha256: String,
}

pub(crate) fn production_updates_head(
    store: &HubStore,
    vehicle_id: Uuid,
) -> Result<Option<ProductionUpdatesHead>, UpdatesDeliveryError> {
    store
        .manifest_for_vehicle(vehicle_id)?
        .filter(|manifest| manifest.schema == HUB_PROJECTION_SCHEMA_V3)
        .map(|manifest| {
            let manifest_sha256 = hex_sha256(
                &serde_json::to_vec(&manifest).map_err(|error| reject(error.to_string()))?,
            );
            Ok(ProductionUpdatesHead {
                snapshot_id: manifest.snapshot_id,
                head_sequence: manifest.head_sequence,
                manifest_sha256,
            })
        })
        .transpose()
}

/// Parse the pinned PostgreSQL COPY fixture without writing any source database.
pub fn parse_pinned_updates_fixture() -> Result<ParsedUpdatesFixture, UpdatesDeliveryError> {
    parse_updates_copy_sql(PINNED_FIXTURE_SQL)
}

/// Parse TeslaMate `updates` COPY CSV (comma, `\\N` null, LF) into physical rows.
pub fn parse_updates_copy_sql(bytes: &[u8]) -> Result<ParsedUpdatesFixture, UpdatesDeliveryError> {
    let sql_sha256 = hex_sha256(bytes);
    if bytes != PINNED_FIXTURE_SQL && sql_sha256 == PINNED_FIXTURE_SQL_SHA256 {
        return Err(reject("fixture bytes do not match the pinned SQL identity"));
    }
    if sql_sha256 == PINNED_FIXTURE_SQL_SHA256 && bytes != PINNED_FIXTURE_SQL {
        return Err(reject("pinned fixture identity collision"));
    }
    if bytes == PINNED_FIXTURE_SQL && sql_sha256 != PINNED_FIXTURE_SQL_SHA256 {
        return Err(reject("pinned fixture SHA-256 drifted"));
    }
    if !bytes.ends_with(b"COMMIT;\n") || bytes.contains(&b'\r') {
        return Err(reject(
            "COPY fixture must use LF row endings and a COMMIT trailer",
        ));
    }
    let marker =
        b"COPY updates (id, car_id, start_date, end_date, version) FROM stdin WITH (FORMAT csv, NULL '\\N');\n";
    let start = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .ok_or_else(|| reject("COPY updates header is missing"))?
        + marker.len();
    const TERMINATOR: &[u8] = b"\n\\.\n";
    let end = bytes[start..]
        .windows(TERMINATOR.len())
        .position(|window| window == TERMINATOR)
        .ok_or_else(|| reject("COPY terminator is missing"))?
        + start;
    let cars = b"COPY cars (id) FROM stdin WITH (FORMAT csv, NULL '\\N');\n-32768\n\\.\n\n";
    if !bytes.windows(cars.len()).any(|window| window == cars) {
        return Err(reject("selected-car COPY row is missing"));
    }
    let mut rows = Vec::new();
    for line in bytes[start..end].split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields = copy_fields(line)?;
        if fields.len() != 5 {
            return Err(reject("COPY column count is not 5"));
        }
        let required = |index: usize, label: &str| {
            fields[index]
                .as_deref()
                .ok_or_else(|| reject(format!("{label} is unexpectedly SQL NULL")))
        };
        rows.push(TeslaMateUpdatePhysicalV2_2 {
            id: parse_ascii_i32(required(0, "id")?, "id")?,
            car_id: parse_ascii_i16(required(1, "car_id")?, "car_id")?,
            start_date_pg_us: parse_pg_timestamp_us(required(2, "start_date")?)?,
            end_date_pg_us: fields[3]
                .as_deref()
                .map(parse_pg_timestamp_us)
                .transpose()?,
            version: fields[4]
                .as_deref()
                .map(|value| {
                    String::from_utf8(value.to_vec())
                        .map_err(|_| reject("version is not exact UTF-8"))
                })
                .transpose()?,
        });
    }
    if rows.iter().any(|row| row.car_id != PINNED_SELECTED_CAR_ID) {
        return Err(reject("fixture contains a non-selected car"));
    }
    Ok(ParsedUpdatesFixture {
        selected_car_id: PINNED_SELECTED_CAR_ID,
        rows,
        sql_sha256,
    })
}

/// PostgreSQL COPY CSV field split. Only a raw `\\N` token is NULL.
pub fn copy_fields(line: &[u8]) -> Result<Vec<Option<Vec<u8>>>, UpdatesDeliveryError> {
    let mut fields = Vec::new();
    let mut raw = Vec::new();
    let mut value = Vec::new();
    let mut quoted = false;
    let mut index = 0;
    while index < line.len() {
        match line[index] {
            b'"' => {
                raw.push(b'"');
                if quoted && index + 1 < line.len() && line[index + 1] == b'"' {
                    value.push(b'"');
                    raw.push(b'"');
                    index += 1;
                } else {
                    quoted = !quoted;
                }
            }
            b',' if !quoted => {
                fields.push(finish_copy_field(&raw, &value));
                raw.clear();
                value.clear();
            }
            b'\\' => {
                raw.push(b'\\');
                if let Some(&escaped) = line.get(index + 1) {
                    raw.push(escaped);
                    value.push(match escaped {
                        b'\\' => b'\\',
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        other => other,
                    });
                    index += 1;
                } else {
                    value.push(b'\\');
                }
            }
            byte => {
                raw.push(byte);
                value.push(byte);
            }
        }
        index += 1;
    }
    if quoted {
        return Err(reject("unterminated COPY quote"));
    }
    fields.push(finish_copy_field(&raw, &value));
    Ok(fields)
}

fn finish_copy_field(raw: &[u8], value: &[u8]) -> Option<Vec<u8>> {
    if raw == b"\\N" {
        None
    } else {
        Some(value.to_vec())
    }
}

fn parse_ascii_i32(raw: &[u8], label: &str) -> Result<i32, UpdatesDeliveryError> {
    std::str::from_utf8(raw)
        .map_err(|_| reject(format!("{label} is not UTF-8")))?
        .parse()
        .map_err(|_| reject(format!("{label} is not a signed i32")))
}

fn parse_ascii_i16(raw: &[u8], label: &str) -> Result<i16, UpdatesDeliveryError> {
    std::str::from_utf8(raw)
        .map_err(|_| reject(format!("{label} is not UTF-8")))?
        .parse()
        .map_err(|_| reject(format!("{label} is not a signed i16")))
}

/// Decode a PostgreSQL `timestamp without time zone` text value as microseconds
/// since 2000-01-01 00:00:00, preserving `±infinity` sentinels.
pub fn parse_pg_timestamp_us(raw: &[u8]) -> Result<i64, UpdatesDeliveryError> {
    match raw {
        b"-infinity" => Ok(i64::MIN),
        b"infinity" => Ok(i64::MAX),
        _ => {
            let text = std::str::from_utf8(raw).map_err(|_| reject("timestamp is not UTF-8"))?;
            parse_finite_pg_timestamp_us(text)
        }
    }
}

fn parse_finite_pg_timestamp_us(text: &str) -> Result<i64, UpdatesDeliveryError> {
    let (date, time) = text
        .split_once(' ')
        .ok_or_else(|| reject("timestamp is missing the date/time separator"))?;
    let mut date_parts = date.split('-');
    let year: i32 = parse_time_part(date_parts.next(), "year")?;
    let month: u32 = parse_time_part(date_parts.next(), "month")?;
    let day: u32 = parse_time_part(date_parts.next(), "day")?;
    if date_parts.next().is_some() {
        return Err(reject("timestamp date has extra fields"));
    }
    let (hms, fraction) = match time.split_once('.') {
        Some((hms, fraction)) => (hms, Some(fraction)),
        None => (time, None),
    };
    let mut time_parts = hms.split(':');
    let hour: u32 = parse_time_part(time_parts.next(), "hour")?;
    let minute: u32 = parse_time_part(time_parts.next(), "minute")?;
    let second: u32 = parse_time_part(time_parts.next(), "second")?;
    if time_parts.next().is_some() {
        return Err(reject("timestamp time has extra fields"));
    }
    let micros = match fraction {
        None => 0,
        Some(digits) => {
            if digits.is_empty()
                || digits.len() > 6
                || !digits.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(reject("timestamp fraction is not 1..6 digits"));
            }
            let padded = format!("{digits:0<6}");
            padded
                .parse::<i64>()
                .map_err(|_| reject("timestamp fraction is out of range"))?
        }
    };
    if hour > 23 || minute > 59 || second > 59 {
        return Err(reject("timestamp time is out of range"));
    }
    let days = days_from_civil(year, month, day)? - days_from_civil(2000, 1, 1)?;
    let seconds = days
        .checked_mul(86_400)
        .and_then(|value| value.checked_add(i64::from(hour) * 3_600))
        .and_then(|value| value.checked_add(i64::from(minute) * 60))
        .and_then(|value| value.checked_add(i64::from(second)))
        .ok_or_else(|| reject("timestamp overflowed PostgreSQL seconds"))?;
    seconds
        .checked_mul(1_000_000)
        .and_then(|value| value.checked_add(micros))
        .ok_or_else(|| reject("timestamp overflowed PostgreSQL microseconds"))
}

fn parse_time_part<T: std::str::FromStr>(
    raw: Option<&str>,
    label: &str,
) -> Result<T, UpdatesDeliveryError> {
    raw.ok_or_else(|| reject(format!("timestamp {label} is missing")))?
        .parse()
        .map_err(|_| reject(format!("timestamp {label} is not an integer")))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> Result<i64, UpdatesDeliveryError> {
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return Err(reject("timestamp date is out of range"));
    }
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_shifted = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let day_of_year = (153 * month_shifted + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Ok(era * 146_097 + day_of_era - 719_468)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Build the schema-2.2 snapshot used by the pinned selected-car updates path.
pub fn updates_snapshot_v2_2(rows: Vec<TeslaMateUpdatePhysicalV2_2>) -> ProjectionSnapshotV2_2 {
    ProjectionSnapshotV2_2 {
        global_settings: vec![ProjectionGlobalSettingsV2_2 {
            id: 1,
            unit_of_length: ProjectionUnitOfLengthV2_2::Kilometers,
            unit_of_temperature: ProjectionUnitOfTemperatureV2_2::Celsius,
            unit_of_pressure: ProjectionUnitOfPressureV2_2::Bar,
            preferred_range: ProjectionPreferredRangeV2_2::Rated,
            base_url: None,
            grafana_url: None,
            language: String::new(),
            theme_mode: "system".into(),
            inserted_at_pg_us: 0,
            updated_at_pg_us: 0,
        }],
        cars: vec![ProjectionCarV2_2 {
            id: PINNED_SELECTED_CAR_ID,
            eid: 1,
            vid: 1,
            vin: None,
            name: None,
            model: None,
            efficiency: None,
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            display_priority: 0,
            inserted_at_pg_us: 0,
            updated_at_pg_us: 0,
            settings_id: 1,
        }],
        car_settings: vec![ProjectionCarSettingsV2_2 {
            id: 1,
            suspend_min: 0,
            suspend_after_idle_min: 0,
            req_not_unlocked: false,
            free_supercharging: false,
            use_streaming_api: false,
            enabled: true,
            lfp_battery: false,
        }],
        addresses: vec![],
        geofences: vec![],
        drives: vec![],
        positions: vec![],
        charging_processes: vec![],
        charges: vec![],
        states: vec![],
        updates: rows.into_iter().map(Into::into).collect(),
    }
}

pub fn pinned_updates_binding() -> ProjectionBinding {
    ProjectionBinding {
        installation_id: parse_uuid(FIXED_INSTALLATION_ID),
        account_id: parse_uuid(FIXED_ACCOUNT_ID),
        vehicle_id: parse_uuid(FIXED_VEHICLE_ID),
        generation: 1,
        selected_car_id: i64::from(PINNED_SELECTED_CAR_ID),
    }
}

fn parse_uuid(value: &str) -> Uuid {
    Uuid::parse_str(value).expect("fixed delivery UUID")
}

/// Write one locally verified schema-2.2 pack from already-decoded update rows.
pub fn write_updates_schema_22_pack(
    packs_dir: impl Into<PathBuf>,
    rows: Vec<TeslaMateUpdatePhysicalV2_2>,
) -> Result<(BuiltProjectionPack, ProjectionSnapshotV2_2), UpdatesDeliveryError> {
    let snapshot = updates_snapshot_v2_2(rows);
    let request = updates_pack_request(&snapshot);
    let built = ProjectionPackWriter::new(packs_dir).write_full_snapshot_2_2(&request)?;
    Ok((built, snapshot))
}

pub fn updates_pack_request<'a>(
    snapshot: &'a ProjectionSnapshotV2_2,
) -> ProjectionPackRequestV2_2<'a> {
    ProjectionPackRequestV2_2 {
        pack_id: parse_uuid(FIXED_PACK_ID),
        snapshot_id: parse_uuid(FIXED_SNAPSHOT_ID),
        ordinal: 0,
        binding: pinned_updates_binding(),
        sequence: SequenceRange {
            from_exclusive: 0,
            to_inclusive: 0,
        },
        snapshot,
    }
}

/// Sign a schema-2.2 full-snapshot manifest from an already-built pack.
pub fn sign_updates_schema_22_manifest(
    request: &ProjectionPackRequestV2_2<'_>,
    built: &BuiltProjectionPack,
    cursor_key: &CursorKey,
) -> Result<SyncManifest, UpdatesDeliveryError> {
    request
        .signed_manifest(built, cursor_key)
        .map_err(Into::into)
}

/// Sign a no-op state bound to the published snapshot. This is not a delta.
pub fn sign_updates_schema_22_noop(
    binding: &ProjectionBinding,
    snapshot_id: Uuid,
    head_sequence: u64,
    pack_sha256: &str,
    cursor_key: &CursorKey,
) -> Result<SignedNoOpState, UpdatesDeliveryError> {
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        crate::protocol::CursorClaims {
            protocol: crate::protocol::PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V3,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: head_sequence,
        },
    )
    .map_err(|error| reject(error.to_string()))?;
    Ok(SignedNoOpState {
        schema: "teslatlas-hub-schema-22-noop-v1".into(),
        projection_schema: "2.2".into(),
        installation_id: binding.installation_id,
        account_id: binding.account_id,
        vehicle_id: binding.vehicle_id,
        generation: binding.generation,
        snapshot_id,
        head_sequence,
        pack_sha256: pack_sha256.to_owned(),
        terminal_cursor,
        source_witness: None,
    })
}

/// Read Hub schema-2.2 `updates` rows from an already-built pack file.
pub fn read_hub_updates_from_pack(
    pack: &TransportPack,
    pack_path: &Path,
) -> Result<Vec<TeslaMateUpdatePhysicalV2_2>, UpdatesDeliveryError> {
    let (_directory, path) = private_sqlite_tempfile_from_pack(pack, pack_path)?;
    read_hub_updates_from_sqlite_path(&path, pack.row_count)
}

pub fn read_hub_updates_from_sqlite(
    sqlite: &[u8],
) -> Result<Vec<TeslaMateUpdatePhysicalV2_2>, UpdatesDeliveryError> {
    if u64::try_from(sqlite.len()).unwrap_or(u64::MAX)
        > ProtocolLimits::default().max_uncompressed_pack_bytes
    {
        return Err(reject("reopened SQLite pack exceeds its fixed byte bound"));
    }
    let (_directory, path) = private_sqlite_tempfile(sqlite)?;
    read_hub_updates_from_sqlite_path(&path, ProtocolLimits::default().max_rows_per_pack)
}

fn read_hub_updates_from_sqlite_path(
    path: &Path,
    maximum_rows: u64,
) -> Result<Vec<TeslaMateUpdatePhysicalV2_2>, UpdatesDeliveryError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| reject(error.to_string()))?;
    let mut statement = connection
        .prepare(
            "SELECT id, car_id, start_date_pg_us, end_date_pg_us, version
             FROM updates
            ORDER BY start_date_pg_us, id LIMIT ?1",
        )
        .map_err(|error| reject(error.to_string()))?;
    let query_limit = maximum_rows
        .checked_add(1)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| reject("reopened SQLite row bound is invalid"))?;
    let rows = statement
        .query_map([query_limit], |row| {
            Ok(TeslaMateUpdatePhysicalV2_2 {
                id: row.get(0)?,
                car_id: row.get(1)?,
                start_date_pg_us: row.get(2)?,
                end_date_pg_us: row.get(3)?,
                version: row.get(4)?,
            })
        })
        .map_err(|error| reject(error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| reject(error.to_string()))?;
    if u64::try_from(rows.len()).unwrap_or(u64::MAX) > maximum_rows {
        return Err(reject("reopened SQLite pack exceeds its fixed row bound"));
    }
    drop(statement);
    drop(connection);
    Ok(rows)
}

fn exactly_one_capture_row<T>(
    mut rows: Vec<T>,
    table: &'static str,
) -> Result<T, UpdatesDeliveryError> {
    if rows.len() != 1 {
        return Err(reject(format!(
            "reopened production pack must contain exactly one {table} row"
        )));
    }
    Ok(rows.pop().expect("length checked"))
}

fn read_hub_production_capture_from_pack(
    pack: &TransportPack,
    pack_path: &Path,
) -> Result<ProductionUpdatesCaptureProof, UpdatesDeliveryError> {
    let (_directory, path) = private_sqlite_tempfile_from_pack(pack, pack_path)?;
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| reject(error.to_string()))?;

    let global_settings = {
        let mut statement = connection
            .prepare(
                "SELECT id, unit_of_length, unit_of_temperature, unit_of_pressure,
                        preferred_range, base_url, grafana_url, language, theme_mode,
                        inserted_at_pg_us, updated_at_pg_us
                 FROM global_settings ORDER BY id",
            )
            .map_err(|error| reject(error.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                Ok(ProductionGlobalSettingsCapture {
                    id: row.get(0)?,
                    unit_of_length: row.get(1)?,
                    unit_of_temperature: row.get(2)?,
                    unit_of_pressure: row.get(3)?,
                    preferred_range: row.get(4)?,
                    base_url: row.get(5)?,
                    grafana_url: row.get(6)?,
                    language: row.get(7)?,
                    theme_mode: row.get(8)?,
                    inserted_at_pg_us: row.get(9)?,
                    updated_at_pg_us: row.get(10)?,
                })
            })
            .map_err(|error| reject(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| reject(error.to_string()))?;
        exactly_one_capture_row(rows, "global_settings")?
    };
    let car = {
        let mut statement = connection
            .prepare(
                "SELECT id, eid, vid, vin, name, model, efficiency, trim_badging,
                        marketing_name, exterior_color, wheel_type, spoiler_type,
                        display_priority, inserted_at_pg_us, updated_at_pg_us, settings_id
                 FROM cars ORDER BY id",
            )
            .map_err(|error| reject(error.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                Ok(ProductionCarCapture {
                    id: row.get(0)?,
                    eid: row.get(1)?,
                    vid: row.get(2)?,
                    vin: row.get(3)?,
                    name: row.get(4)?,
                    model: row.get(5)?,
                    efficiency_bits_be: row.get(6)?,
                    trim_badging: row.get(7)?,
                    marketing_name: row.get(8)?,
                    exterior_color: row.get(9)?,
                    wheel_type: row.get(10)?,
                    spoiler_type: row.get(11)?,
                    display_priority: row.get(12)?,
                    inserted_at_pg_us: row.get(13)?,
                    updated_at_pg_us: row.get(14)?,
                    settings_id: row.get(15)?,
                })
            })
            .map_err(|error| reject(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| reject(error.to_string()))?;
        let row = exactly_one_capture_row(rows, "cars")?;
        if row
            .efficiency_bits_be
            .as_ref()
            .is_some_and(|value| value.len() != 8)
        {
            return Err(reject(
                "reopened production pack car efficiency is not an exact FLOAT8 payload",
            ));
        }
        row
    };
    let car_settings = {
        let mut statement = connection
            .prepare(
                "SELECT id, suspend_min, suspend_after_idle_min, req_not_unlocked,
                        free_supercharging, use_streaming_api, enabled, lfp_battery
                 FROM car_settings ORDER BY id",
            )
            .map_err(|error| reject(error.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                Ok(ProductionCarSettingsCapture {
                    id: row.get(0)?,
                    suspend_min: row.get(1)?,
                    suspend_after_idle_min: row.get(2)?,
                    req_not_unlocked: row.get(3)?,
                    free_supercharging: row.get(4)?,
                    use_streaming_api: row.get(5)?,
                    enabled: row.get(6)?,
                    lfp_battery: row.get(7)?,
                })
            })
            .map_err(|error| reject(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| reject(error.to_string()))?;
        exactly_one_capture_row(rows, "car_settings")?
    };
    let updates = {
        let mut statement = connection
            .prepare(
                "SELECT id, car_id, start_date_pg_us, end_date_pg_us, version
                 FROM updates ORDER BY start_date_pg_us, id LIMIT ?1",
            )
            .map_err(|error| reject(error.to_string()))?;
        statement
            .query_map(
                [i64::try_from(pack.row_count.saturating_add(1))
                    .map_err(|_| reject("reopened production row bound is invalid"))?],
                |row| {
                    Ok(TeslaMateUpdatePhysicalV2_2 {
                        id: row.get(0)?,
                        car_id: row.get(1)?,
                        start_date_pg_us: row.get(2)?,
                        end_date_pg_us: row.get(3)?,
                        version: row.get(4)?,
                    })
                },
            )
            .map_err(|error| reject(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| reject(error.to_string()))?
    };
    if u64::try_from(updates.len()).unwrap_or(u64::MAX) > pack.row_count {
        return Err(reject("reopened production pack exceeds its row bound"));
    }
    Ok(ProductionUpdatesCaptureProof {
        global_settings,
        car,
        car_settings,
        updates,
    })
}

fn private_sqlite_tempfile(
    sqlite: &[u8],
) -> Result<(PrivateTempDir, PathBuf), UpdatesDeliveryError> {
    let directory = PrivateTempDir::create()?;
    let path = directory.create_file("hub-updates.sqlite")?;
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .map_err(|error| reject(error.to_string()))?;
    file.write_all(sqlite)
        .and_then(|()| file.sync_all())
        .map_err(|error| reject(error.to_string()))?;
    drop(file);
    Ok((directory, path))
}

fn private_sqlite_tempfile_from_pack(
    pack: &TransportPack,
    pack_path: &Path,
) -> Result<(PrivateTempDir, PathBuf), UpdatesDeliveryError> {
    let limits = ProtocolLimits::default();
    pack.validate(limits)
        .map_err(|error| reject(error.to_string()))?;
    let source_fd = open(
        pack_path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| reject("cannot securely open candidate pack"))?;
    let source_stat = fstat(&source_fd).map_err(|_| reject("cannot inspect candidate pack"))?;
    if !FileType::from_raw_mode(source_stat.st_mode).is_file()
        || source_stat.st_size < 0
        || u64::try_from(source_stat.st_size).ok() != Some(pack.compressed_bytes)
    {
        return Err(reject(
            "candidate pack size or file type mismatches its descriptor",
        ));
    }

    let directory = PrivateTempDir::create()?;
    let compressed_path = directory.create_file("candidate.sqlite.zst")?;
    let mut source = File::from(source_fd).take(
        pack.compressed_bytes
            .checked_add(1)
            .ok_or_else(|| reject("candidate compressed byte bound overflowed"))?,
    );
    let mut compressed = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&compressed_path)
        .map_err(|error| reject(error.to_string()))?;
    let copied =
        io::copy(&mut source, &mut compressed).map_err(|error| reject(error.to_string()))?;
    if copied != pack.compressed_bytes {
        return Err(reject("candidate compressed bytes mismatch its descriptor"));
    }
    compressed
        .sync_all()
        .map_err(|error| reject(error.to_string()))?;
    drop(compressed);

    pack.verify_reader(
        File::open(&compressed_path).map_err(|error| reject(error.to_string()))?,
        limits,
    )
    .map_err(|error| reject(error.to_string()))?;

    let sqlite_path = directory.create_file("hub-updates.sqlite")?;
    let compressed = File::open(&compressed_path).map_err(|error| reject(error.to_string()))?;
    let decoder = zstd::stream::read::Decoder::new(compressed)
        .map_err(|_| reject("candidate pack decompression failed"))?;
    let mut bounded = decoder.take(
        pack.uncompressed_bytes
            .checked_add(1)
            .ok_or_else(|| reject("candidate uncompressed byte bound overflowed"))?,
    );
    let mut sqlite = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&sqlite_path)
        .map_err(|error| reject(error.to_string()))?;
    let expanded =
        io::copy(&mut bounded, &mut sqlite).map_err(|error| reject(error.to_string()))?;
    if expanded != pack.uncompressed_bytes {
        return Err(reject(
            "candidate uncompressed bytes mismatch its descriptor",
        ));
    }
    sqlite
        .sync_all()
        .map_err(|error| reject(error.to_string()))?;
    drop(sqlite);
    Ok((directory, sqlite_path))
}

fn read_bounded_pack_bytes(
    pack: &TransportPack,
    pack_path: &Path,
) -> Result<Vec<u8>, UpdatesDeliveryError> {
    let limits = ProtocolLimits::default();
    pack.validate(limits)
        .map_err(|error| reject(error.to_string()))?;
    let fd = open(
        pack_path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| reject("cannot securely open candidate pack"))?;
    let stat = fstat(&fd).map_err(|_| reject("cannot inspect candidate pack"))?;
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_size < 0
        || u64::try_from(stat.st_size).ok() != Some(pack.compressed_bytes)
    {
        return Err(reject(
            "candidate pack size or file type mismatches its descriptor",
        ));
    }
    let capacity = usize::try_from(pack.compressed_bytes)
        .map_err(|_| reject("candidate compressed byte bound exceeds this host"))?;
    let mut bytes = Vec::with_capacity(capacity);
    File::from(fd)
        .take(
            pack.compressed_bytes
                .checked_add(1)
                .ok_or_else(|| reject("candidate compressed byte bound overflowed"))?,
        )
        .read_to_end(&mut bytes)
        .map_err(|error| reject(error.to_string()))?;
    if bytes.len() != capacity {
        return Err(reject("candidate compressed bytes mismatch its descriptor"));
    }
    Ok(bytes)
}

impl PrivateTempDir {
    fn create() -> Result<Self, UpdatesDeliveryError> {
        let root = std::env::temp_dir().join(format!(
            "teslatlas-updates-hub-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder
            .create(&root)
            .map_err(|error| reject(error.to_string()))?;
        Ok(Self { path: root })
    }

    fn create_file(&self, name: &str) -> Result<PathBuf, UpdatesDeliveryError> {
        let path = self.path.join(name);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| reject(error.to_string()))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| reject(error.to_string()))?;
        drop(file);
        Ok(path)
    }
}

struct PrivateTempDir {
    path: PathBuf,
}

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn production_updates_snapshot(source: &DirectUpdatesSourceV2_2) -> ProjectionSnapshotV2_2 {
    ProjectionSnapshotV2_2 {
        global_settings: vec![source.global_settings.clone().into()],
        cars: vec![source.car.clone().into()],
        car_settings: vec![source.car_settings.clone().into()],
        addresses: vec![],
        geofences: vec![],
        drives: vec![],
        positions: vec![],
        charging_processes: vec![],
        charges: vec![],
        states: vec![],
        updates: source.updates.iter().cloned().map(Into::into).collect(),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ProductionGlobalSettingsCapture {
    id: i64,
    unit_of_length: String,
    unit_of_temperature: String,
    unit_of_pressure: String,
    preferred_range: String,
    base_url: Option<String>,
    grafana_url: Option<String>,
    language: String,
    theme_mode: String,
    inserted_at_pg_us: i64,
    updated_at_pg_us: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ProductionCarCapture {
    id: i16,
    eid: i64,
    vid: i64,
    vin: Option<String>,
    name: Option<String>,
    model: Option<String>,
    efficiency_bits_be: Option<Vec<u8>>,
    trim_badging: Option<String>,
    marketing_name: Option<String>,
    exterior_color: Option<String>,
    wheel_type: Option<String>,
    spoiler_type: Option<String>,
    display_priority: i16,
    inserted_at_pg_us: i64,
    updated_at_pg_us: i64,
    settings_id: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ProductionCarSettingsCapture {
    id: i64,
    suspend_min: i32,
    suspend_after_idle_min: i32,
    req_not_unlocked: bool,
    free_supercharging: bool,
    use_streaming_api: bool,
    enabled: bool,
    lfp_battery: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ProductionUpdatesCaptureProof {
    global_settings: ProductionGlobalSettingsCapture,
    car: ProductionCarCapture,
    car_settings: ProductionCarSettingsCapture,
    updates: Vec<TeslaMateUpdatePhysicalV2_2>,
}

fn production_updates_capture_proof(
    source: &DirectUpdatesSourceV2_2,
) -> ProductionUpdatesCaptureProof {
    let mut updates = source.updates.clone();
    updates.sort_by(|left, right| {
        left.start_date_pg_us
            .cmp(&right.start_date_pg_us)
            .then(left.id.cmp(&right.id))
    });
    ProductionUpdatesCaptureProof {
        global_settings: ProductionGlobalSettingsCapture {
            id: source.global_settings.id,
            unit_of_length: source.global_settings.unit_of_length.as_str().into(),
            unit_of_temperature: source.global_settings.unit_of_temperature.as_str().into(),
            unit_of_pressure: source.global_settings.unit_of_pressure.as_str().into(),
            preferred_range: source.global_settings.preferred_range.as_str().into(),
            base_url: source.global_settings.base_url.clone(),
            grafana_url: source.global_settings.grafana_url.clone(),
            language: source.global_settings.language.clone(),
            theme_mode: source.global_settings.theme_mode.clone(),
            inserted_at_pg_us: source.global_settings.inserted_at_pg_us,
            updated_at_pg_us: source.global_settings.updated_at_pg_us,
        },
        car: ProductionCarCapture {
            id: source.car.id,
            eid: source.car.eid,
            vid: source.car.vid,
            vin: source.car.vin.clone(),
            name: source.car.name.clone(),
            model: source.car.model.clone(),
            efficiency_bits_be: source
                .car
                .efficiency
                .map(|value| value.to_bits().to_be_bytes().to_vec()),
            trim_badging: source.car.trim_badging.clone(),
            marketing_name: source.car.marketing_name.clone(),
            exterior_color: source.car.exterior_color.clone(),
            wheel_type: source.car.wheel_type.clone(),
            spoiler_type: source.car.spoiler_type.clone(),
            display_priority: source.car.display_priority,
            inserted_at_pg_us: source.car.inserted_at_pg_us,
            updated_at_pg_us: source.car.updated_at_pg_us,
            settings_id: source.car.settings_id,
        },
        car_settings: ProductionCarSettingsCapture {
            id: source.car_settings.id,
            suspend_min: source.car_settings.suspend_min,
            suspend_after_idle_min: source.car_settings.suspend_after_idle_min,
            req_not_unlocked: source.car_settings.req_not_unlocked,
            free_supercharging: source.car_settings.free_supercharging,
            use_streaming_api: source.car_settings.use_streaming_api,
            enabled: source.car_settings.enabled,
            lfp_battery: source.car_settings.lfp_battery,
        },
        updates,
    }
}

fn production_source_capture_sha256(
    capture: &ProductionUpdatesCaptureProof,
) -> Result<String, UpdatesDeliveryError> {
    let canonical = serde_json::to_vec(capture).map_err(|error| reject(error.to_string()))?;
    let mut bytes = b"teslatlas-hub/production-source-capture/v2\n".to_vec();
    bytes.extend_from_slice(&canonical);
    Ok(hex_sha256(&bytes))
}

fn production_car_identity_sha256(
    car: &ProductionCarCapture,
) -> Result<String, UpdatesDeliveryError> {
    let canonical = serde_json::to_vec(&(car.eid, car.vid, car.vin.as_deref()))
        .map_err(|error| reject(error.to_string()))?;
    let mut bytes = b"teslatlas-hub/teslamate-car-identity/v1\n".to_vec();
    bytes.extend_from_slice(&canonical);
    Ok(hex_sha256(&bytes))
}

fn production_source_witness(
    source_capture: &DirectUpdatesSourceV2_2,
    source_proof: &ProductionUpdatesCaptureProof,
    source_capture_sha256: String,
    source: &LogicalUpdatesStream,
    hub: &LogicalUpdatesStream,
    noop: &SignedNoOpState,
) -> Result<ProductionUpdatesSourceWitness, UpdatesDeliveryError> {
    let observed_migration_count = u64::try_from(source_capture.schema.observed_migration_count)
        .map_err(|_| reject("observed migration count exceeds the witness domain"))?;
    Ok(ProductionUpdatesSourceWitness {
        schema: "teslatlas-pg-source-witness-v1".into(),
        selected_car_id: i64::from(source_capture.car.id),
        postgres_snapshot_sha256: source_capture.postgres_snapshot_sha256.clone(),
        source_transaction: "read_only_repeatable_read_exported_snapshot".into(),
        source_revision: source_capture.schema.pinned_source_revision.into(),
        pinned_migration_set_sha256: source_capture.schema.pinned_migration_set_sha256.into(),
        source_schema_fingerprint: source_capture.schema.fingerprint.clone(),
        observed_migration_version: source_capture.schema.observed_migration_version,
        observed_migration_count,
        source_capture_sha256,
        source_logical_sha256: source.sha256.clone(),
        source_row_count: source.summary.row_count,
        source_completed_row_count: source.summary.completed_row_count,
        source_open_row_count: source.summary.open_row_count,
        source_null_version_row_count: source.summary.null_version_row_count,
        source_empty_version_row_count: source.summary.empty_version_row_count,
        source_start_min_pg_us: source.summary.start_min_pg_us,
        source_start_max_pg_us: source.summary.start_max_pg_us,
        source_end_min_pg_us: source.summary.end_min_pg_us,
        source_end_max_pg_us: source.summary.end_max_pg_us,
        hub_logical_sha256: hub.sha256.clone(),
        hub_row_count: hub.summary.row_count,
        hub_completed_row_count: hub.summary.completed_row_count,
        hub_open_row_count: hub.summary.open_row_count,
        hub_null_version_row_count: hub.summary.null_version_row_count,
        hub_empty_version_row_count: hub.summary.empty_version_row_count,
        hub_start_min_pg_us: hub.summary.start_min_pg_us,
        hub_start_max_pg_us: hub.summary.start_max_pg_us,
        hub_end_min_pg_us: hub.summary.end_min_pg_us,
        hub_end_max_pg_us: hub.summary.end_max_pg_us,
        car_identity_sha256: production_car_identity_sha256(&source_proof.car)?,
        pack_sha256: noop.pack_sha256.clone(),
        head_sequence: noop.head_sequence,
        terminal_cursor_sha256: hex_sha256(noop.terminal_cursor.as_str().as_bytes()),
    })
}

fn sign_production_updates_schema_22_noop(
    binding: &ProjectionBinding,
    snapshot_id: Uuid,
    head_sequence: u64,
    pack_sha256: &str,
    cursor_key: &CursorKey,
    source_capture: &DirectUpdatesSourceV2_2,
    source_proof: &ProductionUpdatesCaptureProof,
    source_capture_sha256: String,
    source: &LogicalUpdatesStream,
    hub: &LogicalUpdatesStream,
) -> Result<SignedNoOpState, UpdatesDeliveryError> {
    let mut noop =
        sign_updates_schema_22_noop(binding, snapshot_id, head_sequence, pack_sha256, cursor_key)?;
    noop.source_witness = Some(production_source_witness(
        source_capture,
        source_proof,
        source_capture_sha256,
        source,
        hub,
        &noop,
    )?);
    Ok(noop)
}

fn witnesses_match_published_source(
    stored: &ProductionUpdatesSourceWitness,
    candidate: &ProductionUpdatesSourceWitness,
) -> bool {
    let mut candidate = candidate.clone();
    // PostgreSQL issues a fresh exported-snapshot token for a retry even when
    // every captured source fact is byte-identical. The stored token hash
    // witnesses the snapshot which produced the already-published bytes; all
    // content, schema, identity, Hub, sequence, and cursor facts must match.
    candidate.postgres_snapshot_sha256 = stored.postgres_snapshot_sha256.clone();
    &candidate == stored
}

fn write_verified_production_pack(
    store: &HubStore,
    request: &ProjectionPackRequestV2_2<'_>,
    source: &LogicalUpdatesStream,
    source_capture: &ProductionUpdatesCaptureProof,
) -> Result<(BuiltProjectionPack, LogicalUpdatesStream), UpdatesDeliveryError> {
    let built = ProjectionPackWriter::new(store.packs_dir()).write_full_snapshot_2_2(request)?;
    built
        .metadata
        .verify_reader(
            File::open(&built.path).map_err(|error| reject(error.to_string()))?,
            ProtocolLimits::default(),
        )
        .map_err(|error| reject(error.to_string()))?;
    let hub = match verify_reopened_production_capture(
        &built.metadata,
        &built.path,
        source,
        source_capture,
    ) {
        Ok(hub) => hub,
        Err(error) => {
            discard_unpublished_production_pack(store, &built);
            return Err(error);
        }
    };
    Ok((built, hub))
}

fn verify_reopened_production_capture(
    pack: &TransportPack,
    pack_path: &Path,
    source: &LogicalUpdatesStream,
    source_capture: &ProductionUpdatesCaptureProof,
) -> Result<LogicalUpdatesStream, UpdatesDeliveryError> {
    let hub_capture = read_hub_production_capture_from_pack(pack, pack_path)?;
    if &hub_capture != source_capture {
        return Err(reject(
            "reopened production Hub roots or updates do not exactly match the exported source snapshot",
        ));
    }
    let hub = encode_updates_logical_stream(&hub_capture.updates)?;
    if source.sha256 != hub.sha256 || source.rows != hub.rows || source.summary != hub.summary {
        return Err(reject(
            "production Hub updates do not exactly match the exported source snapshot",
        ));
    }
    Ok(hub)
}

fn discard_unpublished_production_pack(store: &HubStore, built: &BuiltProjectionPack) {
    if !built.may_remove_unpublished_file() {
        return;
    }
    if matches!(
        store.pack_sha256_is_retained(&built.metadata.sha256.to_string()),
        Ok(true)
    ) {
        return;
    }
    if let Err(error) = fs::remove_file(&built.path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %built.path.display(), %error, "could not remove unpublished schema-2.2 pack");
    }
}

fn production_publication_report(
    source_capture: &DirectUpdatesSourceV2_2,
    source_capture_sha256: String,
    source: &LogicalUpdatesStream,
    hub: &LogicalUpdatesStream,
    manifest: &SyncManifest,
    noop: &SignedNoOpState,
    noop_bytes: &[u8],
    reused_current_snapshot: bool,
) -> Result<ProductionUpdatesPublication, UpdatesDeliveryError> {
    let manifest_bytes = serde_json::to_vec(manifest).map_err(|error| reject(error.to_string()))?;
    let pack = manifest
        .chunks
        .first()
        .ok_or_else(|| reject("production schema-2.2 manifest has no pack"))?;
    let source_witness = noop
        .source_witness
        .clone()
        .ok_or_else(|| reject("production schema-2.2 no-op has no source witness"))?;
    Ok(ProductionUpdatesPublication {
        state: if reused_current_snapshot {
            "schema_2_2_already_current".into()
        } else {
            "schema_2_2_published_after_legacy_commit".into()
        },
        selected_car_id: i64::from(source_capture.car.id),
        vehicle_id: manifest.vehicle_id,
        snapshot_id: manifest.snapshot_id,
        sequence: manifest.head_sequence,
        reused_current_snapshot,
        source_revision: source_capture.schema.pinned_source_revision.into(),
        source_schema_fingerprint: source_capture.schema.fingerprint.clone(),
        observed_migration_version: source_capture.schema.observed_migration_version,
        observed_migration_count: source_capture.schema.observed_migration_count,
        source_logical_sha256: source.sha256.clone(),
        source_capture_sha256,
        hub_logical_sha256: hub.sha256.clone(),
        source_summary: source.summary.clone(),
        hub_summary: hub.summary.clone(),
        manifest_sha256: hex_sha256(&manifest_bytes),
        pack_sha256: pack.sha256.to_string(),
        noop_sha256: hex_sha256(noop_bytes),
        source_witness,
    })
}

/// Publish the exact physical updates captured by the production direct
/// importer. PostgreSQL is already closed when this runs. Therefore the
/// legacy catalogue commit and this schema-2.2 commit are deliberately two
/// transactions; any error must be reported as a retryable partial state by
/// the caller.
#[cfg(test)]
fn publish_production_updates_schema_22(
    store: &HubStore,
    cursor_key: &CursorKey,
    binding: &ProjectionBinding,
    source_capture: DirectUpdatesSourceV2_2,
) -> Result<ProductionUpdatesPublication, UpdatesDeliveryError> {
    let publication_gate = store.try_acquire_publication_gate()?;
    let expected_head = production_updates_head(store, binding.vehicle_id)?;
    publish_production_updates_schema_22_with_gate(
        store,
        cursor_key,
        binding,
        source_capture,
        &publication_gate,
        &expected_head,
        None,
    )
}

pub(crate) fn publish_production_updates_schema_22_with_gate(
    store: &HubStore,
    cursor_key: &CursorKey,
    binding: &ProjectionBinding,
    source_capture: DirectUpdatesSourceV2_2,
    publication_gate: &PublicationGate,
    expected_head: &Option<ProductionUpdatesHead>,
    admitted_legacy_head: Option<(Uuid, u64)>,
) -> Result<ProductionUpdatesPublication, UpdatesDeliveryError> {
    if source_capture.schema.pinned_source_revision != TESLAMATE_V4_SOURCE_REVISION
        || source_capture.schema.pinned_migration_set_sha256 != TESLAMATE_V4_MIGRATION_SET_SHA256
        || source_capture.schema.observed_migration_count != TESLAMATE_V4_MIGRATION_COUNT
        || source_capture.schema.observed_migration_version != MAX_VALIDATED_MIGRATION
    {
        return Err(reject(
            "production schema-2.2 source capture contradicts its pinned TeslaMate v4 schema facts",
        ));
    }
    if !is_canonical_sha256(&source_capture.postgres_snapshot_sha256)
        || !is_canonical_sha256(&source_capture.schema.fingerprint)
        || binding.selected_car_id <= 0
        || i64::from(source_capture.car.id) != binding.selected_car_id
        || source_capture
            .updates
            .iter()
            .any(|row| i64::from(row.car_id) != binding.selected_car_id)
    {
        return Err(reject(
            "production schema-2.2 source capture does not match its pinned selected-car binding",
        ));
    }
    let source = encode_updates_logical_stream(&source_capture.updates)?;
    let source_proof = production_updates_capture_proof(&source_capture);
    let source_capture_sha256 = production_source_capture_sha256(&source_proof)?;
    let snapshot = production_updates_snapshot(&source_capture);
    if &production_updates_head(store, binding.vehicle_id)? != expected_head {
        return Err(reject(
            "schema-2.2 publication head changed after the source snapshot was captured",
        ));
    }

    let admitted_legacy_is_current =
        match admitted_legacy_head {
            Some((base_snapshot_id, head_sequence)) => store
                .v2_head(binding.vehicle_id)?
                .is_some_and(|(stored_base, stored_head, _)| {
                    stored_base == base_snapshot_id
                        && u64::try_from(stored_head).ok() == Some(head_sequence)
                }),
            None => false,
        };
    let current_schema_22 = match store.manifest_for_vehicle(binding.vehicle_id)? {
        Some(manifest) if manifest.schema == HUB_PROJECTION_SCHEMA_V3 => Some(manifest),
        Some(manifest)
            if manifest.schema == HUB_PROJECTION_SCHEMA_V2
                && admitted_legacy_is_current
                && admitted_legacy_head.is_some_and(|(base_snapshot_id, _)| {
                    manifest.snapshot_id == base_snapshot_id
                })
                && manifest.installation_id == binding.installation_id
                && manifest.account_id == binding.account_id
                && manifest.vehicle_id == binding.vehicle_id
                && manifest.generation == binding.generation =>
        {
            None
        }
        Some(_) => {
            return Err(reject(
                "production schema-2.2 publication found an unadmitted other-schema head",
            ));
        }
        None => None,
    };
    if current_schema_22.as_ref().is_some_and(|current| {
        current.installation_id != binding.installation_id
            || current.account_id != binding.account_id
            || current.vehicle_id != binding.vehicle_id
            || current.generation != binding.generation
    }) {
        return Err(reject(
            "production schema-2.2 successor changed the existing Hub identity or generation",
        ));
    }
    if current_schema_22
        .as_ref()
        .is_some_and(|current| !matches!(current.chunks.as_slice(), [_]))
    {
        return Err(reject(
            "existing production schema-2.2 manifest is not a single full-snapshot pack",
        ));
    }
    if let Some(current) = current_schema_22.as_ref() {
        let stored_noop_bytes = store
            .schema_22_noop_for_snapshot(binding.vehicle_id, current.snapshot_id)?
            .ok_or_else(|| reject("existing production schema-2.2 no-op is missing"))?;
        let stored_noop: SignedNoOpState = serde_json::from_slice(&stored_noop_bytes)
            .map_err(|error| reject(error.to_string()))?;
        if serde_json::to_vec(&stored_noop).map_err(|error| reject(error.to_string()))?
            != stored_noop_bytes
        {
            return Err(reject(
                "existing production schema-2.2 no-op is not canonical typed JSON",
            ));
        }
        validate_schema_22_pair(current, &stored_noop)?;
        validate_schema_22_cursor_key(current, cursor_key)?;
        if stored_noop
            .source_witness
            .as_ref()
            .is_none_or(|witness| witness.selected_car_id != binding.selected_car_id)
        {
            return Err(reject(
                "production schema-2.2 successor changed the stored selected-car witness",
            ));
        }
    }

    if let Some(current) = current_schema_22
        && let [pack] = current.chunks.as_slice()
    {
        let current_request = ProjectionPackRequestV2_2 {
            pack_id: pack.pack_id,
            snapshot_id: current.snapshot_id,
            ordinal: pack.ordinal,
            binding: binding.clone(),
            sequence: pack.sequence,
            snapshot: &snapshot,
        };
        let (built, hub) =
            write_verified_production_pack(store, &current_request, &source, &source_proof)?;
        let candidate_manifest =
            sign_updates_schema_22_manifest(&current_request, &built, cursor_key)?;
        let candidate_noop = sign_production_updates_schema_22_noop(
            binding,
            current.snapshot_id,
            current.head_sequence,
            &built.metadata.sha256.to_string(),
            cursor_key,
            &source_capture,
            &source_proof,
            source_capture_sha256.clone(),
            &source,
            &hub,
        )?;
        let stored_noop =
            store.schema_22_noop_for_snapshot(binding.vehicle_id, current.snapshot_id)?;
        if candidate_manifest == current
            && built.metadata.sha256 == pack.sha256
            && let Some(stored_noop_bytes) = stored_noop.as_deref()
        {
            let stored_noop: SignedNoOpState = serde_json::from_slice(stored_noop_bytes)
                .map_err(|error| reject(error.to_string()))?;
            let canonical_stored_noop =
                serde_json::to_vec(&stored_noop).map_err(|error| reject(error.to_string()))?;
            validate_schema_22_pair(&current, &stored_noop)?;
            if canonical_stored_noop == stored_noop_bytes
                && stored_noop
                    .source_witness
                    .as_ref()
                    .zip(candidate_noop.source_witness.as_ref())
                    .is_some_and(|(stored, candidate)| {
                        witnesses_match_published_source(stored, candidate)
                    })
            {
                return production_publication_report(
                    &source_capture,
                    source_capture_sha256,
                    &source,
                    &hub,
                    &current,
                    &stored_noop,
                    stored_noop_bytes,
                    true,
                );
            }
        }
        discard_unpublished_production_pack(store, &built);
    }

    let sequence =
        store.reserve_next_full_snapshot_sequence(publication_gate, binding.vehicle_id)?;
    let snapshot_id = Uuid::new_v4();
    let request = ProjectionPackRequestV2_2 {
        pack_id: Uuid::new_v4(),
        snapshot_id,
        ordinal: 0,
        binding: binding.clone(),
        sequence: SequenceRange {
            from_exclusive: sequence,
            to_inclusive: sequence,
        },
        snapshot: &snapshot,
    };
    let (built, hub) = write_verified_production_pack(store, &request, &source, &source_proof)?;
    let manifest = sign_updates_schema_22_manifest(&request, &built, cursor_key)?;
    let noop = sign_production_updates_schema_22_noop(
        binding,
        snapshot_id,
        sequence,
        &built.metadata.sha256.to_string(),
        cursor_key,
        &source_capture,
        &source_proof,
        source_capture_sha256.clone(),
        &source,
        &hub,
    )?;
    let noop_bytes = serde_json::to_vec(&noop).map_err(|error| reject(error.to_string()))?;
    if let Err(error) =
        publish_updates_schema_22_with_gate(store, publication_gate, &manifest, &noop)
    {
        discard_unpublished_production_pack(store, &built);
        return Err(error);
    }
    production_publication_report(
        &source_capture,
        source_capture_sha256,
        &source,
        &hub,
        &manifest,
        &noop,
        &noop_bytes,
        false,
    )
}

/// Catalogue a signed schema-2.2 full snapshot and its signed no-op.
pub fn publish_updates_schema_22(
    store: &HubStore,
    manifest: &SyncManifest,
    noop: &SignedNoOpState,
) -> Result<(), UpdatesDeliveryError> {
    let publication_gate = store.try_acquire_publication_gate()?;
    publish_updates_schema_22_with_gate(store, &publication_gate, manifest, noop)
}

fn publish_updates_schema_22_with_gate(
    store: &HubStore,
    publication_gate: &PublicationGate,
    manifest: &SyncManifest,
    noop: &SignedNoOpState,
) -> Result<(), UpdatesDeliveryError> {
    validate_schema_22_pair(manifest, noop)?;
    let current_snapshot_id = store
        .manifest_for_vehicle(noop.vehicle_id)?
        .filter(|current| current.schema == HUB_PROJECTION_SCHEMA_V3)
        .map(|current| current.snapshot_id);
    store.prepare_schema_22_noop_publication(
        publication_gate,
        noop.vehicle_id,
        current_snapshot_id,
    )?;
    // The immutable no-op is durable before its manifest becomes current.
    // Snapshot-keyed sidecars keep the prior manifest servable if this
    // process stops before or during the SQLite catalogue transaction.
    store.publish_schema_22_noop(publication_gate, noop)?;
    store.publish_schema_22_manifest(publication_gate, manifest)?;
    Ok(())
}

/// Drive the shipped source → Hub schema-2.2 write → signed snapshot + no-op path.
pub fn deliver_pinned_updates_to_hub(
    work_root: &Path,
    store: &HubStore,
    cursor_key: &CursorKey,
) -> Result<UpdatesDeliveryArtifacts, UpdatesDeliveryError> {
    let fixture = parse_pinned_updates_fixture()?;
    let source = encode_updates_logical_stream(&fixture.rows)?;
    if source.sha256 != PINNED_CANONICAL_SHA256 || source.bytes.len() != PINNED_CANONICAL_BYTES {
        return Err(reject(
            "pinned fixture logical stream does not match the frozen canonical digest",
        ));
    }
    if source.summary.open_row_count == 0 || source.summary.null_version_row_count == 0 {
        return Err(reject(
            "pinned fixture must include an open row and a null-version row",
        ));
    }
    let (built, snapshot) =
        write_updates_schema_22_pack(work_root.join("packs"), fixture.rows.clone())?;
    let request = updates_pack_request(&snapshot);
    let pack_bytes = read_bounded_pack_bytes(&built.metadata, &built.path)?;
    built
        .metadata
        .verify_reader(
            Cursor::new(pack_bytes.as_slice()),
            ProtocolLimits::default(),
        )
        .map_err(|error| reject(error.to_string()))?;
    let hub_rows = read_hub_updates_from_pack(&built.metadata, &built.path)?;
    let hub = encode_updates_logical_stream(&hub_rows)?;
    if hub.sha256 != source.sha256 {
        return Err(reject(
            "Hub logical stream does not match the source logical stream",
        ));
    }
    let manifest = sign_updates_schema_22_manifest(&request, &built, cursor_key)?;
    let manifest_bytes =
        serde_json::to_vec(&manifest).map_err(|error| reject(error.to_string()))?;
    let manifest_sha256 = hex_sha256(&manifest_bytes);
    let pack_sha256 = built.metadata.sha256.to_string();
    let noop = sign_updates_schema_22_noop(
        &request.binding,
        request.snapshot_id,
        request.sequence.to_inclusive,
        &pack_sha256,
        cursor_key,
    )?;
    let noop_bytes = serde_json::to_vec(&noop).map_err(|error| reject(error.to_string()))?;
    publish_updates_schema_22(store, &manifest, &noop)?;
    let empty_app = encode_updates_logical_stream(&[])?;
    let receipt = assemble_updates_receipt(
        &source,
        &hub,
        &empty_app,
        &manifest,
        &pack_bytes,
        &manifest_bytes,
        work_root,
        false,
    )?;
    Ok(UpdatesDeliveryArtifacts {
        source,
        hub,
        fixture,
        binding: request.binding.clone(),
        built,
        manifest,
        manifest_bytes,
        manifest_sha256,
        pack_bytes,
        noop,
        noop_bytes,
        receipt,
    })
}

pub fn assemble_updates_receipt(
    source: &LogicalUpdatesStream,
    hub: &LogicalUpdatesStream,
    app: &LogicalUpdatesStream,
    _manifest: &SyncManifest,
    pack_bytes: &[u8],
    manifest_bytes: &[u8],
    work_root: &Path,
    app_committed: bool,
) -> Result<UpdatesReceipt, UpdatesDeliveryError> {
    let layers_equal = source.sha256 == hub.sha256
        && hub.sha256 == app.sha256
        && !source.sha256.is_empty()
        && source.summary.row_count == hub.summary.row_count
        && hub.summary.row_count == app.summary.row_count
        && source.summary.completed_row_count == hub.summary.completed_row_count
        && hub.summary.completed_row_count == app.summary.completed_row_count
        && source.summary.open_row_count == hub.summary.open_row_count
        && hub.summary.open_row_count == app.summary.open_row_count
        && source.summary.null_version_row_count == hub.summary.null_version_row_count
        && hub.summary.null_version_row_count == app.summary.null_version_row_count
        && source.summary.start_min_pg_us == hub.summary.start_min_pg_us
        && hub.summary.start_min_pg_us == app.summary.start_min_pg_us
        && source.summary.start_max_pg_us == hub.summary.start_max_pg_us
        && hub.summary.start_max_pg_us == app.summary.start_max_pg_us
        && source.summary.end_min_pg_us == hub.summary.end_min_pg_us
        && hub.summary.end_min_pg_us == app.summary.end_min_pg_us
        && source.summary.end_max_pg_us == hub.summary.end_max_pg_us
        && hub.summary.end_max_pg_us == app.summary.end_max_pg_us
        && app_committed;
    let accepted_field_count = if layers_equal { UPDATES_FIELD_COUNT } else { 0 };
    let pack_sha256 = hex_sha256(pack_bytes);
    let manifest_sha256 = hex_sha256(manifest_bytes);
    let identity = receipt_identity_hashes()?;
    let _ = work_root;
    Ok(UpdatesReceipt {
        schema: "teslatlas-teslamate-v4-updates-receipt-v1".into(),
        receipt_state: if layers_equal {
            "accepted".into()
        } else {
            "incomplete".into()
        },
        receipt_not_fabricated: true,
        reference_commit: PINNED_TESLAMATE_REVISION.into(),
        table: "updates".into(),
        field_count: UPDATES_FIELD_COUNT,
        app_schema_version: APP_SCHEMA_VERSION.into(),
        canonical_stream_schema: LOGICAL_STREAM_SCHEMA.into(),
        source_row_count: source.summary.row_count,
        selected_car_row_count: source.summary.row_count,
        hub_row_count: hub.summary.row_count,
        app_row_count: app.summary.row_count,
        completed_row_counts: LayerCounts {
            source: source.summary.completed_row_count,
            hub: hub.summary.completed_row_count,
            app: app.summary.completed_row_count,
        },
        open_row_counts: LayerCounts {
            source: source.summary.open_row_count,
            hub: hub.summary.open_row_count,
            app: app.summary.open_row_count,
        },
        null_version_row_counts: LayerCounts {
            source: source.summary.null_version_row_count,
            hub: hub.summary.null_version_row_count,
            app: app.summary.null_version_row_count,
        },
        skipped_reasons: serde_json::Map::new(),
        time_bounds: serde_json::json!({
            "source": bounds_json(&source.summary),
            "hub": bounds_json(&hub.summary),
            "app": bounds_json(&app.summary),
        }),
        hashes: serde_json::json!({
            "source_logical_rows_sha256": source.sha256,
            "hub_logical_rows_sha256": hub.sha256,
            "app_logical_rows_sha256": app.sha256,
            "candidate_artifact_sha256": pack_sha256,
            "binary_path": identity.binary_path,
            "binary_sha256": identity.binary_sha256,
            "config_sha256": identity.config_sha256,
            "toolchain_sha256": identity.toolchain_sha256,
        }),
        candidate: serde_json::json!({
            "identity": "hub-main-schema-2.2-updates-lossless",
            "artifact_sha256": pack_sha256,
        }),
        selected_car_id: PINNED_SELECTED_CAR_ID,
        accepted_field_count,
        schema_version: "2.2".into(),
        manifest_sha256,
        pack_sha256,
        source_commit: TESLAMATE_V4_SOURCE_REVISION.into(),
        binary_sha256: identity.binary_sha256,
        config_sha256: identity.config_sha256,
        toolchain_sha256: identity.toolchain_sha256,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptIdentityHashes {
    pub binary_path: String,
    pub binary_sha256: String,
    pub config_sha256: String,
    pub toolchain_sha256: String,
}

fn shipping_hub_binary_path() -> Result<PathBuf, UpdatesDeliveryError> {
    let mut candidates = Vec::new();
    if let Ok(target_dir) = std::env::var("CARGO_TARGET_DIR") {
        candidates.push(PathBuf::from(target_dir).join("release/teslatlas-hub"));
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/teslatlas-hub"));
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            reject(
                "release teslatlas-hub binary is missing; build it with RUSTFLAGS='-D warnings' cargo build --locked --release --bin teslatlas-hub",
            )
        })
}

/// Bind the shipping Hub binary, shipping config, and rustc toolchain.
pub fn receipt_identity_hashes() -> Result<ReceiptIdentityHashes, UpdatesDeliveryError> {
    let binary_path = shipping_hub_binary_path()?;
    let binary_bytes = fs::read(&binary_path).map_err(|error| reject(error.to_string()))?;
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config_toml = fs::read(manifest_dir.join("packaging/config.toml"))
        .map_err(|error| reject(error.to_string()))?;
    let plist = fs::read(manifest_dir.join("packaging/com.teslatlas.hub.plist.in"))
        .map_err(|error| reject(error.to_string()))?;
    let mut config_document = b"teslatlas-hub-shipping-config-v1\n".to_vec();
    config_document
        .extend_from_slice(&(u64::try_from(config_toml.len()).unwrap_or(0)).to_be_bytes());
    config_document.extend_from_slice(&config_toml);
    config_document.extend_from_slice(&(u64::try_from(plist.len()).unwrap_or(0)).to_be_bytes());
    config_document.extend_from_slice(&plist);
    let rustc = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|error| reject(error.to_string()))?;
    if !rustc.status.success() {
        return Err(reject("rustc -vV failed"));
    }
    let rust_version = option_env!("CARGO_PKG_RUST_VERSION").unwrap_or("1.97");
    let mut toolchain_document = b"teslatlas-hub-toolchain-v1\n".to_vec();
    toolchain_document.extend_from_slice(env!("CARGO_PKG_NAME").as_bytes());
    toolchain_document.push(b'\n');
    toolchain_document.extend_from_slice(env!("CARGO_PKG_VERSION").as_bytes());
    toolchain_document.push(b'\n');
    toolchain_document.extend_from_slice(rust_version.as_bytes());
    toolchain_document.push(b'\n');
    toolchain_document.extend_from_slice(&rustc.stdout);
    Ok(ReceiptIdentityHashes {
        binary_path: binary_path.display().to_string(),
        binary_sha256: hex_sha256(&binary_bytes),
        config_sha256: hex_sha256(&config_document),
        toolchain_sha256: hex_sha256(&toolchain_document),
    })
}

fn bounds_json(summary: &crate::updates_logical::LogicalUpdatesSummary) -> serde_json::Value {
    serde_json::json!({
        "start_min_pg_us": summary.start_min_pg_us,
        "start_max_pg_us": summary.start_max_pg_us,
        "end_min_pg_us": summary.end_min_pg_us,
        "end_max_pg_us": summary.end_max_pg_us,
    })
}

/// Library entry the binary Serve path uses after a pack is catalogued:
/// return the signed manifest and signed no-op bodies.
pub fn schema_22_signed_artifacts(
    store: &HubStore,
    vehicle_id: Uuid,
    cursor_key: &CursorKey,
) -> Result<(Vec<u8>, Vec<u8>), UpdatesDeliveryError> {
    let manifest = store
        .manifest_for_vehicle(vehicle_id)?
        .ok_or_else(|| reject("schema 2.2 manifest is not catalogued"))?;
    if manifest.schema != HUB_PROJECTION_SCHEMA_V3 {
        return Err(reject("catalogued manifest is not schema 2.2"));
    }
    let manifest_bytes =
        serde_json::to_vec(&manifest).map_err(|error| reject(error.to_string()))?;
    let noop_bytes = store
        .schema_22_noop_for_snapshot(vehicle_id, manifest.snapshot_id)?
        .ok_or_else(|| reject("schema 2.2 no-op is not catalogued"))?;
    let noop: SignedNoOpState =
        serde_json::from_slice(&noop_bytes).map_err(|error| reject(error.to_string()))?;
    if serde_json::to_vec(&noop).map_err(|error| reject(error.to_string()))? != noop_bytes {
        return Err(reject("schema 2.2 no-op is not canonical typed JSON"));
    }
    validate_schema_22_pair(&manifest, &noop)?;
    validate_schema_22_cursor_key(&manifest, cursor_key)?;
    Ok((manifest_bytes, noop_bytes))
}

fn validate_schema_22_cursor_key(
    manifest: &SyncManifest,
    cursor_key: &CursorKey,
) -> Result<(), UpdatesDeliveryError> {
    let claims = manifest
        .terminal_cursor
        .verify(cursor_key)
        .map_err(|error| reject(error.to_string()))?;
    if claims.protocol != manifest.protocol
        || claims.schema != manifest.schema
        || claims.installation_id != manifest.installation_id
        || claims.account_id != manifest.account_id
        || claims.vehicle_id != manifest.vehicle_id
        || claims.generation != manifest.generation
        || claims.sequence != manifest.head_sequence
    {
        return Err(reject(
            "schema 2.2 terminal cursor does not match the manifest identity or head",
        ));
    }
    Ok(())
}

pub(crate) fn validate_schema_22_pair(
    manifest: &SyncManifest,
    noop: &SignedNoOpState,
) -> Result<(), UpdatesDeliveryError> {
    manifest
        .validate()
        .map_err(|error| reject(error.to_string()))?;
    let pack = match manifest.chunks.as_slice() {
        [pack] => pack,
        _ => return Err(reject("schema 2.2 manifest must contain exactly one pack")),
    };
    if manifest.schema != HUB_PROJECTION_SCHEMA_V3
        || noop.schema != "teslatlas-hub-schema-22-noop-v1"
        || noop.projection_schema != "2.2"
        || noop.installation_id != manifest.installation_id
        || noop.account_id != manifest.account_id
        || noop.vehicle_id != manifest.vehicle_id
        || noop.generation != manifest.generation
        || noop.snapshot_id != manifest.snapshot_id
        || noop.head_sequence != manifest.head_sequence
        || noop.pack_sha256 != pack.sha256.to_string()
        || noop.terminal_cursor != manifest.terminal_cursor
    {
        return Err(reject("schema 2.2 manifest/no-op pair does not match"));
    }
    if let Some(witness) = noop.source_witness.as_ref() {
        validate_production_source_witness(noop, witness)?;
    }
    Ok(())
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && value.bytes().any(|byte| byte != b'0')
}

#[allow(clippy::too_many_arguments)]
fn validate_witness_summary(
    row_count: u64,
    completed_row_count: u64,
    open_row_count: u64,
    null_version_row_count: u64,
    empty_version_row_count: u64,
    start_min_pg_us: Option<i64>,
    start_max_pg_us: Option<i64>,
    end_min_pg_us: Option<i64>,
    end_max_pg_us: Option<i64>,
) -> bool {
    completed_row_count.checked_add(open_row_count) == Some(row_count)
        && null_version_row_count <= row_count
        && empty_version_row_count <= row_count
        && null_version_row_count
            .checked_add(empty_version_row_count)
            .is_some_and(|total| total <= row_count)
        && match row_count {
            0 => {
                start_min_pg_us.is_none()
                    && start_max_pg_us.is_none()
                    && end_min_pg_us.is_none()
                    && end_max_pg_us.is_none()
            }
            _ => {
                start_min_pg_us
                    .zip(start_max_pg_us)
                    .is_some_and(|(minimum, maximum)| minimum <= maximum)
                    && match completed_row_count {
                        0 => end_min_pg_us.is_none() && end_max_pg_us.is_none(),
                        _ => end_min_pg_us
                            .zip(end_max_pg_us)
                            .is_some_and(|(minimum, maximum)| minimum <= maximum),
                    }
            }
        }
}

fn validate_production_source_witness(
    noop: &SignedNoOpState,
    witness: &ProductionUpdatesSourceWitness,
) -> Result<(), UpdatesDeliveryError> {
    let hashes = [
        witness.postgres_snapshot_sha256.as_str(),
        witness.pinned_migration_set_sha256.as_str(),
        witness.source_schema_fingerprint.as_str(),
        witness.source_capture_sha256.as_str(),
        witness.source_logical_sha256.as_str(),
        witness.hub_logical_sha256.as_str(),
        witness.car_identity_sha256.as_str(),
        witness.pack_sha256.as_str(),
        witness.terminal_cursor_sha256.as_str(),
    ];
    let source_summary_valid = validate_witness_summary(
        witness.source_row_count,
        witness.source_completed_row_count,
        witness.source_open_row_count,
        witness.source_null_version_row_count,
        witness.source_empty_version_row_count,
        witness.source_start_min_pg_us,
        witness.source_start_max_pg_us,
        witness.source_end_min_pg_us,
        witness.source_end_max_pg_us,
    );
    let hub_summary_valid = validate_witness_summary(
        witness.hub_row_count,
        witness.hub_completed_row_count,
        witness.hub_open_row_count,
        witness.hub_null_version_row_count,
        witness.hub_empty_version_row_count,
        witness.hub_start_min_pg_us,
        witness.hub_start_max_pg_us,
        witness.hub_end_min_pg_us,
        witness.hub_end_max_pg_us,
    );
    if witness.schema != "teslatlas-pg-source-witness-v1"
        || witness.source_transaction != "read_only_repeatable_read_exported_snapshot"
        || witness.source_revision != TESLAMATE_V4_SOURCE_REVISION
        || witness.pinned_migration_set_sha256 != TESLAMATE_V4_MIGRATION_SET_SHA256
        || witness.observed_migration_count
            != u64::try_from(TESLAMATE_V4_MIGRATION_COUNT).expect("migration count fits u64")
        || witness.observed_migration_version != MAX_VALIDATED_MIGRATION
        || i16::try_from(witness.selected_car_id)
            .ok()
            .is_none_or(|selected_car_id| selected_car_id <= 0)
        || hashes.iter().any(|value| !is_canonical_sha256(value))
        || witness.source_logical_sha256 != witness.hub_logical_sha256
        || witness.source_row_count != witness.hub_row_count
        || witness.source_completed_row_count != witness.hub_completed_row_count
        || witness.source_open_row_count != witness.hub_open_row_count
        || witness.source_null_version_row_count != witness.hub_null_version_row_count
        || witness.source_empty_version_row_count != witness.hub_empty_version_row_count
        || witness.source_start_min_pg_us != witness.hub_start_min_pg_us
        || witness.source_start_max_pg_us != witness.hub_start_max_pg_us
        || witness.source_end_min_pg_us != witness.hub_end_min_pg_us
        || witness.source_end_max_pg_us != witness.hub_end_max_pg_us
        || !source_summary_valid
        || !hub_summary_valid
        || witness.pack_sha256 != noop.pack_sha256
        || witness.head_sequence != noop.head_sequence
        || witness.terminal_cursor_sha256 != hex_sha256(noop.terminal_cursor.as_str().as_bytes())
    {
        return Err(reject(
            "schema 2.2 production source witness is invalid or mismatched",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::{SourceDescriptor, VehicleDescriptor},
        teslamate_projection::{
            TeslaMateCarPhysicalV2_2, TeslaMateCarSettingsPhysicalV2_2,
            TeslaMateSettingsPhysicalV2_2,
        },
        teslamate_reader::TeslaMateSchemaInfo,
        teslamate_schema::{MAX_VALIDATED_MIGRATION, TESLAMATE_V4_MIGRATION_SET_SHA256},
        updates_logical::decode_updates_logical_stream,
    };

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn production_source(car_id: i16) -> DirectUpdatesSourceV2_2 {
        DirectUpdatesSourceV2_2 {
            postgres_snapshot_sha256: hex_sha256(Uuid::new_v4().as_bytes()),
            schema: TeslaMateSchemaInfo {
                observed_migration_version: MAX_VALIDATED_MIGRATION,
                observed_migration_count: TESLAMATE_V4_MIGRATION_COUNT,
                minimum_supported_migration_version: MAX_VALIDATED_MIGRATION,
                maximum_validated_migration_version: MAX_VALIDATED_MIGRATION,
                pinned_source_revision: TESLAMATE_V4_SOURCE_REVISION,
                pinned_migration_set_sha256: TESLAMATE_V4_MIGRATION_SET_SHA256,
                fingerprint: hex_sha256(b"production-repeatable-read-schema-fingerprint"),
            },
            global_settings: TeslaMateSettingsPhysicalV2_2 {
                id: 1,
                unit_of_length: ProjectionUnitOfLengthV2_2::Kilometers,
                unit_of_temperature: ProjectionUnitOfTemperatureV2_2::Celsius,
                unit_of_pressure: ProjectionUnitOfPressureV2_2::Bar,
                preferred_range: ProjectionPreferredRangeV2_2::Rated,
                base_url: None,
                grafana_url: None,
                language: "en".into(),
                theme_mode: "system".into(),
                inserted_at_pg_us: 0,
                updated_at_pg_us: 0,
            },
            car: TeslaMateCarPhysicalV2_2 {
                id: car_id,
                eid: 100,
                vid: 200,
                vin: Some("TESTVIN".into()),
                name: Some("Selected".into()),
                model: None,
                efficiency: None,
                trim_badging: None,
                marketing_name: None,
                exterior_color: None,
                wheel_type: None,
                spoiler_type: None,
                display_priority: 0,
                inserted_at_pg_us: 0,
                updated_at_pg_us: 0,
                settings_id: 3,
            },
            car_settings: TeslaMateCarSettingsPhysicalV2_2 {
                id: 3,
                suspend_min: 21,
                suspend_after_idle_min: 15,
                req_not_unlocked: false,
                free_supercharging: false,
                use_streaming_api: true,
                enabled: true,
                lfp_battery: false,
            },
            updates: vec![
                TeslaMateUpdatePhysicalV2_2 {
                    id: -4,
                    car_id,
                    start_date_pg_us: -1,
                    end_date_pg_us: Some(0),
                    version: None,
                },
                TeslaMateUpdatePhysicalV2_2 {
                    id: 9,
                    car_id,
                    start_date_pg_us: 1,
                    end_date_pg_us: None,
                    version: Some("  βeta 🚗  ".into()),
                },
            ],
        }
    }

    #[test]
    fn production_capture_publishes_dynamic_exact_pair_and_reuses_exact_bytes() {
        let temp = tempfile::tempdir().expect("store root");
        let store = HubStore::initialize(temp.path()).expect("store");
        let cursor_key = CursorKey::from_bytes([43; 32]);
        let registered_source = store
            .register_source(
                &SourceDescriptor::new("teslamate", format!("test-source-{}", Uuid::new_v4())),
                1_000,
            )
            .expect("source");
        let registered_vehicle = store
            .register_vehicle(
                &VehicleDescriptor {
                    source_id: registered_source.source_id,
                    source_vehicle_key: format!("test-car-{}", Uuid::new_v4()),
                    vin: Some("TESTVIN".into()),
                    display_name: Some("Selected".into()),
                    tesla_eid: Some(100),
                    tesla_vid: Some(200),
                },
                1_000,
            )
            .expect("vehicle");
        let binding = ProjectionBinding {
            installation_id: store.installation_id().expect("installation"),
            account_id: registered_source.source_id,
            vehicle_id: registered_vehicle.vehicle_id,
            generation: registered_source.generation,
            selected_car_id: 7,
        };
        let mut source = production_source(7);
        source.updates.reverse();

        let first =
            publish_production_updates_schema_22(&store, &cursor_key, &binding, source.clone())
                .expect("publish production pair");
        assert!(!first.reused_current_snapshot);
        assert_eq!(first.source_logical_sha256, first.hub_logical_sha256);
        assert_eq!(first.source_summary, first.hub_summary);
        assert_eq!(first.source_summary.row_count, 2);
        assert_eq!(first.source_summary.open_row_count, 1);
        assert_eq!(first.source_summary.null_version_row_count, 1);
        assert_eq!(
            first.source_witness.postgres_snapshot_sha256,
            source.postgres_snapshot_sha256
        );
        assert_eq!(
            first.source_witness.source_logical_sha256,
            first.source_witness.hub_logical_sha256
        );
        assert_eq!(first.source_witness.head_sequence, first.sequence);
        let (manifest_bytes, noop_bytes) =
            schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
                .expect("signed pair");
        assert_eq!(hex_sha256(&manifest_bytes), first.manifest_sha256);
        assert_eq!(hex_sha256(&noop_bytes), first.noop_sha256);
        let noop_json: serde_json::Value = serde_json::from_slice(&noop_bytes).expect("no-op JSON");
        let source_witness_json = noop_json
            .get("sourceWitness")
            .expect("camelCase production source witness");
        assert_eq!(source_witness_json["sourceRowCount"], 2);
        assert_eq!(source_witness_json["hubRowCount"], 2);
        assert!(source_witness_json.get("postgresSnapshotSha256").is_some());
        assert!(noop_json.get("source_witness").is_none());

        let mut retry_source = source.clone();
        retry_source.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
        let second =
            publish_production_updates_schema_22(&store, &cursor_key, &binding, retry_source)
                .expect("idempotent production pair from a fresh exported snapshot");
        assert!(second.reused_current_snapshot);
        assert_eq!(second.snapshot_id, first.snapshot_id);
        assert_eq!(second.sequence, first.sequence);
        assert_eq!(second.pack_sha256, first.pack_sha256);
        assert_eq!(
            schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key).expect("same pair"),
            (manifest_bytes, noop_bytes)
        );
        let pair_before_schema_rejections =
            schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
                .expect("pair before rejection");
        let mut wrong_migration_count = source.clone();
        wrong_migration_count.schema.observed_migration_count = TESLAMATE_V4_MIGRATION_COUNT - 1;
        let error = publish_production_updates_schema_22(
            &store,
            &cursor_key,
            &binding,
            wrong_migration_count,
        )
        .expect_err("contradictory migration count must fail closed");
        assert!(error.message.contains("contradicts"));
        assert_eq!(
            schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
                .expect("pair survives migration-count rejection"),
            pair_before_schema_rejections
        );
        let mut wrong_migration_version = source.clone();
        wrong_migration_version.schema.observed_migration_version -= 1;
        let error = publish_production_updates_schema_22(
            &store,
            &cursor_key,
            &binding,
            wrong_migration_version,
        )
        .expect_err("contradictory migration high-water must fail closed");
        assert!(error.message.contains("contradicts"));
        assert_eq!(
            schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
                .expect("pair survives migration-version rejection"),
            pair_before_schema_rejections
        );
        let stale_a_head =
            production_updates_head(&store, binding.vehicle_id).expect("capture A head");

        let mut changed_source = source.clone();
        changed_source.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
        changed_source.updates[1].version = Some("new exact version".into());
        let changed =
            publish_production_updates_schema_22(&store, &cursor_key, &binding, changed_source)
                .expect("changed source publishes a successor full snapshot");
        assert!(!changed.reused_current_snapshot);
        assert_ne!(changed.snapshot_id, first.snapshot_id);
        assert!(changed.sequence > first.sequence);
        assert_ne!(changed.source_logical_sha256, first.source_logical_sha256);
        let changed_manifest = store
            .manifest_for_vehicle(binding.vehicle_id)
            .expect("changed manifest lookup")
            .expect("changed manifest");
        assert_eq!(changed_manifest.generation, binding.generation);
        assert_eq!(changed_manifest.snapshot_id, changed.snapshot_id);
        assert_eq!(changed_manifest.head_sequence, changed.sequence);
        let changed_pair = schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("newer B pair");
        let stale_gate = store
            .try_acquire_publication_gate()
            .expect("stale A finalizer gate");
        let stale_error = publish_production_updates_schema_22_with_gate(
            &store,
            &cursor_key,
            &binding,
            source.clone(),
            &stale_gate,
            &stale_a_head,
            None,
        )
        .expect_err("A captured before newer B must not publish after B");
        drop(stale_gate);
        assert!(stale_error.message.contains("head changed"));
        assert_eq!(
            schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
                .expect("newer B pair survives stale A"),
            changed_pair
        );

        let mut empty_source = source.clone();
        empty_source.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
        empty_source.updates.clear();
        let empty = publish_production_updates_schema_22(
            &store,
            &cursor_key,
            &binding,
            empty_source.clone(),
        )
        .expect("zero-row snapshot publishes a signed watermark");
        assert!(!empty.reused_current_snapshot);
        assert!(empty.sequence > changed.sequence);
        assert_eq!(empty.source_witness.source_row_count, 0);
        assert_eq!(empty.source_witness.hub_row_count, 0);
        assert_eq!(empty.source_witness.source_open_row_count, 0);
        assert_eq!(empty.source_witness.source_null_version_row_count, 0);
        assert_eq!(empty.source_witness.source_start_min_pg_us, None);
        assert_eq!(empty.source_witness.source_end_max_pg_us, None);
        let empty_pair = schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
            .expect("zero-row signed pair");
        empty_source.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
        let empty_replay =
            publish_production_updates_schema_22(&store, &cursor_key, &binding, empty_source)
                .expect("zero-row replay is exact-byte idempotent");
        assert!(empty_replay.reused_current_snapshot);
        assert_eq!(empty_replay.snapshot_id, empty.snapshot_id);
        assert_eq!(empty_replay.sequence, empty.sequence);
        assert_eq!(
            schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
                .expect("same zero-row pair"),
            empty_pair
        );

        let current = store
            .manifest_for_vehicle(binding.vehicle_id)
            .expect("current manifest")
            .expect("published manifest");
        let mut wrong_binding = binding.clone();
        wrong_binding.selected_car_id = 8;
        let error = publish_production_updates_schema_22(
            &store,
            &cursor_key,
            &wrong_binding,
            source.clone(),
        )
        .expect_err("mismatched selected car must fail closed");
        assert!(error.message.contains("selected-car binding"));
        assert_eq!(
            store
                .manifest_for_vehicle(binding.vehicle_id)
                .expect("manifest after rejection")
                .expect("manifest retained"),
            current
        );

        let mut rebound_binding = binding.clone();
        rebound_binding.selected_car_id = 8;
        let mut rebound_source = source.clone();
        rebound_source.postgres_snapshot_sha256 = hex_sha256(Uuid::new_v4().as_bytes());
        rebound_source.car.id = 8;
        for row in &mut rebound_source.updates {
            row.car_id = 8;
        }
        let error = publish_production_updates_schema_22(
            &store,
            &cursor_key,
            &rebound_binding,
            rebound_source,
        )
        .expect_err("same-generation successor cannot change the stored selected car");
        assert!(error.message.contains("stored selected-car witness"));
        assert_eq!(
            schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
                .expect("pair survives selected-car rebound"),
            empty_pair
        );

        let mut wrong_generation = binding.clone();
        wrong_generation.generation += 1;
        let error =
            publish_production_updates_schema_22(&store, &cursor_key, &wrong_generation, source)
                .expect_err("successor generation drift must fail closed");
        assert!(error.message.contains("identity or generation"));
        assert_eq!(
            store
                .manifest_for_vehicle(binding.vehicle_id)
                .expect("manifest after generation rejection")
                .expect("manifest retained"),
            current
        );

        let mut tampered_noop: SignedNoOpState =
            serde_json::from_slice(&empty_pair.1).expect("typed zero-row no-op");
        tampered_noop
            .source_witness
            .as_mut()
            .expect("production witness")
            .source_row_count = 1;
        let error = publish_updates_schema_22(&store, &current, &tampered_noop)
            .expect_err("tampered source watermark must fail closed");
        assert!(error.message.contains("source witness"));
        assert_eq!(
            schema_22_signed_artifacts(&store, binding.vehicle_id, &cursor_key)
                .expect("pair survives witness tamper"),
            empty_pair
        );
    }

    #[test]
    fn reopened_production_pack_root_mismatch_fails_closed() {
        let temporary = tempfile::tempdir().expect("pack directory");
        let source = production_source(7);
        let source_stream =
            encode_updates_logical_stream(&source.updates).expect("source logical stream");
        let source_capture = production_updates_capture_proof(&source);
        let snapshot = production_updates_snapshot(&source);
        let mut binding = pinned_updates_binding();
        binding.selected_car_id = 7;
        let request = ProjectionPackRequestV2_2 {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding,
            sequence: SequenceRange {
                from_exclusive: 1,
                to_inclusive: 1,
            },
            snapshot: &snapshot,
        };
        let built = ProjectionPackWriter::new(temporary.path())
            .write_full_snapshot_2_2(&request)
            .expect("write production-shaped pack");
        verify_reopened_production_capture(
            &built.metadata,
            &built.path,
            &source_stream,
            &source_capture,
        )
        .expect("all independently reopened roots match");

        let mut mismatched_source = source_capture;
        mismatched_source
            .global_settings
            .language
            .push_str("-changed");
        let error = verify_reopened_production_capture(
            &built.metadata,
            &built.path,
            &source_stream,
            &mismatched_source,
        )
        .expect_err("source-only root claim must be rejected");
        assert!(error.message.contains("roots or updates"));
    }

    #[test]
    fn reopened_pack_rejects_size_bomb_and_row_cap_plus_one() {
        let temporary = tempfile::tempdir().expect("pack directory");
        let source = production_source(7);
        let snapshot = production_updates_snapshot(&source);
        let mut binding = pinned_updates_binding();
        binding.selected_car_id = 7;
        let request = ProjectionPackRequestV2_2 {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding,
            sequence: SequenceRange {
                from_exclusive: 1,
                to_inclusive: 1,
            },
            snapshot: &snapshot,
        };
        let built = ProjectionPackWriter::new(temporary.path())
            .write_full_snapshot_2_2(&request)
            .expect("write production-shaped pack");

        let mut wrong_compressed_size = built.metadata.clone();
        wrong_compressed_size.compressed_bytes += 1;
        assert!(
            private_sqlite_tempfile_from_pack(&wrong_compressed_size, &built.path).is_err(),
            "advertised compressed size mismatch must fail before allocation"
        );

        let mut row_capped = built.metadata.clone();
        row_capped.row_count = 1;
        let error = read_hub_updates_from_pack(&row_capped, &built.path)
            .expect_err("two updates exceed an advertised one-row cap");
        assert!(error.message.contains("row bound"));

        let expanded = (0..4_097)
            .map(|index| u8::try_from(index % 251).expect("bounded byte"))
            .collect::<Vec<_>>();
        let bomb = zstd::stream::encode_all(Cursor::new(&expanded), 1).expect("compress bomb");
        let bomb_path = temporary.path().join("bounded-bomb.zst");
        fs::write(&bomb_path, &bomb).expect("write bomb fixture");
        let mut bomb_metadata = built.metadata;
        bomb_metadata.compressed_bytes = u64::try_from(bomb.len()).expect("bomb size");
        bomb_metadata.uncompressed_bytes = 4_096;
        bomb_metadata.sha256 = crate::protocol::Sha256Digest::of_bytes(&bomb);
        assert!(
            private_sqlite_tempfile_from_pack(&bomb_metadata, &bomb_path).is_err(),
            "expanded cap-plus-one input must fail closed"
        );
    }

    #[test]
    fn production_witness_rejects_overlapping_null_and_empty_denominators() {
        assert!(!validate_witness_summary(
            1,
            1,
            0,
            1,
            1,
            Some(10),
            Some(10),
            Some(20),
            Some(20),
        ));
    }

    #[test]
    fn pinned_fixture_logical_stream_matches_frozen_digest() {
        let fixture = parse_pinned_updates_fixture().expect("parse pinned fixture");
        let stream = encode_updates_logical_stream(&fixture.rows).expect("encode");
        assert_eq!(stream.sha256, PINNED_CANONICAL_SHA256);
        assert_eq!(stream.bytes.len(), PINNED_CANONICAL_BYTES);
        assert_eq!(stream.summary.row_count, 6);
        assert_eq!(stream.summary.completed_row_count, 5);
        assert_eq!(stream.summary.open_row_count, 1);
        assert_eq!(stream.summary.null_version_row_count, 2);
        assert_eq!(stream.summary.empty_version_row_count, 1);
        assert_eq!(stream.summary.start_min_pg_us, Some(i64::MIN));
        assert_eq!(stream.summary.start_max_pg_us, Some(978_307_199_999_999));
        assert_eq!(stream.summary.end_min_pg_us, Some(-1));
        assert_eq!(stream.summary.end_max_pg_us, Some(i64::MAX));
        let decoded = decode_updates_logical_stream(&stream.bytes).expect("decode");
        assert_eq!(decoded.rows, stream.rows);
        assert!(decoded.rows.iter().any(|row| row.id == i32::MIN));
        assert!(decoded.rows.iter().any(|row| row.id == i32::MAX));
        assert!(
            decoded
                .rows
                .iter()
                .any(|row| row.version.as_deref() == Some(""))
        );
        assert!(
            decoded
                .rows
                .iter()
                .any(|row| row.version.is_none() && row.end_date_pg_us.is_none())
        );
        assert!(
            decoded
                .rows
                .iter()
                .any(|row| row.version.as_deref() == Some("  βeta 🚗  "))
        );
    }

    #[test]
    fn copy_parser_preserves_null_empty_escapes_commas_and_quotes() {
        let fields = copy_fields(b"\\N,\"\",\\\\N,\"a,b\",\"a\"\"b\"").unwrap();
        assert_eq!(
            fields,
            vec![
                None,
                Some(Vec::new()),
                Some(b"\\N".to_vec()),
                Some(b"a,b".to_vec()),
                Some(b"a\"b".to_vec())
            ]
        );
    }

    #[test]
    fn decompressed_sqlite_uses_private_raii_temporary_storage() {
        let (directory, file_path) = private_sqlite_tempfile(b"not a database").expect("temporary");
        let directory_path = directory.path.clone();
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&directory_path)
                    .expect("directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&file_path)
                    .expect("file metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(directory);
        assert!(!file_path.exists());
        assert!(!directory_path.exists());
    }

    #[test]
    fn timestamp_parser_matches_pinned_fixture_microseconds() {
        assert_eq!(parse_pg_timestamp_us(b"-infinity").unwrap(), i64::MIN);
        assert_eq!(parse_pg_timestamp_us(b"infinity").unwrap(), i64::MAX);
        assert_eq!(
            parse_pg_timestamp_us(b"1999-12-31 23:59:59.999999").unwrap(),
            -1
        );
        assert_eq!(
            parse_pg_timestamp_us(b"2026-01-01 00:00:00.123456").unwrap(),
            820_540_800_123_456
        );
        assert_eq!(
            parse_pg_timestamp_us(b"2030-12-31 23:59:59.999999").unwrap(),
            978_307_199_999_999
        );
    }

    #[test]
    fn timestamp_parser_rejects_invalid_calendar_dates_and_times() {
        assert!(parse_pg_timestamp_us(b"2026-02-29 00:00:00").is_err());
        assert!(parse_pg_timestamp_us(b"2026-04-31 00:00:00").is_err());
        assert!(parse_pg_timestamp_us(b"1900-02-29 00:00:00").is_err());
        assert!(parse_pg_timestamp_us(b"2024-02-29 23:59:59.999999").is_ok());
        assert!(parse_pg_timestamp_us(b"2000-02-29 00:00:00").is_ok());
        assert!(parse_pg_timestamp_us(b"2026-01-01 24:00:00").is_err());
        assert!(parse_pg_timestamp_us(b"2026-01-01 00:60:00").is_err());
        assert!(parse_pg_timestamp_us(b"2026-01-01 00:00:60").is_err());
    }
}
