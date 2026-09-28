// SPDX-License-Identifier: AGPL-3.0-only

//! One optional, signed prepared month from an admitted PhysicalV3 source.
//! Failure here never revokes the already-published history head.

use std::{
    collections::HashSet,
    error::Error,
    fs::{self, File, OpenOptions},
    io::{Read, Write, copy},
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
    TILE_GEOMETRY_VERSION, TileOutput, generate_tile_payload_v1_from_pages,
    prepare_positions_for_tile_rendering_v1,
};
use time::{OffsetDateTime, Time};
use uuid::Uuid;

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

fn compute_month(scratch: &Connection, drives: &[(i64, i32)]) -> PreparedResult<TileOutput> {
    let cancelled = AtomicBool::new(false);
    let mut failure: Option<Box<dyn Error + Send + Sync>> = None;
    let output = generate_tile_payload_v1_from_pages(&cancelled, None, |consume| {
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
    });
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
    match compressed.persist_noclobber(&destination) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut existing = File::open(&destination)?;
            let mut existing_hash = Sha256::new();
            let mut existing_size = 0_u64;
            loop {
                let read = existing.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                existing_size += u64::try_from(read)?;
                existing_hash.update(&buffer[..read]);
            }
            if existing_size != bytes || hex::encode(existing_hash.finalize()) != digest {
                return Err(invalid("prepared content-addressed pack conflicts"));
            }
        }
        Err(error) => return Err(error.error.into()),
    }
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
    let Some(month) = previous_complete_month(now_ms)? else {
        return Ok(());
    };
    let id = artifact_id(admission, &month.name, month.from_ms, month.to_ms);
    let mut catalogue = store.open()?;
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
    let previous: Option<(String, String, String)> = catalogue
        .query_row(
            "SELECT vehicle_id, input_receipt_id, pack_sha256
         FROM prepared_map_months WHERE artifact_id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((vehicle, source_receipt, _)) = &previous
        && (vehicle != &admission.vehicle_id.to_string() || source_receipt != &admission.receipt_id)
    {
        return Err(invalid(
            "prepared artifact identity conflicts with its source head",
        ));
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
    let output = compute_month(&scratch, &drives)?;
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
    let (digest, compressed_bytes, _pack_path) =
        compressed_pack(store, &staging, &sqlite_file, uncompressed_bytes)?;
    if previous
        .as_ref()
        .is_some_and(|(_, _, prior_digest)| prior_digest == &digest)
    {
        return Ok(());
    }
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
    let transaction = catalogue.transaction()?;
    transaction.execute(
        "INSERT INTO prepared_map_months
         (artifact_id, vehicle_id, month, input_manifest_id, input_receipt_id,
          input_sequence, source_digest, pack_sha256, compressed_bytes, receipt_json)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(artifact_id) DO UPDATE SET
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

/// Resolve only a receipt from the current public PhysicalV3 head and exact
/// vehicle. Older prepared receipts remain private after a head replacement.
pub(crate) fn current_receipt(
    store: &HubStore,
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
            "SELECT p.receipt_json FROM prepared_map_months AS p
         JOIN pending_physical_v3_admissions AS h
           ON h.vehicle_id = p.vehicle_id AND h.receipt_id = p.input_receipt_id
         WHERE p.vehicle_id = ?1 AND p.artifact_id = ?2
           AND h.serve_state = 'public_first'",
            params![vehicle_id.to_string(), id],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    Ok(result)
}

/// Authorize content-addressed prepared objects through the current signed
/// receipt catalogue before the shared pack streamer opens any file.
pub(crate) fn current_pack(
    store: &HubStore,
    digest: Sha256Digest,
) -> PreparedResult<Option<StoredPack>> {
    let connection = store.open()?;
    let entry = connection
        .query_row(
            "SELECT p.compressed_bytes FROM prepared_map_months AS p
         JOIN pending_physical_v3_admissions AS h
           ON h.vehicle_id = p.vehicle_id AND h.receipt_id = p.input_receipt_id
         WHERE p.pack_sha256 = ?1 AND h.serve_state = 'public_first'
         LIMIT 1",
            [digest.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
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

    fn source_us(unix_ms: i64) -> i64 {
        unix_ms * 1_000 - PG_EPOCH_OFFSET_US
    }

    #[test]
    fn real_prepared_pack_pointer_gap_cleanup_preserves_rows() {
        let Some(pack_path) = std::env::var_os("TESLATLAS_PREPARED_REAL_PACK") else {
            return;
        };
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
        for index in 0..3 {
            let position = TeslaMatePositionPhysicalV2_2 {
                id: 40 + index,
                car_id: 1,
                drive_id: Some(30),
                date_pg_us: source_us(month.from_ms + 3_600_000 + i64::from(index) * 60_000),
                latitude_e6: ProjectionFixedNumericV2_2::Finite(
                    51_000_000 + i64::from(index) * if changed { 2_000 } else { 1_000 },
                ),
                longitude_e6: ProjectionFixedNumericV2_2::Finite(
                    -100_000 + i64::from(index) * 1_000,
                ),
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
        let open_receipt = current_receipt(&store, binding.vehicle_id, &open_id)
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
    async fn signed_month_survives_restart_noop_and_changed_head() {
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
        let receipt = current_receipt(&store, binding.vehicle_id, &first_id)
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
        let stored = current_pack(&store, digest)
            .expect("pack lookup")
            .expect("pack");
        assert_eq!(
            stored.compressed_bytes,
            fs::metadata(&stored.path).unwrap().len()
        );
        if let Some(python) = std::env::var_os("TESLATLAS_PREPARED_PROTOCOL_VALIDATOR") {
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
            current_receipt(&reopened, binding.vehicle_id, &first_id).unwrap(),
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
            current_receipt(&reopened, binding.vehicle_id, &first_id)
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
            current_receipt(&reopened, binding.vehicle_id, &first_id)
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
        let repaired_receipt = current_receipt(&reopened, binding.vehicle_id, &first_id)
            .unwrap()
            .unwrap();
        let repaired_document: serde_json::Value =
            serde_json::from_slice(&repaired_receipt).unwrap();
        assert_eq!(repaired_document["pack"]["sha256"], digest.to_string());
        assert!(current_pack(&reopened, digest).unwrap().is_some());

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
            current_receipt(&reopened, binding.vehicle_id, &first_id)
                .unwrap()
                .is_none()
        );
        assert!(
            current_receipt(&reopened, binding.vehicle_id, &changed_id)
                .unwrap()
                .is_some()
        );
        let changed_receipt = current_receipt(&reopened, binding.vehicle_id, &changed_id)
            .unwrap()
            .unwrap();
        let changed_document: serde_json::Value = serde_json::from_slice(&changed_receipt).unwrap();
        let changed_digest = changed_document["pack"]["sha256"]
            .as_str()
            .unwrap()
            .parse::<Sha256Digest>()
            .unwrap();
        let changed_pack = current_pack(&reopened, changed_digest).unwrap().unwrap();
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
            current_receipt(&restored, binding.vehicle_id, &changed_id).unwrap(),
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
            current_receipt(&reopened, Uuid::new_v4(), &changed_id)
                .unwrap()
                .is_none()
        );
        let non_ascii_id = format!("map-month-v1.é{}", "a".repeat(62));
        assert!(
            current_receipt(&reopened, binding.vehicle_id, &non_ascii_id)
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
        let non_map_receipt = current_receipt(&reopened, binding.vehicle_id, &non_map_id)
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
            current_receipt(&reopened, binding.vehicle_id, &non_map_id)
                .unwrap()
                .is_some()
        );
    }
}
