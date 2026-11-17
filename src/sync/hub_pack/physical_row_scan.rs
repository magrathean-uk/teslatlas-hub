// SPDX-License-Identifier: AGPL-3.0-only

// Internal, bounded reader for already signed PhysicalV3 full packs.

use rusqlite::types::ValueRef;

/// One verification (or stage write) a test observed on this thread
/// (M08-T19): where it was made, of what (a pack's digest or a stage's
/// path) and whether it passed.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verification {
    pub(crate) place: &'static str,
    pub(crate) object: String,
    pub(crate) ok: bool,
}

#[cfg(test)]
thread_local! {
    static VERIFICATIONS: std::cell::RefCell<Vec<Verification>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Records one verification at a real boundary (M08-T19 tests). The places:
/// `stage-integrity` (a stage's full SQLite check), `stage-write` (an edit of
/// a sealed stage committed), `stage-emit` (a publication writes packs from
/// a stage), `pack-decode` (a pack read authenticated over the bytes it
/// decodes), `pack-reader` (the protocol's verification of a compressed
/// pack), `admission-pack-digest`, `retained-pack-digest` and
/// `delta-pack-digest` (a pack file hashed against the catalogue),
/// `pack-sqlite-integrity` (a decoded or written pack's SQLite check), and
/// the markers `compare` and `context-copy` (a changed set's two readers
/// begin).
#[cfg(test)]
pub(crate) fn record_verification(place: &'static str, object: impl ToString, ok: bool) {
    VERIFICATIONS.with(|log| {
        log.borrow_mut().push(Verification { place, object: object.to_string(), ok });
    });
}

/// The verifications recorded on this thread since the last call.
#[cfg(test)]
pub(crate) fn take_verifications() -> Vec<Verification> {
    VERIFICATIONS.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

/// The verifications recorded on this thread, kept.
#[cfg(test)]
pub(crate) fn verifications() -> Vec<Verification> {
    VERIFICATIONS.with(|log| log.borrow().clone())
}

pub(crate) const PHYSICAL_TABLES_2_2: [&str; 11] = [
    "global_settings",
    "car_settings",
    "cars",
    "addresses",
    "geofences",
    "drives",
    "positions",
    "charging_processes",
    "charges",
    "states",
    "updates",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalTypedRow {
    pub table: &'static str,
    pub id: i64,
    pub digest: [u8; 32],
    pub edges: Vec<(&'static str, i64)>,
    pub closed_drive: Option<bool>,
    pub update_start_pg_us: Option<i64>,
    pub month_date_pg_us: Option<i64>,
}

#[cfg(test)]
thread_local! {
    /// Gives the next decodes of packs other bytes than their files hold
    /// (M08-T19 tests: a view that differs from the bytes hashed before).
    pub(crate) static PACK_DECODE_VIEW: std::cell::RefCell<
        Option<Box<dyn FnMut(&Path) -> Option<Vec<u8>>>>,
    > = const { std::cell::RefCell::new(None) };
}

/// How a reader verifies a pack it decodes (M08-T19). In both, the content
/// digest is computed over the very bytes that are decoded, in the same
/// read, and compared with the manifest's before any row is used; an
/// earlier hash of the same path does not stand for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackCheck {
    /// The decoded SQLite also gets a full integrity check.
    Full,
    /// A pack of the Hub's own heads, integrity-checked when it was written:
    /// bytes with its digest are those bytes, so the decoded SQLite is not
    /// integrity-checked again. Length, identity and layout are checked.
    Authenticated,
}

/// Counts and hashes the bytes a decoder reads.
struct DigestingReader<R> {
    inner: R,
    hash: Sha256,
    bytes: u64,
}

impl<R: Read> Read for DigestingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hash.update(&buffer[..read]);
        self.bytes += read as u64;
        Ok(read)
    }
}

/// Decodes `source` into `output` and authenticates the bytes decoded: their
/// length and SHA-256 must be the manifest's, and the decoded length too.
fn decode_authenticated(
    pack: &TransportPack,
    source: impl Read,
    output: &mut File,
) -> Result<(), ProjectionPackError> {
    let limit = pack.compressed_bytes.checked_add(1).ok_or(ProjectionPackError::CapacityOverflow)?;
    let source = DigestingReader { inner: source.take(limit), hash: Sha256::new(), bytes: 0 };
    let mut decoder = zstd::stream::read::Decoder::new(source).map_err(ProjectionPackError::Decompress)?;
    let maximum = pack.uncompressed_bytes.checked_add(1).ok_or(ProjectionPackError::CapacityOverflow)?;
    let decoded = io::copy(&mut (&mut decoder).take(maximum), output)
        .map_err(ProjectionPackError::Decompress)?;
    // The rest of the input, if any, is hashed too: the digest is of all of it.
    let mut source = decoder.finish().into_inner();
    io::copy(&mut source, &mut io::sink()).map_err(ProjectionPackError::Decompress)?;
    let authentic = source.bytes == pack.compressed_bytes
        && Sha256Digest::from_bytes(source.hash.finalize().into()) == pack.sha256;
    #[cfg(test)]
    record_verification("pack-decode", pack.sha256, authentic);
    if source.bytes != pack.compressed_bytes {
        return Err(ProjectionPackError::Protocol(ProtocolError::CompressedSizeMismatch {
            expected: pack.compressed_bytes,
            actual: source.bytes,
        }));
    }
    if !authentic {
        return Err(ProjectionPackError::Protocol(ProtocolError::PackHashMismatch));
    }
    if decoded != pack.uncompressed_bytes {
        return Err(invalid("physical pack decoded length is invalid"));
    }
    Ok(())
}

pub(crate) struct VerifiedPhysicalPackSqlite {
    connection: Connection,
    _staging: StagedFile,
}

impl VerifiedPhysicalPackSqlite {
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }
}

pub(crate) fn open_verified_physical_pack_2_2(
    pack: &TransportPack,
    manifest: &SyncManifest,
    binding: &ProjectionBinding,
    path: &Path,
    check: PackCheck,
) -> Result<VerifiedPhysicalPackSqlite, ProjectionPackError> {
    pack.validate(ProtocolLimits::hub_sync_v1_1_3_schema_2_2())
        .map_err(ProjectionPackError::Protocol)?;
    let file = File::open(path).map_err(|source| ProjectionPackError::OpenCompressed {
        path: path.to_path_buf(),
        source,
    })?;
    let length = file
        .metadata()
        .map_err(|source| ProjectionPackError::ReadSource {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    if length != pack.compressed_bytes {
        return Err(invalid("physical pack compressed length is invalid"));
    }
    let staging_dir = path
        .parent()
        .and_then(Path::parent)
        .map(|packs| packs.join(".staging"))
        .ok_or_else(|| invalid("physical pack has no staging directory"))?;
    ensure_private_staging_directory(&staging_dir)?;
    let sqlite = StagedFile::create(&staging_dir, "physical-scan.sqlite")?;
    let mut output = OpenOptions::new().write(true).truncate(true).open(sqlite.path())
        .map_err(|source| ProjectionPackError::CreateTemporary {
            path: sqlite.path().to_path_buf(), source,
        })?;
    #[cfg(test)]
    if let Some(view) = PACK_DECODE_VIEW.with(|view| view.borrow_mut().as_mut().and_then(|view| view(path))) {
        decode_authenticated(pack, io::Cursor::new(view), &mut output)?;
    } else {
        decode_authenticated(pack, file, &mut output)?;
    }
    #[cfg(not(test))]
    decode_authenticated(pack, file, &mut output)?;
    drop(output);
    verify_projection_sqlite_2_2_publication_identity(sqlite.path(), pack, manifest, binding)?;
    let connection = Connection::open_with_flags(
        sqlite.path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(ProjectionPackError::OpenSqlite)?;
    connection.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA query_only = ON;")
        .map_err(ProjectionPackError::ConfigureSqlite)?;
    verify_physical_scan_layout(&connection, check == PackCheck::Full)?;
    Ok(VerifiedPhysicalPackSqlite { connection, _staging: sqlite })
}

/// The callback is invoked once per row, so an 11-million-row source never
/// becomes a Vec in memory. `pack` is opened only by its content digest.
pub(crate) fn scan_verified_physical_pack_2_2(
    pack: &TransportPack,
    manifest: &SyncManifest,
    binding: &ProjectionBinding,
    path: &Path,
    check: PackCheck,
    mut visit: impl FnMut(PhysicalTypedRow) -> Result<(), ProjectionPackError>,
) -> Result<(), ProjectionPackError> {
    let verified = open_verified_physical_pack_2_2(pack, manifest, binding, path, check)?;
    let connection = verified.connection();
    let mut total = 0_u64;
    for table in PHYSICAL_TABLES_2_2 {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY id"))
            .map_err(ProjectionPackError::Prepare)?;
        let edges = edge_columns(&statement, table)?;
        let end_date = if matches!(table, "drives" | "charging_processes") {
            Some(
                statement
                    .column_index("end_date_pg_us")
                    .map_err(ProjectionPackError::Prepare)?,
            )
        } else {
            None
        };
        let update_start = if table == "updates" {
            Some(
                statement
                    .column_index("start_date_pg_us")
                    .map_err(ProjectionPackError::Prepare)?,
            )
        } else {
            None
        };
        let month_date = match table {
            "drives" | "charging_processes" => Some(
                statement.column_index("start_date_pg_us").map_err(ProjectionPackError::Prepare)?
            ),
            "positions" | "charges" => Some(
                statement.column_index("date_pg_us").map_err(ProjectionPackError::Prepare)?
            ),
            _ => None,
        };
        let columns = statement.column_count();
        let mut rows = statement
            .query([])
            .map_err(ProjectionPackError::IntegrityCheck)?;
        while let Some(row) = rows.next().map_err(ProjectionPackError::IntegrityCheck)? {
            let id = row
                .get::<_, i64>(0)
                .map_err(ProjectionPackError::IntegrityCheck)?;
            let digest = physical_row_digest(table, row, columns)?;
            let mut related = Vec::with_capacity(edges.len());
            for (target, index) in &edges {
                if let Some(id) = row
                    .get::<_, Option<i64>>(*index)
                    .map_err(ProjectionPackError::IntegrityCheck)?
                {
                    related.push((*target, id));
                }
            }
            let closed_drive = end_date
                .map(|index| {
                    row.get::<_, Option<i64>>(index)
                        .map(|value| value.is_some())
                })
                .transpose()
                .map_err(ProjectionPackError::IntegrityCheck)?;
            let update_start_pg_us = update_start
                .map(|index| row.get::<_, i64>(index))
                .transpose()
                .map_err(ProjectionPackError::IntegrityCheck)?;
            let month_date_pg_us = month_date
                .map(|index| row.get::<_, i64>(index))
                .transpose()
                .map_err(ProjectionPackError::IntegrityCheck)?;
            visit(PhysicalTypedRow {
                table,
                id,
                digest,
                edges: related,
                closed_drive,
                update_start_pg_us,
                month_date_pg_us,
            })?;
            total = total
                .checked_add(1)
                .ok_or(ProjectionPackError::TooManyRows)?;
            if total > pack.row_count {
                return Err(invalid("physical pack row count exceeded"));
            }
        }
    }
    if total != pack.row_count {
        return Err(invalid("physical pack row count differs"));
    }
    Ok(())
}

fn edge_columns(
    statement: &rusqlite::Statement<'_>,
    table: &str,
) -> Result<Vec<(&'static str, usize)>, ProjectionPackError> {
    let names: &[(&str, &str)] = match table {
        "cars" => &[("car_settings", "settings_id")],
        "drives" => &[
            ("positions", "start_position_id"),
            ("positions", "end_position_id"),
            ("addresses", "start_address_id"),
            ("addresses", "end_address_id"),
            ("geofences", "start_geofence_id"),
            ("geofences", "end_geofence_id"),
        ],
        "positions" => &[("drives", "drive_id")],
        "charging_processes" => &[
            ("positions", "position_id"),
            ("addresses", "address_id"),
            ("geofences", "geofence_id"),
        ],
        "charges" => &[("charging_processes", "charging_process_id")],
        _ => &[],
    };
    names
        .iter()
        .map(|(target, column)| {
            statement
                .column_index(column)
                .map(|index| (*target, index))
                .map_err(ProjectionPackError::Prepare)
        })
        .collect()
}

fn physical_row_digest(
    table: &str,
    row: &rusqlite::Row<'_>,
    columns: usize,
) -> Result<[u8; 32], ProjectionPackError> {
    let mut hash = Sha256::new();
    hash.update(b"teslatlas-physical-v3-typed-row/v1\0"); // rename:keep
    hash.update((table.len() as u16).to_be_bytes());
    hash.update(table.as_bytes());
    hash.update((columns as u16).to_be_bytes());
    for index in 0..columns {
        match row
            .get_ref(index)
            .map_err(ProjectionPackError::IntegrityCheck)?
        {
            ValueRef::Null => hash.update([0]),
            ValueRef::Integer(value) => {
                hash.update([1]);
                hash.update(value.to_be_bytes());
            }
            ValueRef::Text(value) => {
                hash.update([2]);
                hash.update((value.len() as u64).to_be_bytes());
                hash.update(value);
            }
            ValueRef::Blob(value) => {
                if value.len() != 8 {
                    return Err(invalid("physical IEEE-754 blob width is invalid"));
                }
                hash.update([3]);
                hash.update((value.len() as u64).to_be_bytes());
                hash.update(value);
            }
            ValueRef::Real(_) => return Err(invalid("physical pack contains an untyped REAL")),
        }
    }
    Ok(hash.finalize().into())
}

fn verify_physical_scan_layout(
    connection: &Connection,
    integrity: bool,
) -> Result<(), ProjectionPackError> {
    let app_id: u32 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if app_id != SQLITE_HUB_PROJECTION_APPLICATION_ID
        || version != HUB_PROJECTION_SCHEMA_V3.sqlite_user_version()
    {
        return Err(invalid("physical pack SQLite header is invalid"));
    }
    let mut tables = connection.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    ).map_err(ProjectionPackError::Prepare)?;
    let actual = tables
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(ProjectionPackError::IntegrityCheck)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if actual
        != [
            "addresses",
            "car_settings",
            "cars",
            "charges",
            "charging_processes",
            "drives",
            "geofences",
            "global_settings",
            "hub_pack_metadata",
            "positions",
            "states",
            "updates",
        ]
    {
        return Err(invalid("physical pack SQLite table set is invalid"));
    }
    verify_projection_table_layout(
        connection,
        "hub_pack_metadata",
        false,
        &[("key", "TEXT", true, true), ("value", "TEXT", true, false)],
    )?;
    for (table, ddl) in [
        ("global_settings", THP2_2_GLOBAL_SETTINGS_SQLITE_DDL),
        ("car_settings", THP2_2_CAR_SETTINGS_SQLITE_DDL),
        ("cars", THP2_2_CARS_SQLITE_DDL),
        ("addresses", THP2_2_ADDRESSES_SQLITE_DDL),
        ("geofences", THP2_2_GEOFENCES_SQLITE_DDL),
        ("drives", THP2_2_DRIVES_SQLITE_DDL),
        ("positions", THP2_2_POSITIONS_SQLITE_DDL),
        ("charging_processes", THP2_2_CHARGING_PROCESSES_SQLITE_DDL),
        ("charges", THP2_2_CHARGES_SQLITE_DDL),
        ("states", THP2_2_STATES_SQLITE_DDL),
        ("updates", THP2_2_UPDATES_SQLITE_DDL),
    ] {
        verify_projection_table_ddl(connection, table, ddl)?;
    }
    if !integrity {
        return Ok(());
    }
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(ProjectionPackError::IntegrityCheck)?;
    #[cfg(test)]
    record_verification("pack-sqlite-integrity", "scan", integrity == "ok");
    if integrity != "ok" {
        return Err(ProjectionPackError::IntegrityFailure);
    }
    Ok(())
}
