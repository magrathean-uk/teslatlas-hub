// SPDX-License-Identifier: AGPL-3.0-only

//! One optional, signed prepared month from an admitted PhysicalV3 source.
//! Failure here never revokes the already-published history head.

use std::{
    collections::HashSet,
    error::Error,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write, copy},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params, types::ValueRef};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use teslatlas_compute::{
    ALGORITHM_VERSION, Drive, MAX_RAW_MAP_POSITIONS_PER_DRIVE, RawMapPosition,
    TILE_GEOMETRY_VERSION, TileOutput, generate_tile_payload_v1_from_pages_with_rdp_limit,
    prepare_positions_for_tile_rendering_v1,
};
use time::{OffsetDateTime, Time};
use uuid::Uuid;

use super::physical_delta_pack::verified_immutable_pack;

use crate::{
    db::{HubStore, PendingPhysicalV3Admission, StoredPack},
    hub_pack::ProjectionFixedNumericV2_2,
    manifest_signing::ManifestSigning,
    protocol::{CursorKey, Sha256Digest},
    teslamate_projection::{TeslaMateDrivePhysicalV2_2, TeslaMatePositionPhysicalV2_2},
    teslamate_stage::{TeslaMateStage, TeslaMateStageTable},
};

type PreparedResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
const ID_DOMAIN: &str = "teslatlas-prepared-map-month-id-v1";
const STYLE: &str = "route-stroke-v8-opaque";
const MAX_SCRATCH_POSITIONS: u64 = 1_000_000;
const MAX_MONTH_BYTES: usize = 8 * 1024 * 1024;
const MAX_PACK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SQLITE_BYTES: u64 = 64 * 1024 * 1024;
const RDP_EVALUATION_LIMIT: u64 = 134_217_728;
const PG_EPOCH_OFFSET_US: i64 = 946_684_800_000_000;
const PACK_SQL: &str = include_str!("prepared_pack_v1.sql");

#[derive(Debug, Clone)]
struct Month {
    name: String,
    from_ms: i64,
    to_ms: i64,
}

fn invalid(message: &'static str) -> Box<dyn Error + Send + Sync> {
    std::io::Error::other(message).into()
}

fn previous_complete_month(now_ms: i64) -> PreparedResult<Option<Month>> {
    if now_ms < 0 {
        return Ok(None);
    }
    let current = OffsetDateTime::from_unix_timestamp(now_ms / 1_000)?.date();
    let current_start = current.replace_day(1)?;
    let Some(previous) = current_start.previous_day() else {
        return Ok(None);
    };
    let previous_start = previous.replace_day(1)?;
    let from_ms = previous_start
        .with_time(Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp()
        * 1_000;
    let to_ms = current_start
        .with_time(Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp()
        * 1_000;
    if from_ms < 0 || to_ms <= from_ms {
        return Ok(None);
    }
    Ok(Some(Month {
        name: format!(
            "{:04}-{:02}",
            previous_start.year(),
            u8::from(previous_start.month())
        ),
        from_ms,
        to_ms,
    }))
}

/// Protocol 1.3 deterministic discovery identity. All source IDs are wire
/// opaque ASCII without LF, so the newline-terminated preimage is unambiguous.
pub(crate) fn artifact_id(
    admission: &PendingPhysicalV3Admission,
    month: &str,
    from_ms: i64,
    to_ms: i64,
) -> String {
    let fields = [
        ID_DOMAIN.to_owned(),
        admission.vehicle_id.to_string(),
        admission.snapshot_id.to_string(),
        admission.receipt_id.clone(),
        admission.head_sequence.to_string(),
        month.to_owned(),
        from_ms.to_string(),
        to_ms.to_string(),
        STYLE.to_owned(),
        TILE_GEOMETRY_VERSION.to_owned(),
        ALGORITHM_VERSION.to_owned(),
    ];
    let preimage = format!("{}\n", fields.join("\n"));
    format!(
        "map-month-v1.{}",
        hex::encode(Sha256::digest(preimage.as_bytes()))
    )
}

fn pg_ms(value: i64) -> PreparedResult<i64> {
    if value == i64::MIN || value == i64::MAX {
        return Err(invalid("prepared map timestamp is infinite"));
    }
    value
        .checked_add(PG_EPOCH_OFFSET_US)
        .and_then(|v| v.checked_div(1_000))
        .ok_or_else(|| invalid("prepared map timestamp is outside App range"))
}

fn coordinate(value: ProjectionFixedNumericV2_2) -> PreparedResult<i64> {
    match value {
        ProjectionFixedNumericV2_2::Finite(value) => Ok(value),
        ProjectionFixedNumericV2_2::NaN => Err(invalid("prepared map coordinate is not finite")),
    }
}

fn scratch_dir(store: &HubStore) -> PreparedResult<PathBuf> {
    let path = store.packs_dir().join(".staging");
    fs::create_dir_all(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

/// Build an index for only the selected month. The sealed stage is keyed by
/// source ID, so a single bounded scan is needed to regroup its positions by
/// drive/date without holding source history in memory.
fn indexed_month_positions(
    stage: &TeslaMateStage,
    scratch: &mut Connection,
    ids: &HashSet<i32>,
    selected_car_id: i64,
) -> PreparedResult<()> {
    scratch.execute_batch(
        "CREATE TABLE selected_positions (
            drive_id INTEGER NOT NULL,
            date_ms INTEGER NOT NULL,
            source_id INTEGER NOT NULL,
            latitude_e6 INTEGER NOT NULL,
            longitude_e6 INTEGER NOT NULL,
            speed INTEGER,
            PRIMARY KEY(drive_id, date_ms, source_id)
         ) WITHOUT ROWID;
         PRAGMA max_page_count = 65536;",
    )?;
    let transaction = scratch.transaction()?;
    let mut insert =
        transaction.prepare("INSERT INTO selected_positions VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
    let mut after = 0;
    let mut accepted = 0_u64;
    loop {
        let page = stage.page::<TeslaMatePositionPhysicalV2_2>(
            TeslaMateStageTable::Positions,
            after,
            10_000,
        )?;
        for row in page.rows {
            let position = row.value;
            let Some(drive_id) = position.drive_id.filter(|id| ids.contains(id)) else {
                continue;
            };
            if i64::from(position.car_id) != selected_car_id {
                return Err(invalid("prepared map position escaped selected car"));
            }
            accepted += 1;
            if accepted > MAX_SCRATCH_POSITIONS {
                return Err(invalid("prepared month exceeds bounded scratch positions"));
            }
            insert.execute(params![
                drive_id,
                pg_ms(position.date_pg_us)?,
                position.id,
                coordinate(position.latitude_e6)?,
                coordinate(position.longitude_e6)?,
                position.speed,
            ])?;
        }
        match page.next_after_id {
            Some(next) => after = next,
            None => break,
        }
    }
    drop(insert);
    transaction.commit()?;
    Ok(())
}

fn read_cleaned_drive(scratch: &Connection, id: i32) -> PreparedResult<Drive> {
    let mut statement = scratch.prepare(
        "SELECT date_ms, latitude_e6, longitude_e6, speed
         FROM selected_positions WHERE drive_id = ?1
         ORDER BY date_ms, source_id LIMIT ?2",
    )?;
    let rows = statement.query_map(
        params![id, i64::try_from(MAX_RAW_MAP_POSITIONS_PER_DRIVE + 1)?],
        |row| {
            Ok(RawMapPosition {
                date_ms: row.get(0)?,
                latitude: row.get::<_, i64>(1)? as f64 / 1_000_000.0,
                longitude: row.get::<_, i64>(2)? as f64 / 1_000_000.0,
                speed_kmh: row.get(3)?,
            })
        },
    )?;
    let positions = rows.collect::<Result<Vec<_>, _>>()?;
    let points = prepare_positions_for_tile_rendering_v1(&positions)?;
    Ok(Drive {
        drive_id: id,
        points,
    })
}

fn compute_month(
    scratch: &Connection,
    drives: &[(i64, i32)],
    rdp_evaluation_limit: u64,
) -> PreparedResult<TileOutput> {
    let cancelled = AtomicBool::new(false);
    let mut failure: Option<Box<dyn Error + Send + Sync>> = None;
    let output = generate_tile_payload_v1_from_pages_with_rdp_limit(
        &cancelled,
        None,
        rdp_evaluation_limit,
        |consume| {
            let mut page = Vec::new();
            let mut page_bytes = 8_usize;
            for (_, id) in drives {
                let drive = match read_cleaned_drive(scratch, *id) {
                    Ok(drive) => drive,
                    Err(error) => {
                        failure = Some(error);
                        return Err(teslatlas_compute::Error::InvalidData(
                            "prepared map drive is unavailable".into(),
                        ));
                    }
                };
                let bytes = 8 + drive.points.len() * 8;
                if page.len() == 128 || page_bytes + bytes > MAX_MONTH_BYTES {
                    consume(&page)?;
                    page.clear();
                    page_bytes = 8;
                }
                if page_bytes + bytes > MAX_MONTH_BYTES {
                    return Err(teslatlas_compute::Error::InvalidData(
                        "prepared drive exceeds bounded page".into(),
                    ));
                }
                page_bytes += bytes;
                page.push(drive);
            }
            if !page.is_empty() {
                consume(&page)?;
            }
            Ok(())
        },
    );
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(output?)
}

fn read_u32(bytes: &[u8], offset: &mut usize) -> PreparedResult<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| invalid("tile offset overflow"))?;
    let slice = bytes
        .get(*offset..end)
        .ok_or_else(|| invalid("tile payload is truncated"))?;
    *offset = end;
    Ok(u32::from_le_bytes(slice.try_into()?))
}

fn decode_tiles(payload: &[u8]) -> PreparedResult<Vec<(u32, u32, u32, Vec<u8>)>> {
    if payload.len() > MAX_MONTH_BYTES {
        return Err(invalid("prepared tile payload exceeds month ceiling"));
    }
    let mut offset = 0;
    if read_u32(payload, &mut offset)? != 1 {
        return Err(invalid("prepared tile payload version is unsupported"));
    }
    let count = usize::try_from(read_u32(payload, &mut offset)?)?;
    if count == 0 || count > 4096 {
        return Err(invalid("prepared tile count is outside contract"));
    }
    let mut tiles = Vec::with_capacity(count);
    let mut app_bytes = 8_usize;
    for _ in 0..count {
        let zoom = read_u32(payload, &mut offset)?;
        let x = read_u32(payload, &mut offset)?;
        let y = read_u32(payload, &mut offset)?;
        let len = usize::try_from(read_u32(payload, &mut offset)?)?;
        let end = offset
            .checked_add(len)
            .ok_or_else(|| invalid("tile length overflow"))?;
        if !(2..=13).contains(&zoom)
            || x >= (1 << zoom)
            || y >= (1 << zoom)
            || len == 0
            || len > 1_600_000
            || len % 8 != 0
        {
            return Err(invalid("prepared tile is outside contract"));
        }
        let bytes = payload
            .get(offset..end)
            .ok_or_else(|| invalid("tile is truncated"))?;
        app_bytes = app_bytes
            .checked_add(40 + len)
            .ok_or_else(|| invalid("month byte overflow"))?;
        if app_bytes > MAX_MONTH_BYTES {
            return Err(invalid("prepared month exceeds App publication ceiling"));
        }
        tiles.push((zoom, x, y, bytes.to_vec()));
        offset = end;
    }
    if offset != payload.len() {
        return Err(invalid("tile payload has trailing bytes"));
    }
    if !tiles
        .windows(2)
        .all(|pair| (pair[0].0, pair[0].1, pair[0].2) < (pair[1].0, pair[1].1, pair[1].2))
    {
        return Err(invalid("prepared tile keys are not strictly ordered"));
    }
    Ok(tiles)
}

fn write_sqlite(
    file: &NamedTempFile,
    id: &str,
    admission: &PendingPhysicalV3Admission,
    month: &Month,
    drive_count: usize,
    tiles: &[(u32, u32, u32, Vec<u8>)],
) -> PreparedResult<u64> {
    let mut connection = Connection::open(file.path())?;
    connection.execute_batch(
        "PRAGMA page_size = 4096;
         PRAGMA auto_vacuum = NONE;
         PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA secure_delete = ON;
         PRAGMA foreign_keys = ON;
         PRAGMA application_id = 1414807888;
         PRAGMA user_version = 1;",
    )?;
    connection.execute_batch(PACK_SQL)?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO prepared_metadata VALUES (?1,?2,?3,?4,?5,?6,?7,?8,
                                               ?9,?10,?11,?12,?13,?14,?15,?16)",
        params![
            1,
            "teslatlas-prepared-v1",
            1,
            "map_months",
            STYLE,
            TILE_GEOMETRY_VERSION,
            1,
            id,
            admission.vehicle_id.to_string(),
            admission.snapshot_id.to_string(),
            admission.receipt_id,
            "2.2",
            i64::try_from(admission.head_sequence)?,
            ALGORITHM_VERSION,
            month.from_ms,
            month.to_ms,
        ],
    )?;
    transaction.execute(
        "INSERT INTO map_months VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            month.name,
            month.from_ms,
            month.to_ms,
            if tiles.is_empty() {
                "readyEmpty"
            } else {
                "readyData"
            },
            i64::try_from(drive_count)?,
            i64::try_from(tiles.len())?,
        ],
    )?;
    {
        let mut insert = transaction.prepare("INSERT INTO map_tiles VALUES (?1,?2,?3,?4,?5)")?;
        for (zoom, x, y, segments) in tiles {
            insert.execute(params![month.name, zoom, x, y, segments])?;
        }
    }
    transaction.commit()?;
    connection.execute_batch("VACUUM")?;
    let before = prepared_logical_digest(&connection)?;
    let mut roots = vec![1_u32];
    {
        let mut query = connection.prepare(
            "SELECT rootpage FROM sqlite_schema
             WHERE type IN ('table', 'index') AND name NOT LIKE 'sqlite_%'
               AND rootpage > 0",
        )?;
        for root in query.query_map([], |row| row.get::<_, i64>(0))? {
            roots.push(u32::try_from(root?)?);
        }
    }
    drop(connection);
    let bytes = fs::metadata(file.path())?.len();
    if bytes > MAX_SQLITE_BYTES {
        return Err(invalid("prepared SQLite image exceeds contract"));
    }
    zero_btree_pointer_gaps(file.path(), &roots)?;
    let checked = Connection::open_with_flags(
        file.path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    if prepared_logical_digest(&checked)? != before
        || checked.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))? != "ok"
    {
        return Err(invalid(
            "prepared SQLite pointer-gap cleanup changed logical content",
        ));
    }
    Ok(bytes)
}

fn prepared_logical_digest(connection: &Connection) -> PreparedResult<[u8; 32]> {
    let mut hash = Sha256::new();
    for (table, order) in [
        ("prepared_metadata", "singleton"),
        ("map_months", "month"),
        ("map_tiles", "month, zoom, tile_x, tile_y"),
    ] {
        hash.update(table.as_bytes());
        let mut query = connection.prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))?;
        let columns = query.column_count();
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            hash.update([0xff]);
            for index in 0..columns {
                match row.get_ref(index)? {
                    ValueRef::Null => hash.update([0]),
                    ValueRef::Integer(value) => {
                        hash.update([1]);
                        hash.update(value.to_le_bytes());
                    }
                    ValueRef::Real(value) => {
                        hash.update([2]);
                        hash.update(value.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(value) => {
                        hash.update([3]);
                        hash.update(u64::try_from(value.len())?.to_le_bytes());
                        hash.update(value);
                    }
                    ValueRef::Blob(value) => {
                        hash.update([4]);
                        hash.update(u64::try_from(value.len())?.to_le_bytes());
                        hash.update(value);
                    }
                }
            }
        }
    }
    Ok(hash.finalize().into())
}

/// SQLite can leave nonzero bytes in the unallocated gap between a B-tree
/// page's pointer array and cell content after VACUUM. Protocol's byte-domain
/// contract requires that gap to be zero. Traverse only known table B-trees;
/// overflow pages and cell payloads are never rewritten.
fn zero_btree_pointer_gaps(path: &Path, roots: &[u32]) -> PreparedResult<usize> {
    let mut image = fs::read(path)?;
    const PAGE_BYTES: usize = 4096;
    if image.len() < PAGE_BYTES
        || image.len() > MAX_SQLITE_BYTES as usize
        || image.len() % PAGE_BYTES != 0
        || !image.starts_with(b"SQLite format 3\0")
    {
        return Err(invalid(
            "prepared SQLite image is outside byte-domain bounds",
        ));
    }
    let page_count = image.len() / PAGE_BYTES;
    let mut pending = roots.to_vec();
    let mut visited = HashSet::new();
    let mut cleared = 0_usize;
    while let Some(page_number) = pending.pop() {
        let number = usize::try_from(page_number)?;
        if number == 0 || number > page_count || !visited.insert(page_number) {
            return Err(invalid("prepared SQLite B-tree page reference is invalid"));
        }
        let start = (number - 1) * PAGE_BYTES;
        let header = start + if number == 1 { 100 } else { 0 };
        let kind = *image
            .get(header)
            .ok_or_else(|| invalid("prepared B-tree header is missing"))?;
        let interior = match kind {
            2 | 5 => true,
            10 | 13 => false,
            _ => return Err(invalid("prepared SQLite B-tree page type is invalid")),
        };
        let cell_count = usize::from(u16::from_be_bytes(
            image[header + 3..header + 5].try_into()?,
        ));
        let content = usize::from(u16::from_be_bytes(
            image[header + 5..header + 7].try_into()?,
        ));
        let content = if content == 0 { PAGE_BYTES } else { content };
        let pointer_start = (header - start) + if interior { 12 } else { 8 };
        let pointer_end = pointer_start
            .checked_add(
                cell_count
                    .checked_mul(2)
                    .ok_or_else(|| invalid("prepared cell count overflow"))?,
            )
            .ok_or_else(|| invalid("prepared pointer array overflow"))?;
        if pointer_end > content || content > PAGE_BYTES {
            return Err(invalid("prepared SQLite B-tree pointer gap is invalid"));
        }
        let mut freeblock = usize::from(u16::from_be_bytes(
            image[header + 1..header + 3].try_into()?,
        ));
        for _ in 0..PAGE_BYTES / 4 {
            if freeblock == 0 {
                break;
            }
            if freeblock < content || freeblock + 4 > PAGE_BYTES {
                return Err(invalid("prepared SQLite freeblock is invalid"));
            }
            let block_at = start + freeblock;
            let next = usize::from(u16::from_be_bytes(
                image[block_at..block_at + 2].try_into()?,
            ));
            let size = usize::from(u16::from_be_bytes(
                image[block_at + 2..block_at + 4].try_into()?,
            ));
            if size < 4 || freeblock + size > PAGE_BYTES || (next != 0 && next < freeblock + size) {
                return Err(invalid("prepared SQLite freeblock chain is invalid"));
            }
            freeblock = next;
        }
        if freeblock != 0 {
            return Err(invalid(
                "prepared SQLite freeblock chain exceeds page bound",
            ));
        }
        if interior {
            pending.push(u32::from_be_bytes(
                image[header + 8..header + 12].try_into()?,
            ));
        }
        for ordinal in 0..cell_count {
            let pointer_at = start + pointer_start + ordinal * 2;
            let cell_at = usize::from(u16::from_be_bytes(
                image[pointer_at..pointer_at + 2].try_into()?,
            ));
            if cell_at < content || cell_at >= PAGE_BYTES || (interior && cell_at + 4 > PAGE_BYTES)
            {
                return Err(invalid("prepared SQLite cell pointer is invalid"));
            }
            if interior {
                pending.push(u32::from_be_bytes(
                    image[start + cell_at..start + cell_at + 4].try_into()?,
                ));
            }
        }
        let gap = &mut image[start + pointer_end..start + content];
        cleared += gap.iter().filter(|byte| **byte != 0).count();
        gap.fill(0);
    }
    let mut output = OpenOptions::new().write(true).open(path)?;
    output.write_all(&image)?;
    output.sync_all()?;
    Ok(cleared)
}

fn compressed_pack(
    store: &HubStore,
    staging: &Path,
    sqlite: &NamedTempFile,
    uncompressed_bytes: u64,
    sync_directory: &dyn Fn(&Path) -> io::Result<()>,
) -> PreparedResult<(String, u64, PathBuf)> {
    let mut compressed = NamedTempFile::new_in(staging)?;
    let mut input = File::open(sqlite.path())?;
    let mut encoder = zstd::stream::Encoder::new(&mut compressed, 3)?;
    encoder.set_pledged_src_size(Some(uncompressed_bytes))?;
    copy(&mut input, &mut encoder)?;
    encoder.finish()?;
    compressed.flush()?;
    compressed.as_file().sync_all()?;
    let bytes = compressed.as_file().metadata()?.len();
    if bytes == 0 || bytes > MAX_PACK_BYTES {
        return Err(invalid("prepared compressed pack exceeds contract"));
    }
    let mut hash = Sha256::new();
    let mut input = File::open(compressed.path())?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    let digest = hex::encode(hash.finalize());
    let directory = store.packs_dir().join("sha256");
    fs::create_dir_all(&directory)?;
    let destination = directory.join(format!("{digest}.sqlite.zst"));
    let installed = match compressed.persist_noclobber(&destination) {
        Ok(file) => file,
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            verified_immutable_pack(&destination, bytes, digest.parse()?, MAX_PACK_BYTES)?
        }
        Err(error) => return Err(error.error.into()),
    };
    installed.sync_all()?;
    sync_directory(&directory)?;
    // The content directory itself may have just been created.
    sync_directory(store.packs_dir())?;
    Ok((digest, bytes, destination))
}

fn signed_receipt(
    cursor_key: &CursorKey,
    admission: &PendingPhysicalV3Admission,
    id: &str,
    month: &Month,
    drive_count: usize,
    tile_count: usize,
    digest: &str,
    compressed_bytes: u64,
    uncompressed_bytes: u64,
    now_ms: i64,
) -> PreparedResult<Vec<u8>> {
    let mut receipt = json!({
        "algorithm_version": ALGORITHM_VERSION,
        "artifact_id": id,
        "artifact_schema_version": 1,
        "artifact_type": "map_months",
        "dirty_spans": {"map_months": [{
            "month": month.name, "from_ms": month.from_ms, "to_ms": month.to_ms,
            "resolution": if tile_count == 0 { "readyEmpty" } else { "readyData" },
            "drive_count": drive_count, "tile_count": tile_count, "reason": "changed"
        }]},
        "generation": {"generation_id": id, "generated_at_ms": now_ms},
        "map_style": STYLE,
        "pack": {
            "compressed_bytes": compressed_bytes,
            "media_type": "application/vnd.teslatlas.prepared+sqlite+zstd;version=1",
            "object_name": format!("{digest}.sqlite.zst"),
            "payload": "teslatlas-prepared-v1",
            "sha256": digest,
            "uncompressed_bytes": uncompressed_bytes,
        },
        "payload": "teslatlas-prepared-v1",
        "scope": "map_months",
        "source": {
            "input_manifest_id": admission.snapshot_id.to_string(),
            "input_manifest_schema": "2.2",
            "input_receipt_id": admission.receipt_id,
            "input_sequence": admission.head_sequence,
            "kind": "hub_compute",
        },
        "tile_geometry_version": TILE_GEOMETRY_VERSION,
        "units": {"coordinates": "wgs84_degrees", "distance": "km", "time": "ms"},
        "vehicle_id": admission.vehicle_id,
        "window": {"from_ms": month.from_ms, "to_ms": month.to_ms},
    });
    let canonical = serde_jcs::to_vec(&receipt)?;
    let signing = ManifestSigning::from_cursor_key(cursor_key);
    let signature = json!({
        "algorithm": "ed25519",
        "key_id": signing.key_id(),
        "signed_payload_sha256": Sha256Digest::of_bytes(&canonical).to_string(),
        "signature": signing.sign_base64(&canonical),
    });
    receipt
        .as_object_mut()
        .ok_or_else(|| invalid("prepared receipt is not an object"))?
        .insert("signature".into(), signature);
    let bytes = serde_json::to_vec(&receipt)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(invalid("prepared receipt exceeds control response ceiling"));
    }
    Ok(bytes)
}

struct CachedMonth {
    vehicle: String,
    source_receipt: String,
    month: String,
    manifest: String,
    sequence: i64,
    source_digest: String,
    digest: String,
    compressed_bytes: i64,
    receipt: Option<Vec<u8>>,
}

fn read_cached_month(row: &rusqlite::Row<'_>) -> rusqlite::Result<CachedMonth> {
    Ok(CachedMonth {
        vehicle: row.get(0)?,
        source_receipt: row.get(1)?,
        month: row.get(2)?,
        manifest: row.get(3)?,
        sequence: row.get(4)?,
        source_digest: row.get(5)?,
        digest: row.get(6)?,
        compressed_bytes: row.get(7)?,
        receipt: row.get(8)?,
    })
}

/// Integrity and current semantics are separate gates. This check applies on
/// delivery even before any producer runs or prunes a restored catalogue.
fn current_semantic_receipt(
    key: &CursorKey,
    id: &str,
    cached: &CachedMonth,
) -> PreparedResult<bool> {
    let Some(bytes) = &cached.receipt else {
        return Ok(false);
    };
    let Ok(mut document) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Ok(false);
    };
    // The producer persists this exact normalized representation. This also
    // rejects duplicate members instead of accepting the parser's last value.
    if serde_json::to_vec(&document)? != *bytes {
        return Ok(false);
    }
    let Some(from_ms) = document["window"]["from_ms"].as_i64() else {
        return Ok(false);
    };
    let Some(to_ms) = document["window"]["to_ms"].as_i64() else {
        return Ok(false);
    };
    let Ok(Some(month)) = previous_complete_month(to_ms) else {
        return Ok(false);
    };
    let fields = [
        ID_DOMAIN.to_owned(),
        cached.vehicle.clone(),
        cached.manifest.clone(),
        cached.source_receipt.clone(),
        cached.sequence.to_string(),
        cached.month.clone(),
        from_ms.to_string(),
        to_ms.to_string(),
        STYLE.to_owned(),
        TILE_GEOMETRY_VERSION.to_owned(),
        ALGORITHM_VERSION.to_owned(),
    ];
    let expected_id = format!(
        "map-month-v1.{}",
        hex::encode(Sha256::digest(
            format!("{}\n", fields.join("\n")).as_bytes()
        ))
    );
    if id != expected_id
        || month.name != cached.month
        || month.from_ms != from_ms
        || month.to_ms != to_ms
        || document["algorithm_version"].as_str() != Some(ALGORITHM_VERSION)
        || document["tile_geometry_version"].as_str() != Some(TILE_GEOMETRY_VERSION)
        || document["map_style"].as_str() != Some(STYLE)
        || document["artifact_id"].as_str() != Some(id)
        || document["generation"]["generation_id"].as_str() != Some(id)
        || document["artifact_schema_version"].as_u64() != Some(1)
        || document["artifact_type"].as_str() != Some("map_months")
        || document["scope"].as_str() != Some("map_months")
        || document["payload"].as_str() != Some("teslatlas-prepared-v1")
        || document["vehicle_id"].as_str() != Some(cached.vehicle.as_str())
        || document["source"]["input_manifest_id"].as_str() != Some(cached.manifest.as_str())
        || document["source"]["input_receipt_id"].as_str() != Some(cached.source_receipt.as_str())
        || document["source"]["input_sequence"].as_u64() != u64::try_from(cached.sequence).ok()
        || document["source"]["input_manifest_schema"].as_str() != Some("2.2")
        || document["source"]["kind"].as_str() != Some("hub_compute")
        || document["pack"]["sha256"].as_str() != Some(cached.digest.as_str())
        || document["pack"]["object_name"].as_str()
            != Some(format!("{}.sqlite.zst", cached.digest).as_str())
        || document["pack"]["compressed_bytes"].as_u64()
            != u64::try_from(cached.compressed_bytes).ok()
        || cached.compressed_bytes <= 0
        || cached.compressed_bytes as u64 > MAX_PACK_BYTES
    {
        return Ok(false);
    }
    let signing = ManifestSigning::from_cursor_key(key);
    let signature = document
        .as_object_mut()
        .and_then(|object| object.remove("signature"));
    let canonical = serde_jcs::to_vec(&document)?;
    Ok(signature
        == Some(json!({
            "algorithm":"ed25519", "key_id":signing.key_id(),
            "signed_payload_sha256":Sha256Digest::of_bytes(&canonical).to_string(),
            "signature":signing.sign_base64(&canonical),
        })))
}

fn prune_obsolete_prepared_receipts(catalogue: &Connection, key: &CursorKey) -> PreparedResult<()> {
    let mut statement = catalogue.prepare(
        "SELECT artifact_id, vehicle_id, input_receipt_id, month, input_manifest_id,
         input_sequence, source_digest, pack_sha256, compressed_bytes,
         CASE WHEN length(receipt_json) <= 2097152 THEN receipt_json END FROM prepared_map_months",
    )?;
    let entries = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, read_cached_month_offset(row)?))
    })?;
    let mut obsolete = Vec::new();
    for entry in entries {
        let (id, cached) = entry?;
        if !current_semantic_receipt(key, &id, &cached)? {
            obsolete.push(id);
        }
    }
    drop(statement);
    for id in obsolete {
        catalogue.execute(
            "DELETE FROM prepared_map_months WHERE artifact_id = ?1",
            [id],
        )?;
    }
    Ok(())
}

fn read_cached_month_offset(row: &rusqlite::Row<'_>) -> rusqlite::Result<CachedMonth> {
    Ok(CachedMonth {
        vehicle: row.get(1)?,
        source_receipt: row.get(2)?,
        month: row.get(3)?,
        manifest: row.get(4)?,
        sequence: row.get(5)?,
        source_digest: row.get(6)?,
        digest: row.get(7)?,
        compressed_bytes: row.get(8)?,
        receipt: row.get(9)?,
    })
}

/// A cached row alone is insufficient. Reconstruct the complete deterministic
/// signed receipt for this exact source, month and current algorithm, then
/// verify its bounded object before skipping all stage/compute work.
fn verified_cached_month(
    store: &HubStore,
    cursor_key: &CursorKey,
    admission: &PendingPhysicalV3Admission,
    id: &str,
    month: &Month,
    cached: &CachedMonth,
) -> PreparedResult<Option<File>> {
    if cached.month != month.name
        || cached.manifest != admission.snapshot_id.to_string()
        || u64::try_from(cached.sequence).ok() != Some(admission.head_sequence)
        || cached.source_digest.parse::<Sha256Digest>().is_err()
    {
        return Ok(None);
    }
    let Ok(compressed_bytes) = u64::try_from(cached.compressed_bytes) else {
        return Ok(None);
    };
    if compressed_bytes == 0 || compressed_bytes > MAX_PACK_BYTES {
        return Ok(None);
    }
    let Ok(digest) = cached.digest.parse::<Sha256Digest>() else {
        return Ok(None);
    };
    if digest.to_string() != cached.digest {
        return Ok(None);
    }
    let Some(receipt) = &cached.receipt else {
        return Ok(None);
    };
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(receipt) else {
        return Ok(None);
    };
    let counts = (|| {
        let span = document["dirty_spans"]["map_months"].as_array()?.first()?;
        Some((
            usize::try_from(span["drive_count"].as_u64()?).ok()?,
            usize::try_from(span["tile_count"].as_u64()?).ok()?,
            document["pack"]["uncompressed_bytes"].as_u64()?,
            document["generation"]["generated_at_ms"].as_i64()?,
        ))
    })();
    let Some((drive_count, tile_count, uncompressed_bytes, generated_at_ms)) = counts else {
        return Ok(None);
    };
    if drive_count > 10_000
        || tile_count > 4096
        || uncompressed_bytes == 0
        || uncompressed_bytes > MAX_SQLITE_BYTES
        || generated_at_ms < 0
    {
        return Ok(None);
    }
    let expected = signed_receipt(
        cursor_key,
        admission,
        id,
        month,
        drive_count,
        tile_count,
        &cached.digest,
        compressed_bytes,
        uncompressed_bytes,
        generated_at_ms,
    )?;
    if receipt != &expected {
        return Ok(None);
    }
    let path = store
        .packs_dir()
        .join("sha256")
        .join(format!("{digest}.sqlite.zst"));
    Ok(verified_immutable_pack(&path, compressed_bytes, digest, MAX_PACK_BYTES).ok())
}

/// Run after a complete PhysicalV3 admission while the importer still owns the
/// sealed stage and publication gate. Returning an error leaves history public;
/// the caller logs it and a later identical import may retry preparation.
pub(crate) fn publish_selected_month(
    store: &HubStore,
    cursor_key: &CursorKey,
    stage: &TeslaMateStage,
    admission: &PendingPhysicalV3Admission,
    now_ms: i64,
) -> PreparedResult<()> {
    publish_selected_month_with_policy(
        store,
        cursor_key,
        stage,
        admission,
        now_ms,
        RDP_EVALUATION_LIMIT,
        &|path| File::open(path)?.sync_all(),
    )
}

fn publish_selected_month_with_policy(
    store: &HubStore,
    cursor_key: &CursorKey,
    stage: &TeslaMateStage,
    admission: &PendingPhysicalV3Admission,
    now_ms: i64,
    rdp_evaluation_limit: u64,
    sync_directory: &dyn Fn(&Path) -> io::Result<()>,
) -> PreparedResult<()> {
    let Some(month) = previous_complete_month(now_ms)? else {
        return Ok(());
    };
    let id = artifact_id(admission, &month.name, month.from_ms, month.to_ms);
    let mut catalogue = store.open()?;
    prune_obsolete_prepared_receipts(&catalogue, cursor_key)?;
    // A replaced head remains available only while its PhysicalV3 rebase
    // receipt is retained. Prune catalogues at admission; pack repair can
    // reclaim the resulting orphaned content-addressed objects later.
    catalogue.execute(
        "DELETE FROM prepared_map_months AS p
         WHERE NOT EXISTS (
             SELECT 1 FROM pending_physical_v3_admissions AS h
             WHERE h.vehicle_id = p.vehicle_id
               AND h.receipt_id = p.input_receipt_id
               AND h.serve_state = 'public_first'
         ) AND NOT EXISTS (
             SELECT 1 FROM retained_physical_v3_admissions AS r
             WHERE r.vehicle_id = p.vehicle_id
               AND r.receipt_id = p.input_receipt_id
               AND r.expires_at_ms > ?1
         )",
        [now_ms],
    )?;
    let previous: Option<CachedMonth> = catalogue
        .query_row(
            "SELECT vehicle_id, input_receipt_id, month, input_manifest_id,
                    input_sequence, source_digest, pack_sha256, compressed_bytes,
                    CASE WHEN length(receipt_json) <= 2097152 THEN receipt_json END
         FROM prepared_map_months WHERE artifact_id = ?1",
            [&id],
            |row| {
                Ok(CachedMonth {
                    vehicle: row.get(0)?,
                    source_receipt: row.get(1)?,
                    month: row.get(2)?,
                    manifest: row.get(3)?,
                    sequence: row.get(4)?,
                    source_digest: row.get(5)?,
                    digest: row.get(6)?,
                    compressed_bytes: row.get(7)?,
                    receipt: row.get(8)?,
                })
            },
        )
        .optional()?;
    if let Some(previous) = &previous
        && (previous.vehicle != admission.vehicle_id.to_string()
            || previous.source_receipt != admission.receipt_id)
    {
        return Err(invalid(
            "prepared artifact identity conflicts with its source head",
        ));
    }
    if let Some(previous) = &previous
        && let Some(pack) =
            verified_cached_month(store, cursor_key, admission, &id, &month, previous)?
    {
        require_current_source_head(&catalogue, admission)?;
        pack.sync_all()?;
        sync_directory(&store.packs_dir().join("sha256"))?;
        sync_directory(store.packs_dir())?;
        return Ok(());
    }

    let mut drives = Vec::new();
    let mut after = 0;
    loop {
        let page =
            stage.page::<TeslaMateDrivePhysicalV2_2>(TeslaMateStageTable::Drives, after, 10_000)?;
        for row in page.rows {
            let drive = row.value;
            if i64::from(drive.car_id) != admission.selected_car_id {
                return Err(invalid("prepared map drive escaped selected car"));
            }
            // The App's PhysicalV3 projection does not publish an unfinished
            // drive or its positions. Use that same drive set for prepared
            // geometry, including when an open drive starts in this month.
            if drive.end_date_pg_us.is_none() {
                continue;
            }
            let start_ms = pg_ms(drive.start_date_pg_us)?;
            if start_ms >= month.from_ms && start_ms < month.to_ms {
                drives.push((start_ms, drive.id));
                if drives.len() > 10_000 {
                    return Err(invalid("prepared month has too many drives"));
                }
            }
        }
        match page.next_after_id {
            Some(next) => after = next,
            None => break,
        }
    }
    drives.sort_unstable();
    let ids = drives.iter().map(|(_, id)| *id).collect::<HashSet<_>>();
    let staging = scratch_dir(store)?;
    let scratch_file = NamedTempFile::new_in(&staging)?;
    let mut scratch = Connection::open(scratch_file.path())?;
    indexed_month_positions(stage, &mut scratch, &ids, admission.selected_car_id)?;
    let output = compute_month(&scratch, &drives, rdp_evaluation_limit)?;
    drop(scratch);
    let (source_digest, tiles) = match output {
        TileOutput::ReadyEmpty { drive_count, .. } => {
            if usize::try_from(drive_count)? != drives.len() {
                return Err(invalid("prepared drive count is inconsistent"));
            }
            let digest =
                Sha256Digest::of_bytes(format!("empty-month-v1:{}", drives.len()).as_bytes())
                    .to_string();
            (digest, Vec::new())
        }
        TileOutput::ReadyData {
            drive_count,
            payload,
            tile_count,
            ..
        } => {
            if usize::try_from(drive_count)? != drives.len() {
                return Err(invalid("prepared drive count is inconsistent"));
            }
            let tiles = decode_tiles(&payload)?;
            if tiles.len() != tile_count {
                return Err(invalid("prepared tile count is inconsistent"));
            }
            (Sha256Digest::of_bytes(&payload).to_string(), tiles)
        }
    };
    let sqlite_file = NamedTempFile::new_in(&staging)?;
    let uncompressed_bytes =
        write_sqlite(&sqlite_file, &id, admission, &month, drives.len(), &tiles)?;
    let (digest, compressed_bytes, _pack_path) = compressed_pack(
        store,
        &staging,
        &sqlite_file,
        uncompressed_bytes,
        sync_directory,
    )?;
    let receipt = signed_receipt(
        cursor_key,
        admission,
        &id,
        &month,
        drives.len(),
        tiles.len(),
        &digest,
        compressed_bytes,
        uncompressed_bytes,
        now_ms,
    )?;

    require_current_source_head(&catalogue, admission)?;
    let transaction = catalogue.transaction()?;
    transaction.execute(
        "INSERT INTO prepared_map_months
         (artifact_id, vehicle_id, month, input_manifest_id, input_receipt_id,
          input_sequence, source_digest, pack_sha256, compressed_bytes, receipt_json)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(artifact_id) DO UPDATE SET
             month = excluded.month,
             input_manifest_id = excluded.input_manifest_id,
             input_sequence = excluded.input_sequence,
             source_digest = excluded.source_digest,
             pack_sha256 = excluded.pack_sha256,
             compressed_bytes = excluded.compressed_bytes,
             receipt_json = excluded.receipt_json
         WHERE prepared_map_months.vehicle_id = excluded.vehicle_id
           AND prepared_map_months.input_receipt_id = excluded.input_receipt_id",
        params![
            id,
            admission.vehicle_id.to_string(),
            month.name,
            admission.snapshot_id.to_string(),
            admission.receipt_id,
            i64::try_from(admission.head_sequence)?,
            source_digest,
            digest,
            i64::try_from(compressed_bytes)?,
            receipt,
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

fn require_current_source_head(
    catalogue: &Connection,
    admission: &PendingPhysicalV3Admission,
) -> PreparedResult<()> {
    let current_receipt: Option<String> = catalogue
        .query_row(
            "SELECT receipt_id FROM pending_physical_v3_admissions
         WHERE vehicle_id = ?1 AND serve_state = 'public_first'",
            [admission.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if current_receipt.as_deref() != Some(admission.receipt_id.as_str()) {
        return Err(invalid(
            "prepared source head changed before catalogue admission",
        ));
    }
    Ok(())
}

/// Resolve only a receipt from the current public PhysicalV3 head and exact
/// vehicle. Older prepared receipts remain private after a head replacement.
pub(crate) fn current_receipt(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    id: &str,
) -> PreparedResult<Option<Vec<u8>>> {
    if id.len() != 77
        || !id.starts_with("map-month-v1.")
        || !id.as_bytes()[13..]
            .iter()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Ok(None);
    }
    let connection = store.open()?;
    let result = connection
        .query_row(
            "SELECT p.vehicle_id, p.input_receipt_id, p.month, p.input_manifest_id,
                    p.input_sequence, p.source_digest, p.pack_sha256, p.compressed_bytes,
                    CASE WHEN length(p.receipt_json) <= 2097152 THEN p.receipt_json END
         FROM prepared_map_months AS p
         JOIN pending_physical_v3_admissions AS h
           ON h.vehicle_id = p.vehicle_id AND h.receipt_id = p.input_receipt_id
         WHERE p.vehicle_id = ?1 AND p.artifact_id = ?2
           AND h.serve_state = 'public_first'
           AND h.snapshot_id = p.input_manifest_id AND h.head_sequence = p.input_sequence",
            params![vehicle_id.to_string(), id],
            read_cached_month,
        )
        .optional()?;
    match result {
        Some(cached) if current_semantic_receipt(cursor_key, id, &cached)? => Ok(cached.receipt),
        _ => Ok(None),
    }
}

/// Authorize content-addressed prepared objects through the current signed
/// receipt catalogue before the shared pack streamer opens any file.
pub(crate) fn current_pack(
    store: &HubStore,
    cursor_key: &CursorKey,
    digest: Sha256Digest,
) -> PreparedResult<Option<StoredPack>> {
    let connection = store.open()?;
    let mut statement = connection.prepare(
        "SELECT p.artifact_id, p.vehicle_id, p.input_receipt_id, p.month, p.input_manifest_id,
                    p.input_sequence, p.source_digest, p.pack_sha256, p.compressed_bytes,
                    CASE WHEN length(p.receipt_json) <= 2097152 THEN p.receipt_json END
         FROM prepared_map_months AS p
         JOIN pending_physical_v3_admissions AS h
           ON h.vehicle_id = p.vehicle_id AND h.receipt_id = p.input_receipt_id
         WHERE p.pack_sha256 = ?1 AND h.serve_state = 'public_first'
           AND h.snapshot_id = p.input_manifest_id AND h.head_sequence = p.input_sequence",
    )?;
    let entries = statement.query_map([digest.to_string()], |row| {
        Ok((row.get::<_, String>(0)?, read_cached_month_offset(row)?))
    })?;
    let mut entry = None;
    for candidate in entries {
        let (id, cached) = candidate?;
        if current_semantic_receipt(cursor_key, &id, &cached)? {
            entry = Some(cached.compressed_bytes);
            break;
        }
    }
    let Some(bytes) = entry else {
        return Ok(None);
    };
    Ok(Some(StoredPack {
        digest,
        compressed_bytes: u64::try_from(bytes)?,
        path: store
            .packs_dir()
            .join("sha256")
            .join(format!("{digest}.sqlite.zst")),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        import::teslamate::physical_fragments::tests::{
            registered_admission_binding, seed_roots, seed_updates, stage_limits,
        },
        teslamate_physical_publication::{
            PhysicalV3PublicationKind, publish_sealed_physical_v3_stage,
        },
        teslamate_stage::TeslaMateStageLimits,
    };

    fn paged_whole_drive_stage(root: &Path, month: &Month, expanded: bool) -> TeslaMateStage {
        let mut stage = TeslaMateStage::create_physical_v3(
            root.join(format!("whole-drive-stage-{}", Uuid::new_v4())),
            TeslaMateStageLimits {
                max_rows: 11_000,
                max_stage_bytes: 16 * 1024 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .unwrap();
        seed_roots(&mut stage);
        let base = month.from_ms + 3_600_000;
        for id in 30..=158 {
            let drive: TeslaMateDrivePhysicalV2_2 = serde_json::from_value(json!({
                "id": id, "car_id": 1,
                "start_date_pg_us": source_us(if id == 158 { base + 60_000 } else { month.from_ms }),
                "end_date_pg_us": source_us(if id == 158 { base + 10_001 * 60_000 } else { month.from_ms + 120_000 }),
            }))
            .unwrap();
            stage
                .insert(TeslaMateStageTable::Drives, i64::from(id), &drive)
                .unwrap();
        }
        let mut insert = |id: i32, drive_id: i32, date_ms: i64, latitude: i64| {
            let position: TeslaMatePositionPhysicalV2_2 = serde_json::from_value(json!({
                "id": id, "car_id": 1, "drive_id": drive_id,
                "date_pg_us": source_us(date_ms),
                "latitude_e6": {"Finite": latitude},
                "longitude_e6": {"Finite": -100_000}, "speed": 0,
            }))
            .unwrap();
            stage
                .insert(TeslaMateStageTable::Positions, i64::from(id), &position)
                .unwrap();
        };
        // Three stationary rows clean to one point. They interleave source IDs
        // with the main drive, but each remains a distinct complete Drive.
        for parked in 0..128 {
            for point in 0..3 {
                insert(
                    6 * parked + 2 * point + 1,
                    30 + parked,
                    month.from_ms + i64::from(point) * 60_000,
                    51_000_000,
                );
            }
        }
        if expanded {
            // Page one has 384 parked rows and 9,616 main-drive A rows.
            // Page two has 385 B rows. Dates reverse source order within both
            // runs, so correct drive/date regrouping is necessary.
            for point in 0..9_616 {
                let id = if point < 384 {
                    2 * (point + 1)
                } else {
                    point + 385
                };
                insert(
                    id,
                    158,
                    base + i64::from(9_616 - point) * 60_000,
                    51_000_000,
                );
            }
            for point in 0..385 {
                insert(
                    10_001 + point,
                    158,
                    base + i64::from(10_001 - point) * 60_000,
                    51_010_000,
                );
            }
        } else {
            insert(10_001, 158, base + 60_000, 51_000_000);
            insert(10_002, 158, base + 10_001 * 60_000, 51_010_000);
        }
        stage.seal().unwrap();
        stage
    }

    #[tokio::test]
    async fn public_preparation_regroups_source_pages_into_complete_drives() {
        let temporary = crate::private_tempdir().unwrap();
        let now = OffsetDateTime::now_utc().unix_timestamp() * 1_000;
        let month = previous_complete_month(now).unwrap().unwrap();
        let stage = paged_whole_drive_stage(temporary.path(), &month, true);
        let first = stage
            .page::<TeslaMatePositionPhysicalV2_2>(TeslaMateStageTable::Positions, 0, 10_000)
            .unwrap();
        assert_eq!(first.rows.len(), 10_000);
        assert_eq!(first.next_after_id, Some(10_000));
        let second = stage
            .page::<TeslaMatePositionPhysicalV2_2>(
                TeslaMateStageTable::Positions,
                first.next_after_id.unwrap(),
                10_000,
            )
            .unwrap();
        assert_eq!(second.rows.len(), 385);
        assert_eq!(second.next_after_id, None);
        assert_eq!(
            first
                .rows
                .iter()
                .filter(|r| r.value.drive_id == Some(158))
                .count(),
            9_616
        );
        assert!(second.rows.iter().all(|r| r.value.drive_id == Some(158)));
        let mut fragmented = Vec::new();
        for page in [first, second] {
            let mut groups =
                std::collections::BTreeMap::<i32, Vec<(i64, i32, RawMapPosition)>>::new();
            for row in page.rows {
                let p = row.value;
                let date_ms = pg_ms(p.date_pg_us).unwrap();
                groups.entry(p.drive_id.unwrap()).or_default().push((
                    date_ms,
                    p.id,
                    RawMapPosition {
                        date_ms,
                        latitude: coordinate(p.latitude_e6).unwrap() as f64 / 1_000_000.0,
                        longitude: coordinate(p.longitude_e6).unwrap() as f64 / 1_000_000.0,
                        speed_kmh: p.speed,
                    },
                ));
            }
            let drives = groups
                .into_iter()
                .map(|(drive_id, mut rows)| {
                    rows.sort_by_key(|(date, id, _)| (*date, *id));
                    Drive {
                        drive_id,
                        points: prepare_positions_for_tile_rendering_v1(
                            &rows.into_iter().map(|(_, _, p)| p).collect::<Vec<_>>(),
                        )
                        .unwrap(),
                    }
                })
                .collect::<Vec<_>>();
            fragmented.push(drives);
        }
        assert_eq!(
            fragmented
                .iter()
                .flatten()
                .filter(|d| d.drive_id == 158)
                .count(),
            2
        );
        // Deliberately violating the whole-Drive precondition loses A→B, even
        // though each fragment is cleaned in date order. This is not the Hub path.
        let bad = generate_tile_payload_v1_from_pages_with_rdp_limit(
            &AtomicBool::new(false),
            None,
            RDP_EVALUATION_LIMIT,
            |consume| {
                for page in &fragmented {
                    for bounded in page.chunks(128) {
                        consume(bounded)?;
                    }
                }
                Ok(())
            },
        )
        .unwrap();
        assert!(matches!(
            bad,
            TileOutput::ReadyEmpty {
                drive_count: 130,
                ..
            }
        ));
        let stage_path = stage.path().to_owned();
        let key = CursorKey::from_bytes([0x58; 32]);
        let mut produced = Vec::new();
        for (name, stage) in [
            ("expanded", stage),
            (
                "compact",
                paged_whole_drive_stage(temporary.path(), &month, false),
            ),
        ] {
            let root = temporary.path().join(name);
            let store = HubStore::initialize(&root).unwrap();
            let binding = registered_admission_binding(&store);
            let publication =
                publish_sealed_physical_v3_stage(&store, &key, binding.clone(), stage)
                    .await
                    .unwrap();
            let id = artifact_id(
                &publication.admission,
                &month.name,
                month.from_ms,
                month.to_ms,
            );
            let receipt = current_receipt(&store, &key, binding.vehicle_id, &id)
                .unwrap()
                .unwrap();
            let document: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
            assert_eq!(document["dirty_spans"]["map_months"][0]["drive_count"], 129);
            assert!(
                document["dirty_spans"]["map_months"][0]["tile_count"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
            let source_digest: String = store
                .open()
                .unwrap()
                .query_row(
                    "SELECT source_digest FROM prepared_map_months WHERE artifact_id = ?1",
                    [&id],
                    |row| row.get(0),
                )
                .unwrap();
            drop(store);
            let reopened = HubStore::open_existing(&root).unwrap();
            assert_eq!(
                current_receipt(&reopened, &key, binding.vehicle_id, &id)
                    .unwrap()
                    .unwrap(),
                receipt
            );
            let pack_digest = document["pack"]["sha256"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            let pack = current_pack(&reopened, &key, pack_digest).unwrap().unwrap();
            let image = NamedTempFile::new_in(temporary.path()).unwrap();
            let raw = zstd::stream::decode_all(File::open(&pack.path).unwrap()).unwrap();
            fs::write(image.path(), raw).unwrap();
            let database = Connection::open(image.path()).unwrap();
            let (count, tile_count): (i64, i64) = database
                .query_row(
                    "SELECT drive_count, tile_count FROM map_months WHERE month = ?1",
                    [&month.name],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(count, 129);
            let tiles = database.prepare("SELECT zoom, tile_x, tile_y, segments FROM map_tiles ORDER BY zoom, tile_x, tile_y")
                .unwrap().query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, Vec<u8>>(3)?)))
                .unwrap().collect::<Result<Vec<_>, _>>().unwrap();
            assert_eq!(tiles.len(), usize::try_from(tile_count).unwrap());
            assert!(tiles.iter().all(|(_, _, _, bytes)| !bytes.is_empty()));
            assert_eq!(
                fs::read_dir(reopened.packs_dir().join(".staging"))
                    .unwrap()
                    .count(),
                0
            );
            produced.push((source_digest, tiles));
        }
        assert!(
            !stage_path.exists(),
            "the public publisher discards its owned stage"
        );
        assert_eq!(
            produced[0], produced[1],
            "the ordinary multi-page publisher matches complete compact drives"
        );
    }

    #[tokio::test]
    async fn public_preparation_waiting_for_gate_discards_cancelled_stage() {
        let temporary = crate::private_tempdir().unwrap();
        let store = HubStore::initialize(temporary.path().join("hub")).unwrap();
        let binding = registered_admission_binding(&store);
        let key = CursorKey::from_bytes([0x59; 32]);
        let now = OffsetDateTime::now_utc().unix_timestamp() * 1_000;
        let month = previous_complete_month(now).unwrap().unwrap();
        let stage = month_stage(temporary.path(), &month, false, false);
        let stage_path = stage.path().to_owned();
        let gate = store.acquire_publication_gate().await.unwrap();
        let mut publication = Box::pin(publish_sealed_physical_v3_stage(
            &store,
            &key,
            binding.clone(),
            stage,
        ));
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(publication.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(publication);
        assert!(!stage_path.exists());
        drop(gate);
        let retry = publish_sealed_physical_v3_stage(
            &store,
            &key,
            binding,
            month_stage(temporary.path(), &month, false, false),
        )
        .await
        .unwrap();
        assert_eq!(retry.kind, PhysicalV3PublicationKind::FirstHead);
    }

    #[tokio::test]
    async fn signed_legacy_meridian_month_is_withheld_before_unchanged_head_recompute() {
        use axum::{
            body::Body,
            http::{Request, StatusCode, header},
        };
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let temporary = crate::private_tempdir().unwrap();
        let root = temporary.path().join("hub");
        let store = HubStore::initialize(&root).unwrap();
        let binding = registered_admission_binding(&store);
        let key = CursorKey::from_bytes([0x49; 32]);
        let now = OffsetDateTime::now_utc().unix_timestamp() * 1_000;
        let month = previous_complete_month(now).unwrap().unwrap();
        let first = publish_sealed_physical_v3_stage(
            &store,
            &key,
            binding.clone(),
            month_stage_with_geometry(temporary.path(), &month, false, false, false, true),
        )
        .await
        .unwrap();
        let current_id = artifact_id(&first.admission, &month.name, month.from_ms, month.to_ms);
        let source_digest: String = store
            .open()
            .unwrap()
            .query_row(
                "SELECT source_digest FROM prepared_map_months WHERE artifact_id = ?1",
                [&current_id],
                |r| r.get(0),
            )
            .unwrap();
        let fields = [
            ID_DOMAIN.to_owned(),
            binding.vehicle_id.to_string(),
            first.admission.snapshot_id.to_string(),
            first.admission.receipt_id.clone(),
            first.admission.head_sequence.to_string(),
            month.name.clone(),
            month.from_ms.to_string(),
            month.to_ms.to_string(),
            STYLE.to_owned(),
            TILE_GEOMETRY_VERSION.to_owned(),
            "0.1.0".to_owned(),
        ];
        let legacy_id = format!(
            "map-month-v1.{}",
            hex::encode(Sha256::digest(
                format!("{}\n", fields.join("\n")).as_bytes()
            ))
        );
        assert_ne!(current_id, legacy_id);
        // The retained pre-fix generator witness yields ReadyEmpty for these
        // exact two meridian points and drive 30 (see external round witness).
        // Package that old result with its own identity, not relabeled new tiles.
        let staging = scratch_dir(&store).unwrap();
        let sqlite = NamedTempFile::new_in(&staging).unwrap();
        let raw_bytes =
            write_sqlite(&sqlite, &legacy_id, &first.admission, &month, 1, &[]).unwrap();
        Connection::open(sqlite.path())
            .unwrap()
            .execute(
                "UPDATE prepared_metadata SET algorithm_version = '0.1.0'",
                [],
            )
            .unwrap();
        let (legacy_digest, legacy_bytes, legacy_path) =
            compressed_pack(&store, &staging, &sqlite, raw_bytes, &|path| {
                File::open(path)?.sync_all()
            })
            .unwrap();
        let mut document: serde_json::Value = serde_json::from_slice(
            &signed_receipt(
                &key,
                &first.admission,
                &legacy_id,
                &month,
                1,
                0,
                &legacy_digest,
                legacy_bytes,
                raw_bytes,
                now,
            )
            .unwrap(),
        )
        .unwrap();
        document.as_object_mut().unwrap().remove("signature");
        document["algorithm_version"] = json!("0.1.0");
        let signing = ManifestSigning::from_cursor_key(&key);
        let receipt = signing.signed_control_document(&document).unwrap();
        let signed: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
        let signature = ed25519_dalek::Signature::from_slice(
            &STANDARD
                .decode(signed["signature"]["signature"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        let public: [u8; 32] = hex::decode(signing.verifying_key_hex())
            .unwrap()
            .try_into()
            .unwrap();
        ed25519_dalek::VerifyingKey::from_bytes(&public)
            .unwrap()
            .verify_strict(&serde_jcs::to_vec(&document).unwrap(), &signature)
            .unwrap();
        assert_eq!(
            Sha256Digest::of_bytes(&fs::read(&legacy_path).unwrap()).to_string(),
            legacy_digest
        );
        let connection = store.open().unwrap();
        connection
            .execute(
                "DELETE FROM prepared_map_months WHERE artifact_id = ?1",
                [&current_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO prepared_map_months VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    legacy_id,
                    binding.vehicle_id.to_string(),
                    month.name,
                    first.admission.snapshot_id.to_string(),
                    first.admission.receipt_id,
                    first.admission.head_sequence as i64,
                    source_digest,
                    legacy_digest,
                    legacy_bytes as i64,
                    receipt
                ],
            )
            .unwrap();
        drop(connection);
        drop(store);
        let store = HubStore::open_existing(&root).unwrap();
        let invitation = store
            .create_pairing("upgrade fixture", 0, i64::MAX)
            .unwrap();
        let access = store
            .claim_pairing(
                invitation.pairing_id,
                invitation.secret(),
                "upgrade client",
                now,
            )
            .unwrap();
        let bearer = access.access_token.as_bearer();
        let app = crate::server::paired_router(store.clone(), &key);
        let get = |uri: String| {
            let app = app.clone();
            async move {
                let response = app
                    .oneshot(
                        Request::builder()
                            .uri(uri)
                            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                            .header(crate::server::SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
                            .header(crate::server::SYNC_PROFILE_HEADER, "hub-sync-v1@1.3.0")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                (
                    response.status(),
                    response.into_body().collect().await.unwrap().to_bytes(),
                )
            }
        };
        let old_uri = format!(
            "/v1/vehicles/{}/sync/prepared-artefacts/{legacy_id}",
            binding.vehicle_id
        );
        let old_pack_uri = format!("/v1/packs/sha256/{legacy_digest}.sqlite.zst");
        assert_eq!(get(old_uri.clone()).await.0, StatusCode::NOT_FOUND);
        assert_eq!(get(old_pack_uri.clone()).await.0, StatusCode::NOT_FOUND);
        assert_eq!(
            store
                .open()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM prepared_map_months WHERE artifact_id=?1",
                    [&legacy_id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1,
            "delivery withholds before pruning or recomputation"
        );

        // An unavailable optional stage must not make the old artifact eligible.
        let unavailable =
            month_stage_with_geometry(temporary.path(), &month, false, false, false, true);
        Connection::open(unavailable.path())
            .unwrap()
            .execute(
                "UPDATE stage_rows SET row_json='null' WHERE table_name='drives'",
                [],
            )
            .unwrap();
        assert!(publish_selected_month(&store, &key, &unavailable, &first.admission, now).is_err());
        assert!(
            store
                .pending_physical_v3_admission_for_vehicle(binding.vehicle_id)
                .unwrap()
                .is_some()
        );
        assert_eq!(get(old_uri.clone()).await.0, StatusCode::NOT_FOUND);
        assert_eq!(
            get(format!(
                "/v1/vehicles/{}/sync/prepared-artefacts/{current_id}",
                binding.vehicle_id
            ))
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(format!("/v1/vehicles/{}/sync/manifest", binding.vehicle_id))
                .await
                .0,
            StatusCode::OK
        );
        let stage = month_stage_with_geometry(temporary.path(), &month, false, false, false, true);
        publish_selected_month(&store, &key, &stage, &first.admission, now).unwrap();
        let (status, corrected_receipt) = get(format!(
            "/v1/vehicles/{}/sync/prepared-artefacts/{current_id}",
            binding.vehicle_id
        ))
        .await;
        assert_eq!(status, StatusCode::OK);
        let corrected: serde_json::Value = serde_json::from_slice(&corrected_receipt).unwrap();
        assert_eq!(corrected["algorithm_version"], ALGORITHM_VERSION);
        assert!(
            corrected["dirty_spans"]["map_months"][0]["tile_count"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(
            corrected["source"]["input_sequence"],
            first.admission.head_sequence
        );
        let digest: Sha256Digest = corrected["pack"]["sha256"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let (status, body) = get(format!("/v1/packs/sha256/{digest}.sqlite.zst")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(Sha256Digest::of_bytes(&body), digest);
        let decoded = zstd::stream::decode_all(&body[..]).unwrap();
        let checked = NamedTempFile::new_in(temporary.path()).unwrap();
        fs::write(checked.path(), decoded).unwrap();
        let checked = Connection::open(checked.path()).unwrap();
        assert!(
            checked
                .query_row("SELECT count(*) FROM map_tiles", [], |r| r.get::<_, i64>(0))
                .unwrap()
                > 0
        );
        assert_eq!(
            checked
                .query_row("SELECT algorithm_version FROM prepared_metadata", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            ALGORITHM_VERSION
        );
        assert_eq!(get(old_uri).await.0, StatusCode::NOT_FOUND);
        assert_eq!(get(old_pack_uri).await.0, StatusCode::NOT_FOUND);
        assert_eq!(
            store
                .open()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM prepared_map_months WHERE artifact_id=?1",
                    [&legacy_id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        publish_selected_month_with_policy(
            &store,
            &key,
            &unavailable,
            &first.admission,
            now,
            0,
            &|path| File::open(path)?.sync_all(),
        )
        .unwrap();
        assert_eq!(
            current_receipt(&store, &key, binding.vehicle_id, &current_id)
                .unwrap()
                .unwrap(),
            corrected_receipt.as_ref()
        );
    }

    fn source_us(unix_ms: i64) -> i64 {
        unix_ms * 1_000 - PG_EPOCH_OFFSET_US
    }

    #[test]
    #[ignore = "requires TESLATLAS_PREPARED_REAL_PACK and TESLATLAS_PREPARED_PROTOCOL_VALIDATOR; run explicitly with --ignored"]
    fn real_prepared_pack_pointer_gap_cleanup_preserves_rows() {
        let pack_path = std::env::var_os("TESLATLAS_PREPARED_REAL_PACK")
            .expect("required real-pack acceptance needs TESLATLAS_PREPARED_REAL_PACK");
        let python = std::env::var_os("TESLATLAS_PREPARED_PROTOCOL_VALIDATOR")
            .expect("real pack check requires the independent Protocol Python validator");
        let pack_path = PathBuf::from(pack_path);
        let mut decoder = zstd::stream::read::Decoder::new(File::open(&pack_path).unwrap())
            .expect("compressed real pack");
        let mut raw = Vec::new();
        decoder
            .by_ref()
            .take(MAX_SQLITE_BYTES + 1)
            .read_to_end(&mut raw)
            .expect("bounded real pack decode");
        assert!(raw.len() <= MAX_SQLITE_BYTES as usize);
        let image = NamedTempFile::new_in(pack_path.parent().unwrap()).expect("private scratch");
        fs::write(image.path(), raw).expect("scratch SQLite image");
        let connection = Connection::open(image.path()).expect("real SQLite image");
        let before = prepared_logical_digest(&connection).expect("logical digest before");
        let mut roots = vec![1_u32];
        {
            let mut query = connection
                .prepare(
                    "SELECT rootpage FROM sqlite_schema
                 WHERE type IN ('table', 'index') AND name NOT LIKE 'sqlite_%'
                   AND rootpage > 0",
                )
                .unwrap();
            for root in query.query_map([], |row| row.get::<_, i64>(0)).unwrap() {
                roots.push(u32::try_from(root.unwrap()).unwrap());
            }
        }
        drop(connection);
        let cleared = zero_btree_pointer_gaps(image.path(), &roots).expect("bounded gap cleanup");
        assert!(cleared > 0, "real pack must exercise nonzero unused bytes");
        let checked = Connection::open_with_flags(image.path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("cleaned image");
        assert_eq!(prepared_logical_digest(&checked).unwrap(), before);
        assert_eq!(
            checked
                .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        drop(checked);
        let protocol_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("teslatlas-protocol");
        let script = "import sqlite3,sys\n".to_owned()
            + "from pathlib import Path\n"
            + "sys.path.insert(0,sys.argv[1])\n"
            + "from conformance import hub_sync\n"
            + "root=Path(sys.argv[2])\n"
            + "profile=hub_sync.load_profile(root)\n"
            + "contract=hub_sync.strict_json((root/profile['prepared_pack_contract']).read_bytes())\n"
            + "path=Path(sys.argv[3])\n"
            + "db=sqlite3.connect(f'file:{path}?mode=ro&immutable=1',uri=True)\n"
            + "assert hub_sync._prepared_sqlite_byte_domain_is_canonical(path.read_bytes(),db,contract)\n";
        let status = std::process::Command::new(python)
            .arg("-c")
            .arg(script)
            .arg(&protocol_root)
            .arg(protocol_root.join("profiles/hub-sync-v1/1.3.0"))
            .arg(image.path())
            .status()
            .expect("Protocol checker starts");
        assert!(status.success(), "Protocol rejected cleaned real image");
        assert_eq!(
            zero_btree_pointer_gaps(image.path(), &roots).expect("cleanup is idempotent"),
            0,
        );
        let mut invalid_image = fs::read(image.path()).expect("private scratch bytes");
        invalid_image[103..105].copy_from_slice(&u16::MAX.to_be_bytes());
        fs::write(image.path(), &invalid_image).expect("invalid scratch page");
        assert!(
            zero_btree_pointer_gaps(image.path(), &roots).is_err(),
            "an out-of-bounds cell pointer array must fail closed",
        );
        assert_eq!(
            fs::read(image.path()).unwrap(),
            invalid_image,
            "failed cleanup must not partially rewrite the image",
        );
    }

    fn month_stage(root: &Path, month: &Month, changed: bool, update: bool) -> TeslaMateStage {
        month_stage_with_open(root, month, changed, update, false)
    }

    fn month_stage_with_open(
        root: &Path,
        month: &Month,
        changed: bool,
        update: bool,
        include_open: bool,
    ) -> TeslaMateStage {
        month_stage_with_geometry(root, month, changed, update, include_open, false)
    }

    fn month_stage_with_geometry(
        root: &Path,
        month: &Month,
        changed: bool,
        update: bool,
        include_open: bool,
        meridian: bool,
    ) -> TeslaMateStage {
        let mut stage = TeslaMateStage::create_physical_v3(
            root.join(format!("map-stage-{}", Uuid::new_v4())),
            TeslaMateStageLimits {
                max_rows: 32,
                ..stage_limits()
            },
        )
        .expect("stage");
        seed_roots(&mut stage);
        let drive = TeslaMateDrivePhysicalV2_2 {
            id: 30,
            car_id: 1,
            start_date_pg_us: source_us(month.from_ms + 3_600_000),
            end_date_pg_us: Some(source_us(month.from_ms + 3_720_000)),
            start_position_id: None,
            end_position_id: None,
            start_address_id: None,
            end_address_id: None,
            start_geofence_id: None,
            end_geofence_id: None,
            outside_temp_avg_e1: None,
            inside_temp_avg_e1: None,
            speed_max: None,
            power_max: None,
            power_min: None,
            start_ideal_range_km_e2: None,
            end_ideal_range_km_e2: None,
            start_rated_range_km_e2: None,
            end_rated_range_km_e2: None,
            start_km: None,
            end_km: None,
            distance: None,
            duration_min: None,
            ascent: None,
            descent: None,
        };
        stage
            .insert(TeslaMateStageTable::Drives, 30, &drive)
            .expect("drive");
        if include_open {
            let mut open_drive = drive.clone();
            open_drive.id = 31;
            open_drive.start_date_pg_us = source_us(month.from_ms + 7_200_000);
            open_drive.end_date_pg_us = None;
            stage
                .insert(TeslaMateStageTable::Drives, 31, &open_drive)
                .expect("open drive");
        }
        for index in 0..if meridian { 2 } else { 3 } {
            let position = TeslaMatePositionPhysicalV2_2 {
                id: 40 + index,
                car_id: 1,
                drive_id: Some(30),
                date_pg_us: source_us(month.from_ms + 3_600_000 + i64::from(index) * 60_000),
                latitude_e6: ProjectionFixedNumericV2_2::Finite(if meridian {
                    66_000_000 + i64::from(index) * 10_000
                } else {
                    51_000_000 + i64::from(index) * if changed { 2_000 } else { 1_000 }
                }),
                longitude_e6: ProjectionFixedNumericV2_2::Finite(if meridian {
                    if index == 0 {
                        -180_000_000
                    } else {
                        180_000_000
                    }
                } else {
                    -100_000 + i64::from(index) * 1_000
                }),
                elevation: None,
                speed: Some(60),
                power: None,
                odometer: None,
                ideal_battery_range_km_e2: None,
                est_battery_range_km_e2: None,
                rated_battery_range_km_e2: None,
                battery_level: None,
                usable_battery_level: None,
                battery_heater: None,
                battery_heater_on: None,
                battery_heater_no_power: None,
                outside_temp_e1: None,
                inside_temp_e1: None,
                fan_status: None,
                driver_temp_setting_e1: None,
                passenger_temp_setting_e1: None,
                is_climate_on: None,
                is_rear_defroster_on: None,
                is_front_defroster_on: None,
                tpms_pressure_fl_e1: None,
                tpms_pressure_fr_e1: None,
                tpms_pressure_rl_e1: None,
                tpms_pressure_rr_e1: None,
            };
            stage
                .insert(
                    TeslaMateStageTable::Positions,
                    i64::from(position.id),
                    &position,
                )
                .expect("position");
            if include_open {
                let mut open_position = position.clone();
                open_position.id = 50 + index;
                open_position.drive_id = Some(31);
                open_position.date_pg_us =
                    source_us(month.from_ms + 7_200_000 + i64::from(index) * 60_000);
                open_position.latitude_e6 =
                    ProjectionFixedNumericV2_2::Finite(52_000_000 + i64::from(index) * 10_000);
                stage
                    .insert(
                        TeslaMateStageTable::Positions,
                        i64::from(open_position.id),
                        &open_position,
                    )
                    .expect("open drive position");
            }
        }
        if update {
            seed_updates(&mut stage, &[10]);
        }
        stage.seal().expect("sealed stage");
        stage
    }

    #[tokio::test]
    async fn open_drive_and_its_positions_do_not_change_prepared_geometry() {
        let temporary = crate::private_tempdir().expect("temporary");
        let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
        let binding = registered_admission_binding(&store);
        let key = CursorKey::from_bytes([0x43; 32]);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as i64;
        let month = previous_complete_month(now_ms)
            .expect("month")
            .expect("interior month");
        let first = publish_sealed_physical_v3_stage(
            &store,
            &key,
            binding.clone(),
            month_stage(temporary.path(), &month, false, false),
        )
        .await
        .expect("closed drive head");
        let first_id = artifact_id(&first.admission, &month.name, month.from_ms, month.to_ms);
        let first_digest: String = store
            .open()
            .unwrap()
            .query_row(
                "SELECT source_digest FROM prepared_map_months WHERE artifact_id = ?1",
                [&first_id],
                |row| row.get(0),
            )
            .unwrap();

        let with_open = publish_sealed_physical_v3_stage(
            &store,
            &key,
            binding.clone(),
            month_stage_with_open(temporary.path(), &month, false, false, true),
        )
        .await
        .expect("open drive head");
        assert_eq!(with_open.kind, PhysicalV3PublicationKind::Rotation);
        let open_id = artifact_id(
            &with_open.admission,
            &month.name,
            month.from_ms,
            month.to_ms,
        );
        let open_receipt = current_receipt(&store, &key, binding.vehicle_id, &open_id)
            .unwrap()
            .expect("prepared receipt");
        let document: serde_json::Value = serde_json::from_slice(&open_receipt).unwrap();
        assert_eq!(document["dirty_spans"]["map_months"][0]["drive_count"], 1);
        let open_digest: String = store
            .open()
            .unwrap()
            .query_row(
                "SELECT source_digest FROM prepared_map_months WHERE artifact_id = ?1",
                [&open_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            open_digest, first_digest,
            "an open drive and its positions cannot change canonical tile bytes",
        );
    }

    #[tokio::test]
    async fn healthy_same_head_skips_unavailable_stage_and_repairs_forged_receipts() {
        let temporary = crate::private_tempdir().unwrap();
        let store = HubStore::initialize(temporary.path().join("hub")).unwrap();
        let binding = registered_admission_binding(&store);
        let key = CursorKey::from_bytes([0x44; 32]);
        let now_ms = OffsetDateTime::now_utc().unix_timestamp() * 1_000;
        let month = previous_complete_month(now_ms).unwrap().unwrap();
        let first = publish_sealed_physical_v3_stage(
            &store,
            &key,
            binding.clone(),
            month_stage(temporary.path(), &month, false, false),
        )
        .await
        .unwrap();
        let id = artifact_id(&first.admission, &month.name, month.from_ms, month.to_ms);
        let original_receipt = current_receipt(&store, &key, binding.vehicle_id, &id)
            .unwrap()
            .unwrap();

        // The admission already binds history. Make subsequent stage reads
        // actually fail, so successful reuse proves those reads are skipped.
        let unavailable = month_stage(temporary.path(), &month, false, false);
        Connection::open(unavailable.path())
            .unwrap()
            .execute(
                "UPDATE stage_rows SET row_json = 'null' WHERE table_name IN ('drives', 'positions')",
                [],
            )
            .unwrap();
        assert!(
            unavailable
                .page::<TeslaMateDrivePhysicalV2_2>(TeslaMateStageTable::Drives, 0, 10_000)
                .is_err()
        );
        assert!(
            unavailable
                .page::<TeslaMatePositionPhysicalV2_2>(TeslaMateStageTable::Positions, 0, 10_000)
                .is_err()
        );
        publish_selected_month_with_policy(
            &store,
            &key,
            &unavailable,
            &first.admission,
            now_ms,
            0,
            &|path| File::open(path)?.sync_all(),
        )
        .expect("healthy signed object requires no stage or RDP work");
        assert_eq!(
            current_receipt(&store, &key, binding.vehicle_id, &id).unwrap(),
            Some(original_receipt)
        );

        let valid = month_stage(temporary.path(), &month, false, false);
        for forged_field in [
            "algorithm_version",
            "source",
            "signature",
            "reformatted",
            "duplicate",
        ] {
            let receipt = current_receipt(&store, &key, binding.vehicle_id, &id)
                .unwrap()
                .unwrap();
            let mut document: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
            match forged_field {
                "source" => document["source"]["input_receipt_id"] = json!("wrong-source"),
                "signature" => document["signature"]["signature"] = json!("invalid-signature"),
                "reformatted" | "duplicate" => {}
                _ => document[forged_field] = json!("obsolete-algorithm"),
            }
            let invalid_receipt = match forged_field {
                "reformatted" => serde_json::to_vec_pretty(&document).unwrap(),
                "duplicate" => {
                    let mut duplicate = br#"{"algorithm_version":"obsolete-algorithm","#.to_vec();
                    duplicate.extend_from_slice(&receipt[1..]);
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&duplicate).unwrap(),
                        document
                    );
                    duplicate
                }
                _ => serde_json::to_vec(&document).unwrap(),
            };
            store
                .open()
                .unwrap()
                .execute(
                    "UPDATE prepared_map_months SET receipt_json = ?1 WHERE artifact_id = ?2",
                    params![invalid_receipt, id],
                )
                .unwrap();
            let failure = publish_selected_month_with_policy(
                &store,
                &key,
                &valid,
                &first.admission,
                now_ms,
                0,
                &|path| File::open(path)?.sync_all(),
            )
            .unwrap_err();
            assert!(
                failure
                    .to_string()
                    .contains("RDP examined-work limit exceeded"),
                "{forged_field}: {failure}"
            );
            publish_selected_month(&store, &key, &valid, &first.admission, now_ms).unwrap();
            let repaired: serde_json::Value = serde_json::from_slice(
                &current_receipt(&store, &key, binding.vehicle_id, &id)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(repaired["algorithm_version"], ALGORITHM_VERSION);
            assert_eq!(
                repaired["source"]["input_receipt_id"],
                first.admission.receipt_id
            );
            assert_ne!(repaired["signature"]["signature"], "invalid-signature");
        }
        store.open().unwrap().execute(
            "UPDATE prepared_map_months SET month = '1900-01', input_sequence = input_sequence + 1 WHERE artifact_id = ?1",
            [&id],
        ).unwrap();
        publish_selected_month(&store, &key, &valid, &first.admission, now_ms).unwrap();
        let restored: (String, i64) = store
            .open()
            .unwrap()
            .query_row(
                "SELECT month, input_sequence FROM prepared_map_months WHERE artifact_id = ?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(restored, (month.name, first.admission.head_sequence as i64));
    }

    #[tokio::test]
    async fn missing_pack_rebuilds_but_damaged_pack_never_bypasses_no_clobber() {
        let temporary = crate::private_tempdir().unwrap();
        let store = HubStore::initialize(temporary.path().join("hub")).unwrap();
        let binding = registered_admission_binding(&store);
        let key = CursorKey::from_bytes([0x45; 32]);
        let now_ms = OffsetDateTime::now_utc().unix_timestamp() * 1_000;
        let month = previous_complete_month(now_ms).unwrap().unwrap();
        let first = publish_sealed_physical_v3_stage(
            &store,
            &key,
            binding.clone(),
            month_stage(temporary.path(), &month, false, false),
        )
        .await
        .unwrap();
        let id = artifact_id(&first.admission, &month.name, month.from_ms, month.to_ms);
        let receipt = current_receipt(&store, &key, binding.vehicle_id, &id)
            .unwrap()
            .unwrap();
        let document: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
        let digest = document["pack"]["sha256"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let pack = current_pack(&store, &key, digest).unwrap().unwrap();
        let original_bytes = fs::read(&pack.path).unwrap();
        fs::remove_file(&pack.path).unwrap();
        let valid = month_stage(temporary.path(), &month, false, false);
        publish_selected_month(&store, &key, &valid, &first.admission, now_ms).unwrap();
        assert_eq!(fs::read(&pack.path).unwrap(), original_bytes);

        let damaged = vec![0_u8; original_bytes.len()];
        fs::write(&pack.path, &damaged).unwrap();
        let failure = publish_selected_month_with_policy(
            &store,
            &key,
            &valid,
            &first.admission,
            now_ms,
            0,
            &|path| File::open(path)?.sync_all(),
        )
        .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("RDP examined-work limit exceeded")
        );
        assert!(publish_selected_month(&store, &key, &valid, &first.admission, now_ms).is_err());
        assert_eq!(
            fs::read(&pack.path).unwrap(),
            damaged,
            "retain conflicting evidence"
        );
        require_current_source_head(&store.open().unwrap(), &first.admission).unwrap();
    }

    #[tokio::test]
    async fn work_exhaustion_and_namespace_sync_failure_do_not_publish_receipts() {
        let temporary = crate::private_tempdir().unwrap();
        let store = HubStore::initialize(temporary.path().join("hub")).unwrap();
        let binding = registered_admission_binding(&store);
        let key = CursorKey::from_bytes([0x46; 32]);
        let now_ms = OffsetDateTime::now_utc().unix_timestamp() * 1_000;
        let month = previous_complete_month(now_ms).unwrap().unwrap();
        let first = publish_sealed_physical_v3_stage(
            &store,
            &key,
            binding.clone(),
            month_stage(temporary.path(), &month, false, false),
        )
        .await
        .unwrap();
        let id = artifact_id(&first.admission, &month.name, month.from_ms, month.to_ms);
        let receipt = current_receipt(&store, &key, binding.vehicle_id, &id)
            .unwrap()
            .unwrap();
        let document: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
        let digest = document["pack"]["sha256"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let pack = current_pack(&store, &key, digest).unwrap().unwrap();
        store
            .open()
            .unwrap()
            .execute(
                "DELETE FROM prepared_map_months WHERE artifact_id = ?1",
                [&id],
            )
            .unwrap();
        fs::remove_file(&pack.path).unwrap();
        let valid = month_stage(temporary.path(), &month, false, false);
        let entries = |path: &Path| -> HashSet<PathBuf> {
            fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect()
        };
        let before_content = entries(&store.packs_dir().join("sha256"));
        let before_staging = entries(&store.packs_dir().join(".staging"));
        let failure = publish_selected_month_with_policy(
            &store,
            &key,
            &valid,
            &first.admission,
            now_ms,
            0,
            &|path| File::open(path)?.sync_all(),
        )
        .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("RDP examined-work limit exceeded")
        );
        assert_eq!(entries(&store.packs_dir().join("sha256")), before_content);
        assert_eq!(entries(&store.packs_dir().join(".staging")), before_staging);
        assert!(
            current_receipt(&store, &key, binding.vehicle_id, &id)
                .unwrap()
                .is_none()
        );
        require_current_source_head(&store.open().unwrap(), &first.admission).unwrap();

        let synced = std::cell::RefCell::new(Vec::new());
        let failure = publish_selected_month_with_policy(
            &store,
            &key,
            &valid,
            &first.admission,
            now_ms,
            RDP_EVALUATION_LIMIT,
            &|path| {
                synced.borrow_mut().push(path.to_path_buf());
                Err(io::Error::other(
                    "injected destination-directory sync failure",
                ))
            },
        )
        .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("injected destination-directory sync failure")
        );
        assert_eq!(*synced.borrow(), vec![store.packs_dir().join("sha256")]);
        assert!(pack.path.is_file(), "unadvertised object can be retried");
        assert!(
            current_receipt(&store, &key, binding.vehicle_id, &id)
                .unwrap()
                .is_none()
        );
        require_current_source_head(&store.open().unwrap(), &first.admission).unwrap();
        publish_selected_month(&store, &key, &valid, &first.admission, now_ms).unwrap();
        assert!(
            current_receipt(&store, &key, binding.vehicle_id, &id)
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn signed_month_survives_restart_noop_and_changed_head() {
        signed_month_contract(false).await;
    }

    #[tokio::test]
    #[ignore = "requires TESLATLAS_PREPARED_PROTOCOL_VALIDATOR; run explicitly with --ignored"]
    async fn required_protocol_validator_accepts_signed_prepared_month() {
        std::env::var_os("TESLATLAS_PREPARED_PROTOCOL_VALIDATOR")
            .expect("required Protocol acceptance needs TESLATLAS_PREPARED_PROTOCOL_VALIDATOR");
        signed_month_contract(true).await;
    }

    async fn signed_month_contract(require_protocol: bool) {
        let temporary = crate::private_tempdir().expect("temporary");
        let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
        let binding = registered_admission_binding(&store);
        let key = CursorKey::from_bytes([0x42; 32]);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as i64;
        let month = previous_complete_month(now_ms)
            .expect("month")
            .expect("interior month");
        let first = publish_sealed_physical_v3_stage(
            &store,
            &key,
            binding.clone(),
            month_stage(temporary.path(), &month, false, false),
        )
        .await
        .expect("first head");
        assert_eq!(first.kind, PhysicalV3PublicationKind::FirstHead);
        let first_id = artifact_id(&first.admission, &month.name, month.from_ms, month.to_ms);
        let receipt = current_receipt(&store, &key, binding.vehicle_id, &first_id)
            .expect("receipt query")
            .expect("published receipt");
        let document: serde_json::Value = serde_json::from_slice(&receipt).expect("receipt JSON");
        assert_eq!(
            document["source"]["input_receipt_id"],
            first.admission.receipt_id
        );
        assert_eq!(document["algorithm_version"], ALGORITHM_VERSION);
        assert_eq!(
            document["dirty_spans"]["map_months"][0]["month"],
            month.name
        );
        let digest = document["pack"]["sha256"]
            .as_str()
            .expect("digest")
            .parse::<Sha256Digest>()
            .expect("sha256");
        let stored = current_pack(&store, &key, digest)
            .expect("pack lookup")
            .expect("pack");
        assert_eq!(
            stored.compressed_bytes,
            fs::metadata(&stored.path).unwrap().len()
        );
        if require_protocol {
            let python = std::env::var_os("TESLATLAS_PREPARED_PROTOCOL_VALIDATOR")
                .expect("required Protocol validator gate");
            let receipt_file = NamedTempFile::new_in(temporary.path()).unwrap();
            fs::write(receipt_file.path(), &receipt).unwrap();
            let protocol_root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("teslatlas-protocol");
            let script = "import json,sys\n".to_owned()
                + "sys.path.insert(0,sys.argv[1])\n"
                + "from conformance import hub_sync\n"
                + "r=json.load(open(sys.argv[2],'rb'))\n"
                + "p=open(sys.argv[3],'rb').read()\n"
                + "keys=hub_sync.load_signing_keys(sys.argv[4])\n"
                + "keys['vehicle_id']=r['vehicle_id']\n"
                + "keys['key_set_id']=r['signature']['key_id']\n"
                + "keys['keys']=[{'algorithm':'ed25519','key_id':r['signature']['key_id'],'public_key':sys.argv[5]}]\n"
                + "hub_sync.load_signing_keys=lambda root:keys\n"
                + "e=hub_sync.validate_prepared_pack(r,p,r['vehicle_id'],sys.argv[4])\n"
                + "assert not e,e\n";
            let public_key = ManifestSigning::from_cursor_key(&key).verifying_key_base64();
            let status = std::process::Command::new(python)
                .arg("-c")
                .arg(script)
                .arg(&protocol_root)
                .arg(receipt_file.path())
                .arg(&stored.path)
                .arg(protocol_root.join("profiles/hub-sync-v1/1.3.0"))
                .arg(public_key)
                .status()
                .expect("Protocol validator starts");
            assert!(status.success(), "Protocol rejected Hub prepared pack");
        }
        let raw = zstd::stream::decode_all(File::open(&stored.path).unwrap()).unwrap();
        let image = NamedTempFile::new_in(temporary.path()).unwrap();
        fs::write(image.path(), raw).unwrap();
        let prepared = Connection::open(image.path()).unwrap();
        assert_eq!(
            prepared
                .query_row("SELECT count(*) FROM map_months", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1,
        );
        assert!(
            prepared
                .query_row("SELECT count(*) FROM map_tiles", [], |r| r.get::<_, i64>(0))
                .unwrap()
                > 0
        );
        drop(prepared);
        let reopened = HubStore::open_existing(temporary.path().join("hub")).expect("restart");
        assert_eq!(
            current_receipt(&reopened, &key, binding.vehicle_id, &first_id).unwrap(),
            Some(receipt.clone()),
        );
        reopened
            .open()
            .unwrap()
            .execute(
                "DELETE FROM prepared_map_months WHERE artifact_id = ?1",
                [&first_id],
            )
            .expect("simulate an optional prepared publication skipped by a prior binary");
        assert!(
            current_receipt(&reopened, &key, binding.vehicle_id, &first_id)
                .unwrap()
                .is_none()
        );

        let unchanged = publish_sealed_physical_v3_stage(
            &reopened,
            &key,
            binding.clone(),
            month_stage(temporary.path(), &month, false, false),
        )
        .await
        .expect("signed no-op");
        assert_eq!(unchanged.kind, PhysicalV3PublicationKind::Unchanged);
        assert_eq!(
            artifact_id(
                &unchanged.admission,
                &month.name,
                month.from_ms,
                month.to_ms
            ),
            first_id
        );
        let count: i64 = reopened
            .open()
            .unwrap()
            .query_row("SELECT count(*) FROM prepared_map_months", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert!(
            current_receipt(&reopened, &key, binding.vehicle_id, &first_id)
                .unwrap()
                .is_some(),
            "a same-head import retries the absent optional prepared month",
        );
        reopened
            .open()
            .unwrap()
            .execute(
                "UPDATE prepared_map_months SET pack_sha256 = ?1, compressed_bytes = 1
             WHERE artifact_id = ?2",
                params!["0".repeat(64), first_id],
            )
            .expect("simulate an older invalid cached prepared pack");
        let repaired = publish_sealed_physical_v3_stage(
            &reopened,
            &key,
            binding.clone(),
            month_stage(temporary.path(), &month, false, false),
        )
        .await
        .expect("same-head prepared repair");
        assert_eq!(repaired.kind, PhysicalV3PublicationKind::Unchanged);
        let repaired_digest: String = reopened
            .open()
            .unwrap()
            .query_row(
                "SELECT pack_sha256 FROM prepared_map_months WHERE artifact_id = ?1",
                [&first_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repaired_digest, digest.to_string());
        let repaired_receipt = current_receipt(&reopened, &key, binding.vehicle_id, &first_id)
            .unwrap()
            .unwrap();
        let repaired_document: serde_json::Value =
            serde_json::from_slice(&repaired_receipt).unwrap();
        assert_eq!(repaired_document["pack"]["sha256"], digest.to_string());
        assert!(current_pack(&reopened, &key, digest).unwrap().is_some());

        let changed = publish_sealed_physical_v3_stage(
            &reopened,
            &key,
            binding.clone(),
            month_stage(temporary.path(), &month, true, false),
        )
        .await
        .expect("changed head");
        assert_eq!(changed.kind, PhysicalV3PublicationKind::Rotation);
        let changed_id = artifact_id(&changed.admission, &month.name, month.from_ms, month.to_ms);
        assert_ne!(changed_id, first_id);
        assert!(
            current_receipt(&reopened, &key, binding.vehicle_id, &first_id)
                .unwrap()
                .is_none()
        );
        assert!(
            current_receipt(&reopened, &key, binding.vehicle_id, &changed_id)
                .unwrap()
                .is_some()
        );
        let changed_receipt = current_receipt(&reopened, &key, binding.vehicle_id, &changed_id)
            .unwrap()
            .unwrap();
        let changed_document: serde_json::Value = serde_json::from_slice(&changed_receipt).unwrap();
        let changed_digest = changed_document["pack"]["sha256"]
            .as_str()
            .unwrap()
            .parse::<Sha256Digest>()
            .unwrap();
        let changed_pack = current_pack(&reopened, &key, changed_digest)
            .unwrap()
            .unwrap();
        reopened
            .repair()
            .expect("repair preserves current and retained prepared packs");
        assert!(
            stored.path.exists(),
            "retained prepared pack survives repair"
        );
        assert!(
            changed_pack.path.exists(),
            "current prepared pack survives repair"
        );
        let backup_root = temporary.path().join("backup");
        reopened
            .backup_to(&backup_root)
            .expect("prepared-pack backup");
        let restored = HubStore::initialize(&backup_root).expect("restore prepared catalogue");
        assert_eq!(
            current_receipt(&restored, &key, binding.vehicle_id, &changed_id).unwrap(),
            Some(changed_receipt),
        );
        assert!(
            backup_root
                .join("packs/sha256")
                .join(stored.path.file_name().unwrap())
                .exists(),
            "retained prepared pack survives backup",
        );
        assert!(
            backup_root
                .join("packs/sha256")
                .join(changed_pack.path.file_name().unwrap())
                .exists(),
            "current prepared pack survives backup",
        );
        assert!(
            current_receipt(&reopened, &key, Uuid::new_v4(), &changed_id)
                .unwrap()
                .is_none()
        );
        let non_ascii_id = format!("map-month-v1.é{}", "a".repeat(62));
        assert!(
            current_receipt(&reopened, &key, binding.vehicle_id, &non_ascii_id)
                .unwrap()
                .is_none()
        );
        let non_map = publish_sealed_physical_v3_stage(
            &reopened,
            &key,
            binding.clone(),
            month_stage(temporary.path(), &month, true, true),
        )
        .await
        .expect("non-map head");
        assert_eq!(non_map.kind, PhysicalV3PublicationKind::Rotation);
        let non_map_id = artifact_id(&non_map.admission, &month.name, month.from_ms, month.to_ms);
        let non_map_receipt = current_receipt(&reopened, &key, binding.vehicle_id, &non_map_id)
            .unwrap()
            .expect("new head has its own signed prepared receipt");
        let non_map_document: serde_json::Value = serde_json::from_slice(&non_map_receipt).unwrap();
        assert_eq!(
            non_map_document["source"]["input_receipt_id"],
            non_map.admission.receipt_id
        );
        assert_ne!(
            non_map_document["pack"]["sha256"],
            document["pack"]["sha256"]
        );
        let (changed_source, non_map_source): (String, String) = reopened
            .open()
            .unwrap()
            .query_row(
                "SELECT a.source_digest, b.source_digest FROM prepared_map_months a,
                 prepared_map_months b WHERE a.artifact_id = ?1 AND b.artifact_id = ?2",
                params![changed_id, non_map_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            changed_source, non_map_source,
            "unchanged map tiles remain identical"
        );
        let count: i64 = reopened
            .open()
            .unwrap()
            .query_row("SELECT count(*) FROM prepared_map_months", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);

        reopened
            .open()
            .unwrap()
            .execute(
                "UPDATE retained_physical_v3_admissions
             SET retained_at_ms = 1, expires_at_ms = 2 WHERE receipt_id = ?1",
                [&first.admission.receipt_id],
            )
            .expect("expire only the first retained head without another import");
        reopened
            .repair()
            .expect("expiry repair without new publication");
        let remaining: i64 = reopened
            .open()
            .unwrap()
            .query_row("SELECT count(*) FROM prepared_map_months", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 2, "expired prepared catalogue entry is pruned");
        assert!(!stored.path.exists(), "expired prepared pack is reclaimed");
        assert!(
            changed_pack.path.exists(),
            "unexpired retained pack remains"
        );
        assert!(
            current_receipt(&reopened, &key, binding.vehicle_id, &non_map_id)
                .unwrap()
                .is_some()
        );
    }
}
