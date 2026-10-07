// SPDX-License-Identifier: AGPL-3.0-only

//! Parity and measurement of the test-only original inserter against the production prepared inserter.

use super::*;
use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, os::unix::fs::MetadataExt, path::PathBuf, time::Instant};

const SCRATCH_BYTES: u64 = 512 * 1024 * 1024;
const MINIMUM_FREE_BYTES: u64 = 30 * 1024 * 1024 * 1024;
const MAX_SELECTED_DECODED_BYTES: u64 = 256 * 1024 * 1024;

// Exact production scratch shape and configuration, with a smaller page ceiling
// for the bounded population component. Neither inserter changes this schema.
fn scratch(
    directory: &Path,
    maximum: u64,
    minimum_free: u64,
) -> Result<PhysicalDeltaComparison, PhysicalCompareError> {
    ensure_private_staging_directory(directory)?;
    let required = maximum
        .checked_add(ProtocolLimits::hub_sync_v1_1_3_schema_2_2().max_uncompressed_pack_bytes)
        .and_then(|v| v.checked_add(minimum_free))
        .ok_or(PhysicalCompareError::Capacity)?;
    if maximum < 4096 || available_bytes(directory)? < required {
        return Err(PhysicalCompareError::Capacity);
    }
    let scratch = tempfile::Builder::new()
        .prefix("physical-compare-measurement-")
        .suffix(".sqlite")
        .tempfile_in(directory)?;
    let connection = Connection::open(scratch.path())?;
    connection.execute_batch(
        "PRAGMA trusted_schema=OFF; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;
             PRAGMA temp_store=MEMORY; PRAGMA automatic_index=OFF;
             PRAGMA cache_size=-32768; PRAGMA page_size=4096;",
    )?;
    connection.pragma_update(
        None,
        "max_page_count",
        i64::try_from(maximum / 4096).map_err(|_| PhysicalCompareError::Capacity)?,
    )?;
    connection.execute_batch(
        "CREATE TABLE row_versions (
                 side INTEGER NOT NULL, table_name TEXT NOT NULL, id INTEGER NOT NULL,
                 digest BLOB NOT NULL CHECK(length(digest)=32), pack_ordinal INTEGER NOT NULL,
                 closed_drive INTEGER, update_start_pg_us INTEGER, month_date_pg_us INTEGER,
                 PRIMARY KEY(side,table_name,id)
             ) WITHOUT ROWID;
             CREATE INDEX row_versions_latest_update
               ON row_versions(update_start_pg_us DESC,id DESC)
               WHERE side=1 AND table_name='updates';
             CREATE TABLE edges (
                 side INTEGER NOT NULL, from_table TEXT NOT NULL, from_id INTEGER NOT NULL,
                 to_table TEXT NOT NULL, to_id INTEGER NOT NULL,
                 PRIMARY KEY(from_table,from_id,side,to_table,to_id)
             ) WITHOUT ROWID;
             CREATE INDEX edges_target ON edges(to_table,to_id,side,from_table,from_id);
             CREATE TABLE changed_raw (
                 table_name TEXT NOT NULL, id INTEGER NOT NULL, removed INTEGER NOT NULL,
                 PRIMARY KEY(table_name,id)
             ) WITHOUT ROWID;
             CREATE TABLE affected_projected (
                 table_name TEXT NOT NULL, id INTEGER NOT NULL,
                 PRIMARY KEY(table_name,id)
             ) WITHOUT ROWID;
             CREATE TABLE target_context (
                 table_name TEXT NOT NULL, id INTEGER NOT NULL, pack_ordinal INTEGER,
                 PRIMARY KEY(table_name,id)
             ) WITHOUT ROWID;
             CREATE INDEX target_context_pack_order
               ON target_context(pack_ordinal,table_name,id);",
    )?;
    Ok(PhysicalDeltaComparison {
        connection,
        scratch,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TableFingerprint {
    table: &'static str,
    rows: u64,
    digest: String,
}

fn fingerprints(db: &Connection) -> Result<Vec<TableFingerprint>, rusqlite::Error> {
    let queries = [
        (
            "row_versions",
            "SELECT * FROM row_versions ORDER BY side,table_name,id",
        ),
        (
            "edges",
            "SELECT * FROM edges ORDER BY from_table,from_id,side,to_table,to_id",
        ),
        (
            "changed_raw",
            "SELECT * FROM changed_raw ORDER BY table_name,id",
        ),
        (
            "affected_projected",
            "SELECT * FROM affected_projected ORDER BY table_name,id",
        ),
        (
            "target_context",
            "SELECT * FROM target_context ORDER BY table_name,id",
        ),
    ];
    let mut result = Vec::new();
    for (table, query) in queries {
        let mut statement = db.prepare(query)?;
        let columns = statement.column_count();
        let mut rows = statement.query([])?;
        let mut hash = Sha256::new();
        hash.update(b"teslatlas-hub/physical-comparison-measurement/v1\0");
        hash.update(table.as_bytes());
        hash.update((columns as u64).to_be_bytes());
        let mut count = 0_u64;
        while let Some(row) = rows.next()? {
            for column in 0..columns {
                match row.get_ref(column)? {
                    ValueRef::Null => hash.update([0]),
                    ValueRef::Integer(value) => {
                        hash.update([1]);
                        hash.update(value.to_be_bytes());
                    }
                    ValueRef::Real(value) => {
                        hash.update([2]);
                        hash.update(value.to_bits().to_be_bytes());
                    }
                    ValueRef::Text(value) => {
                        hash.update([3]);
                        hash.update((value.len() as u64).to_be_bytes());
                        hash.update(value);
                    }
                    ValueRef::Blob(value) => {
                        hash.update([4]);
                        hash.update((value.len() as u64).to_be_bytes());
                        hash.update(value);
                    }
                }
            }
            count = count.checked_add(1).expect("bounded fingerprint count");
        }
        hash.update(count.to_be_bytes());
        result.push(TableFingerprint {
            table,
            rows: count,
            digest: hex::encode(hash.finalize()),
        });
    }
    Ok(result)
}

fn fixture_rows(target: bool) -> Vec<PhysicalTypedRow> {
    let row = |table, id, byte, edges, closed_drive, date| PhysicalTypedRow {
        table,
        id,
        digest: [byte; 32],
        edges,
        closed_drive,
        update_start_pg_us: if table == "updates" { date } else { None },
        month_date_pg_us: date,
    };
    let mut rows = vec![
        row("global_settings", 1, 1, vec![], None, None),
        row("car_settings", 1, 2, vec![], None, None),
        row("cars", 1, 3, vec![("car_settings", 1)], None, None),
        row("addresses", 1, 4, vec![], None, None),
        row("geofences", 1, 5, vec![], None, None),
        row(
            "drives",
            10,
            if target { 21 } else { 6 },
            vec![("positions", 100), ("addresses", 1)],
            Some(!target),
            Some(1_000_000),
        ),
        row(
            "positions",
            100,
            7,
            vec![("drives", 10)],
            None,
            Some(1_000_000),
        ),
        row(
            "charging_processes",
            20,
            if target { 22 } else { 8 },
            vec![("positions", 100), ("geofences", 1)],
            Some(true),
            Some(1_000_000),
        ),
        row(
            "charges",
            200,
            9,
            vec![("charging_processes", 20)],
            None,
            Some(1_000_000),
        ),
        row(
            "updates",
            300,
            if target { 23 } else { 10 },
            vec![],
            None,
            Some(1_000_000),
        ),
    ];
    if target {
        rows.push(row(
            "positions",
            102,
            24,
            vec![("drives", 10)],
            None,
            Some(2_000_000),
        ));
    } else {
        rows.push(row(
            "positions",
            101,
            11,
            vec![("drives", 10)],
            None,
            Some(2_000_000),
        ));
    }
    rows
}

#[test]
fn prepared_physical_inserter_matches_all_scratch_tables_and_conflicts() {
    let temp = crate::private_tempdir().unwrap();
    let mut reference = None;
    for prepared in [false, true] {
        let mut comparison = scratch(
            &temp
                .path()
                .join(if prepared { "prepared" } else { "reference" }),
            8 * 1024 * 1024,
            0,
        )
        .unwrap();
        for side in 0..2 {
            let tx = comparison.connection.transaction().unwrap();
            let mut cached = if prepared {
                Some(PreparedInserter::new(&tx).unwrap())
            } else {
                None
            };
            for row in fixture_rows(side == 1) {
                if let Some(cached) = cached.as_mut() {
                    cached.insert(side, 0, row).unwrap();
                } else {
                    insert_row(&tx, side, 0, row).unwrap();
                }
            }
            let root = fixture_rows(side == 1).remove(2);
            if let Some(cached) = cached.as_mut() {
                cached.insert(side, 1, root.clone()).unwrap();
            } else {
                insert_row(&tx, side, 1, root.clone()).unwrap();
            }
            let mut changed_digest = root;
            changed_digest.digest[0] ^= 1;
            let mut changed_closed = fixture_rows(side == 1).remove(5);
            changed_closed.closed_drive = changed_closed.closed_drive.map(|value| !value);
            for conflict in [changed_digest, changed_closed] {
                let result = if let Some(cached) = cached.as_mut() {
                    cached.insert(side, 2, conflict)
                } else {
                    insert_row(&tx, side, 2, conflict)
                };
                assert!(
                    matches!(result,Err(ProjectionPackError::Invalid(message)) if message=="repeated physical row differs across packs")
                );
            }
            drop(cached);
            tx.commit().unwrap();
        }
        derive_changes(&comparison.connection, 1).unwrap();
        comparison.prepare_wire_scope().unwrap();
        let tables = fingerprints(&comparison.connection).unwrap();
        assert!(
            tables.iter().all(|table| table.rows > 0),
            "fixture exercises all five tables"
        );
        let mut roots = Vec::new();
        comparison
            .visit_roots(|root| {
                roots.push(root);
                Ok(())
            })
            .unwrap();
        assert!(roots.len() >= 2);
        let months = comparison.affected_months().unwrap();
        assert!(roots.iter().any(|root| root.root_type == "drive"
            && root.id == 10
            && root.base_state == "closed"
            && root.target_state == "open"
            && root.target_child_count == 2));
        let mut changed = Vec::new();
        comparison
            .visit_changed_raw(|table, id, removed| {
                changed.push((table.to_owned(), id, removed));
                Ok(())
            })
            .unwrap();
        assert!(changed.contains(&("positions".to_owned(), 101, true)));
        assert!(changed.contains(&("positions".to_owned(), 102, false)));
        let first_ordinal: i64 = comparison
            .connection
            .query_row(
                "SELECT pack_ordinal FROM row_versions WHERE side=1 AND table_name='cars' AND id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            first_ordinal, 0,
            "repeated roots preserve first target ordinal"
        );
        let result = (tables, roots, months, full_fingerprint(&comparison));
        if let Some(reference) = &reference {
            assert_eq!(&result, reference);
        } else {
            reference = Some(result);
        }
    }
}

#[test]
fn prepared_physical_inserter_preserves_sqlite_failure_and_cleanup() {
    let temp = crate::private_tempdir().unwrap();
    for prepared in [false, true] {
        let mut comparison = scratch(
            &temp
                .path()
                .join(if prepared { "prepared" } else { "reference" }),
            8 * 1024 * 1024,
            0,
        )
        .unwrap();
        comparison.connection.execute_batch("CREATE TRIGGER reject_edge BEFORE INSERT ON edges BEGIN SELECT RAISE(ABORT,'fixture edge failure'); END;").unwrap();
        let tx = comparison.connection.transaction().unwrap();
        let row = fixture_rows(false).remove(2);
        let result = if prepared {
            PreparedInserter::new(&tx).unwrap().insert(0, 0, row)
        } else {
            insert_row(&tx, 0, 0, row)
        };
        assert!(matches!(
            result,
            Err(ProjectionPackError::IntegrityCheck(_))
        ));
        tx.rollback().unwrap();
        // Production discards this disposable journal-OFF scratch on error;
        // it does not depend on rollback restoring previously dirty pages.
        let path = comparison.scratch_path().to_owned();
        drop(comparison);
        assert!(!path.exists());
    }
}

fn checked<T, E>(result: Result<T, E>, message: &'static str) -> T {
    result.unwrap_or_else(|_| panic!("{message}"))
}

fn private_read(path: &Path, maximum: u64) -> zeroize::Zeroizing<Vec<u8>> {
    use rustix::fs::{Mode, OFlags, openat};
    assert!(path.is_absolute(), "absolute private input required");
    let parent = checked(
        crate::runtime::development_event_log::validated_directory(
            path.parent().expect("input parent"),
        ),
        "private input parent required",
    );
    let fd = checked(
        openat(
            &parent,
            path.file_name().expect("input leaf"),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ),
        "private input open failed",
    );
    let file = File::from(fd);
    let before = checked(file.metadata(), "input metadata");
    assert!(
        before.is_file()
            && before.uid() == rustix::process::getuid().as_raw()
            && before.mode() & 0o777 == 0o600
            && before.nlink() == 1
            && before.len() <= maximum,
        "private input mode/owner/size invalid"
    );
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    checked(
        (&file).take(maximum + 1).read_to_end(&mut bytes),
        "private input read failed",
    );
    let after = checked(file.metadata(), "input metadata after read");
    assert!(
        bytes.len() as u64 == before.len()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.mode() == after.mode()
            && after.nlink() == 1,
        "private input changed"
    );
    bytes
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PopulationDescriptor {
    manifest: SyncManifest,
    binding: ProjectionBinding,
    ordinals: Vec<u32>,
}

#[test]
#[ignore = "explicit copied verified physical pack population comparison"]
fn measure_verified_physical_pack_statement_reuse() {
    use rustix::fs::{Mode, OFlags, openat};
    use serde_json::json;
    let path =
        |name| PathBuf::from(std::env::var_os(name).expect("benchmark private path required"));
    let descriptor_path = path("TESLATLAS_HUB_BENCH_COMPARE_DESCRIPTOR_FILE");
    let key_path = path("TESLATLAS_HUB_BENCH_CURSOR_KEY_FILE");
    let packs = path("TESLATLAS_HUB_BENCH_PACKS_DIRECTORY");
    let output = path("TESLATLAS_HUB_BENCH_OUTPUT_DIRECTORY");
    assert!(
        packs.is_absolute()
            && output.is_absolute()
            && packs.starts_with(&output)
            && packs != output,
        "copied packs must be below private benchmark output"
    );
    let output_fd = checked(
        crate::runtime::development_event_log::validated_directory(&output),
        "existing private output required",
    );
    let _packs_fd = checked(
        crate::runtime::development_event_log::validated_directory(&packs),
        "private copied packs required",
    );
    let content_fd = checked(
        crate::runtime::development_event_log::validated_directory(&packs.join("sha256")),
        "private copied content directory required",
    );
    let descriptor_bytes = private_read(&descriptor_path, 4 * 1024 * 1024);
    let input_digest = hex::encode(Sha256::digest(&descriptor_bytes[..]));
    let descriptor: PopulationDescriptor = checked(
        serde_json::from_slice(&descriptor_bytes),
        "invalid private descriptor",
    );
    let key_bytes = private_read(&key_path, 32);
    assert!(
        key_bytes.len() == 32,
        "cursor key must contain exactly32 raw bytes"
    );
    let key = CursorKey::from_bytes(checked(
        <[u8; 32]>::try_from(key_bytes.as_slice()),
        "cursor key length",
    ));
    drop(key_bytes);
    let limits = ProtocolLimits::hub_sync_v1_1_3_schema_2_2();
    checked(
        descriptor.manifest.validate_with_limits(limits),
        "manifest limits invalid",
    );
    checked(
        descriptor.manifest.validate_terminal_cursor(&key),
        "terminal cursor invalid",
    );
    let manifest = &descriptor.manifest;
    let binding = &descriptor.binding;
    assert!(
        manifest.schema == crate::protocol::HUB_PROJECTION_SCHEMA_V3
            && manifest.mode == crate::protocol::TransferMode::FullSnapshot
            && manifest.installation_id == binding.installation_id
            && manifest.account_id == binding.account_id
            && manifest.vehicle_id == binding.vehicle_id
            && manifest.generation == binding.generation
            && binding.selected_car_id > 0,
        "manifest binding invalid"
    );
    let ordinals: BTreeSet<_> = descriptor.ordinals.iter().copied().collect();
    assert!(
        !ordinals.is_empty() && ordinals.len() == descriptor.ordinals.len() && ordinals.len() <= 32,
        "1 through32 unique ordinals required"
    );
    let selected: Vec<_> = manifest
        .chunks
        .iter()
        .filter(|pack| ordinals.contains(&pack.ordinal))
        .collect();
    assert!(selected.len() == ordinals.len(), "selected ordinal missing");
    let sum = |field: fn(&crate::protocol::TransportPack) -> u64| {
        selected
            .iter()
            .try_fold(0_u64, |n, pack| n.checked_add(field(pack)))
            .expect("selected totals overflow")
    };
    let rows = sum(|p| p.row_count);
    let decoded = sum(|p| p.uncompressed_bytes);
    let compressed = sum(|p| p.compressed_bytes);
    assert!(
        rows > 0 && rows <= 300_000 && decoded <= MAX_SELECTED_DECODED_BYTES,
        "bounded selected rows/decoded limit exceeded"
    );
    let staging = packs.join(".staging");
    ensure_private_staging_directory(&staging)
        .unwrap_or_else(|_| panic!("private scratch required"));
    let mut file_witnesses = Vec::new();
    for pack in &selected {
        let name = format!("{}.sqlite.zst", pack.sha256);
        let file = File::from(checked(
            openat(
                &content_fd,
                name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            ),
            "copied pack open failed",
        ));
        let metadata = checked(file.metadata(), "copied pack metadata");
        assert!(
            metadata.is_file()
                && metadata.uid() == rustix::process::getuid().as_raw()
                && metadata.mode() & 0o777 == 0o400
                && metadata.nlink() == 1
                && metadata.len() == pack.compressed_bytes,
            "copied input must be single-link owner0400 and exact length"
        );
        file_witnesses.push((
            name,
            (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            ),
        ));
    }
    let mut runs = Vec::new();
    let mut medians = [Vec::new(), Vec::new()];
    let mut expected = None;
    let repeats = match std::env::var("TESLATLAS_HUB_BENCH_COMPARE_REPEATS") {
        Err(std::env::VarError::NotPresent) => 6_usize,
        Ok(value) if value == "4" => 4,
        Ok(value) if value == "6" => 6,
        _ => panic!("comparison repeats must be4 or6"),
    };
    // The separate warmup pair is excluded. Even measured rounds give both
    // modes exactly the same number of first and second positions.
    for round in 0..=repeats {
        for turn in 0..2 {
            let prepared = (round + turn) % 2 == 1;
            let warmup = round == 0;
            eprintln!(
                "{}",
                json!({"event":"physical_population_started","prepared":prepared,"warmup":warmup,"round":round,"order":turn,"selected_packs":selected.len(),"selected_rows":rows})
            );
            let mut comparison = checked(
                scratch(&staging, SCRATCH_BYTES, MINIMUM_FREE_BYTES),
                "scratch admission failed",
            );
            let started = Instant::now();
            let mut visited = 0_u64;
            for side in 0..2 {
                for pack in &selected {
                    let path = packs
                        .join("sha256")
                        .join(format!("{}.sqlite.zst", pack.sha256));
                    let tx = checked(
                        comparison.connection.transaction(),
                        "population transaction",
                    );
                    let mut cached = if prepared {
                        Some(checked(PreparedInserter::new(&tx), "statement preparation"))
                    } else {
                        None
                    };
                    checked(
                        scan_verified_physical_pack_2_2(pack, manifest, binding, &path, |row| {
                            if let Some(cached) = cached.as_mut() {
                                cached.insert(side, i64::from(pack.ordinal), row)?;
                            } else {
                                insert_row(&tx, side, i64::from(pack.ordinal), row)?;
                            }
                            visited = visited
                                .checked_add(1)
                                .ok_or(ProjectionPackError::TooManyRows)?;
                            Ok(())
                        }),
                        "verified population failed",
                    );
                    drop(cached);
                    checked(tx.commit(), "population commit failed");
                    assert!(
                        checked(available_bytes(&staging), "free space") >= MINIMUM_FREE_BYTES,
                        "minimum free reserve exhausted"
                    );
                }
            }
            let population_ms = started.elapsed().as_secs_f64() * 1000.0;
            assert!(
                visited == rows.checked_mul(2).expect("bounded side count"),
                "scan row totals differ"
            );
            let started = Instant::now();
            checked(
                derive_changes(&comparison.connection, binding.selected_car_id),
                "derive failed",
            );
            checked(comparison.prepare_wire_scope(), "wire scope failed");
            let derive_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = Instant::now();
            let tables = checked(fingerprints(&comparison.connection), "fingerprint failed");
            let (_, page_limit, bytes) =
                checked(comparison.scratch_plan_and_limit(), "scratch limit witness");
            assert!(
                page_limit == (SCRATCH_BYTES / 4096) as i64 && bytes <= SCRATCH_BYTES,
                "scratch page cap differs"
            );
            assert!(
                tables[2..].iter().all(|table| table.rows == 0),
                "same-subset sides must be unchanged"
            );
            if let Some(expected) = &expected {
                assert!(&tables == expected, "exact scratch contents differ");
            } else {
                expected = Some(tables.clone());
            }
            for (name, witness) in &file_witnesses {
                let file = File::from(checked(
                    openat(
                        &content_fd,
                        name.as_str(),
                        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                        Mode::empty(),
                    ),
                    "copied pack recheck",
                ));
                let m = checked(file.metadata(), "copied metadata recheck");
                assert!(
                    (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec()) == *witness
                        && m.nlink() == 1
                        && m.mode() & 0o777 == 0o400,
                    "copied input changed"
                );
            }
            let parity_ms = started.elapsed().as_secs_f64() * 1000.0;
            if !warmup {
                medians[usize::from(prepared)].push(population_ms);
            }
            let run = json!({"event":"physical_population_complete","prepared":prepared,"warmup":warmup,"round":round,"order":turn,"population_ms":population_ms,"derive_ms":derive_ms,"parity_ms":parity_ms,"visited_rows":visited,"scratch_bytes":bytes,"tables":tables});
            eprintln!("{run}");
            runs.push(run);
            let scratch_path = comparison.scratch_path().to_owned();
            drop(comparison);
            assert!(!scratch_path.exists(), "owned scratch cleanup failed");
        }
    }
    let medians = medians.map(|mut values| {
        assert_eq!(values.len(), repeats);
        values.sort_by(f64::total_cmp);
        let middle = values.len() / 2;
        (values[middle - 1] + values[middle]) / 2.0
    });
    let receipt = json!({"event":"verified_physical_pack_statement_reuse","scope":"same verified subset populated on both sides; component only, not changed-publication acceptance",
        "debug_assertions":cfg!(debug_assertions),"sqlite_version":rusqlite::version(),"descriptor_digest":input_digest,"selected_packs":selected.len(),"selected_rows":rows,
        "selected_decoded_bytes":decoded,"selected_compressed_bytes":compressed,"scratch_cap_bytes":SCRATCH_BYTES,"sqlite_cache_kib":32768,"minimum_free_bytes":MINIMUM_FREE_BYTES,
        "all_pack_validations":true,"exact_five_table_parity":true,"os_cache_purged":false,"measured_repeats_per_mode":repeats,"warmup_repeats_per_mode":1,"measured_first_per_mode":repeats/2,"measured_second_per_mode":repeats/2,"order":"alternating reference/prepared; separate warmup pair","median":"average of two middle measured values","reference_median_ms":medians[0],"prepared_median_ms":medians[1],"ratio":medians[0]/medians[1],"runs":runs,"catalogue_published":false});
    let fd = checked(
        openat(
            &output_fd,
            "statement-reuse-receipt.json",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ),
        "new receipt required",
    );
    let file = File::from(fd);
    checked(
        serde_json::to_writer_pretty(&file, &receipt),
        "receipt write failed",
    );
    checked(file.sync_all(), "receipt sync failed");
    eprintln!("{receipt}");
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FullComparisonDescriptor {
    old: SyncManifest,
    new: SyncManifest,
    binding: ProjectionBinding,
}

fn validate_full_descriptor(descriptor: &FullComparisonDescriptor, key: &CursorKey) {
    let limits = ProtocolLimits::hub_sync_v1_1_3_schema_2_2();
    for manifest in [&descriptor.old, &descriptor.new] {
        checked(
            manifest.validate_with_limits(limits),
            "full manifest limits",
        );
        checked(
            manifest.validate_terminal_cursor(key),
            "full cursor invalid",
        );
        assert!(
            manifest.schema == crate::protocol::HUB_PROJECTION_SCHEMA_V3
                && manifest.mode == crate::protocol::TransferMode::FullSnapshot
                && manifest.installation_id == descriptor.binding.installation_id
                && manifest.account_id == descriptor.binding.account_id
                && manifest.vehicle_id == descriptor.binding.vehicle_id
                && manifest.generation == descriptor.binding.generation
                && !manifest.chunks.is_empty()
                && manifest.chunks.len() <= 1771,
            "full manifest binding/count invalid"
        );
        let totals = manifest
            .chunks
            .iter()
            .try_fold((0_u64, 0_u64), |(rows, bytes), pack| {
                Some((
                    rows.checked_add(pack.row_count)?,
                    bytes.checked_add(pack.uncompressed_bytes)?,
                ))
            })
            .expect("full totals overflow");
        assert!(
            totals.0 <= 20_000_000 && totals.1 <= 32 * 1024 * 1024 * 1024,
            "full per-side rows/decoded bound exceeded"
        );
    }
    assert!(
        descriptor.binding.selected_car_id > 0
            && descriptor.new.head_sequence > descriptor.old.head_sequence,
        "full sequence/source car invalid"
    );
}

// The unchanged original walk, including validation/admission and per-pack
// transactions, is test-only. The candidate calls production compare directly.
fn reference_full_comparison(
    descriptor: &FullComparisonDescriptor,
    key: &CursorKey,
    packs: &Path,
    maximum: u64,
) -> PhysicalDeltaComparison {
    validate_full_descriptor(descriptor, key);
    let mut comparison = checked(
        scratch(&packs.join(".staging"), maximum, MINIMUM_FREE_BYTES),
        "full reference scratch",
    );
    for (side, manifest) in [(0_i64, &descriptor.old), (1_i64, &descriptor.new)] {
        for pack in &manifest.chunks {
            let path = packs
                .join("sha256")
                .join(format!("{}.sqlite.zst", pack.sha256));
            let tx = checked(
                comparison.connection.transaction(),
                "full reference transaction",
            );
            checked(
                scan_verified_physical_pack_2_2(
                    pack,
                    manifest,
                    &descriptor.binding,
                    &path,
                    |row| insert_row(&tx, side, i64::from(pack.ordinal), row),
                ),
                "full reference verified scan",
            );
            checked(tx.commit(), "full reference commit");
            assert!(
                checked(available_bytes(&packs.join(".staging")), "full free space")
                    >= MINIMUM_FREE_BYTES,
                "full free reserve exhausted"
            );
        }
    }
    checked(
        derive_changes(&comparison.connection, descriptor.binding.selected_car_id),
        "full reference derive",
    );
    comparison
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct FullFingerprint {
    tables: Vec<TableFingerprint>,
    roots: (u64, String),
    affected: (u64, String),
    context: (u64, String),
    months: (u64, String),
}

fn full_fingerprint(comparison: &PhysicalDeltaComparison) -> FullFingerprint {
    let mut roots = Sha256::new();
    let mut roots_count = 0_u64;
    checked(
        comparison.visit_roots(|row| {
            let encoded = serde_json::to_vec(&(
                row.root_type,
                row.id,
                row.base_state,
                row.target_state,
                row.target_child_count,
            ))
            .expect("root fingerprint encoding");
            roots.update((encoded.len() as u64).to_be_bytes());
            roots.update(encoded);
            roots_count += 1;
            Ok(())
        }),
        "full root witness",
    );
    let mut affected = Sha256::new();
    let mut affected_count = 0_u64;
    checked(
        comparison.visit_affected_projected(|table, id, removed| {
            let encoded =
                serde_json::to_vec(&(table, id, removed)).expect("affected fingerprint encoding");
            affected.update((encoded.len() as u64).to_be_bytes());
            affected.update(encoded);
            affected_count += 1;
            Ok(())
        }),
        "full affected witness",
    );
    let mut context = Sha256::new();
    let mut context_count = 0_u64;
    checked(
        comparison.visit_target_context(|table, id, ordinal| {
            let encoded =
                serde_json::to_vec(&(table, id, ordinal)).expect("context fingerprint encoding");
            context.update((encoded.len() as u64).to_be_bytes());
            context.update(encoded);
            context_count += 1;
            Ok(())
        }),
        "full context witness",
    );
    let months = checked(comparison.affected_months(), "full month witness");
    FullFingerprint {
        tables: checked(
            fingerprints(&comparison.connection),
            "full table fingerprints",
        ),
        roots: (roots_count, hex::encode(roots.finalize())),
        affected: (affected_count, hex::encode(affected.finalize())),
        context: (context_count, hex::encode(context.finalize())),
        months: (
            months.len() as u64,
            hex::encode(Sha256::digest(
                serde_json::to_vec(&months).expect("month fingerprint encoding"),
            )),
        ),
    }
}

#[test]
#[ignore = "explicit full retained-head comparison; no source capture/publication"]
fn measure_full_verified_physical_comparison_statement_reuse() {
    use rustix::fs::{Mode, OFlags, openat};
    use serde_json::json;
    let path = |name| PathBuf::from(std::env::var_os(name).expect("full private path required"));
    let descriptor_bytes = private_read(
        &path("TESLATLAS_HUB_BENCH_COMPARE_FULL_DESCRIPTOR_FILE"),
        8 * 1024 * 1024,
    );
    let descriptor_digest = hex::encode(Sha256::digest(&descriptor_bytes[..]));
    let descriptor: FullComparisonDescriptor = checked(
        serde_json::from_slice(&descriptor_bytes),
        "full descriptor invalid",
    );
    let key_bytes = private_read(&path("TESLATLAS_HUB_BENCH_CURSOR_KEY_FILE"), 32);
    let key = CursorKey::from_bytes(checked(
        <[u8; 32]>::try_from(key_bytes.as_slice()),
        "full cursor key length",
    ));
    drop(key_bytes);
    validate_full_descriptor(&descriptor, &key);
    let packs = path("TESLATLAS_HUB_BENCH_PACKS_DIRECTORY");
    let output = path("TESLATLAS_HUB_BENCH_OUTPUT_DIRECTORY");
    assert!(
        packs.is_absolute()
            && output.is_absolute()
            && packs.starts_with(&output)
            && packs != output,
        "full copied packs must be below output"
    );
    let output_fd = checked(
        crate::runtime::development_event_log::validated_directory(&output),
        "full private output",
    );
    let _packs_fd = checked(
        crate::runtime::development_event_log::validated_directory(&packs),
        "full private packs",
    );
    let content_fd = checked(
        crate::runtime::development_event_log::validated_directory(&packs.join("sha256")),
        "full private content",
    );
    let mut witnesses = std::collections::BTreeMap::new();
    for manifest in [&descriptor.old, &descriptor.new] {
        for pack in &manifest.chunks {
            let name = format!("{}.sqlite.zst", pack.sha256);
            let file = File::from(checked(
                openat(
                    &content_fd,
                    name.as_str(),
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                    Mode::empty(),
                ),
                "full copied input open",
            ));
            let m = checked(file.metadata(), "full copied input metadata");
            assert!(
                m.is_file()
                    && m.uid() == rustix::process::getuid().as_raw()
                    && m.mode() & 0o777 == 0o400
                    && m.nlink() == 1
                    && m.len() == pack.compressed_bytes,
                "full copied input mode/length invalid"
            );
            witnesses.insert(name, (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec()));
        }
    }
    let repeats = match std::env::var("TESLATLAS_HUB_BENCH_COMPARE_FULL_REPEATS") {
        Err(std::env::VarError::NotPresent) => 2_usize,
        Ok(value) if value == "1" => 1,
        Ok(value) if value == "2" => 2,
        _ => panic!("full repeats must be1 or2"),
    };
    let warmups = match std::env::var("TESLATLAS_HUB_BENCH_COMPARE_FULL_WARMUP_PAIRS") {
        Err(std::env::VarError::NotPresent) => 0_usize,
        Ok(value) if value == "0" => 0,
        Ok(value) if value == "1" => 1,
        _ => panic!("full warmup pairs must be0 or1"),
    };
    const FULL_SCRATCH_BYTES: u64 = 8 * 1024 * 1024 * 1024;
    let mut expected = None;
    let mut runs = Vec::new();
    let mut measured = [Vec::new(), Vec::new()];
    for pair in 0..warmups + repeats {
        let warmup = pair < warmups;
        let round = pair.saturating_sub(warmups);
        for order in 0..2 {
            let prepared = (round + order) % 2 == 1;
            eprintln!(
                "{}",
                json!({"event":"full_comparison_started","prepared":prepared,"warmup":warmup,"round":round,"order":order})
            );
            let started = Instant::now();
            let comparison = if prepared {
                checked(
                    PhysicalDeltaComparison::compare(
                        &descriptor.old,
                        &descriptor.new,
                        &descriptor.binding,
                        &key,
                        &packs,
                        FULL_SCRATCH_BYTES,
                        MINIMUM_FREE_BYTES,
                    ),
                    "full production compare",
                )
            } else {
                reference_full_comparison(&descriptor, &key, &packs, FULL_SCRATCH_BYTES)
            };
            let comparison_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = Instant::now();
            checked(comparison.prepare_wire_scope(), "full dependency closure");
            let closure_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = Instant::now();
            let fingerprint = full_fingerprint(&comparison);
            if let Some(expected) = &expected {
                assert_eq!(&fingerprint, expected, "full comparison/scope differs");
            } else {
                expected = Some(fingerprint);
            }
            let (_, pages, bytes) = checked(
                comparison.scratch_plan_and_limit(),
                "full scratch cap witness",
            );
            assert!(
                pages == (FULL_SCRATCH_BYTES / 4096) as i64 && bytes <= FULL_SCRATCH_BYTES,
                "full scratch cap differs"
            );
            for (name, witness) in &witnesses {
                let file = File::from(checked(
                    openat(
                        &content_fd,
                        name.as_str(),
                        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                        Mode::empty(),
                    ),
                    "full copied input recheck",
                ));
                let m = checked(file.metadata(), "full copied metadata recheck");
                assert!(
                    (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec()) == *witness
                        && m.nlink() == 1
                        && m.mode() & 0o777 == 0o400,
                    "full copied input changed"
                );
            }
            let parity_ms = started.elapsed().as_secs_f64() * 1000.0;
            if !warmup {
                measured[usize::from(prepared)].push(comparison_ms);
            }
            let run = json!({"event":"full_comparison_complete","prepared":prepared,"warmup":warmup,"round":round,"order":order,"comparison_ms":comparison_ms,"closure_ms":closure_ms,"parity_ms":parity_ms,"scratch_bytes":bytes});
            eprintln!("{run}");
            runs.push(run);
            let owned = comparison.scratch_path().to_owned();
            drop(comparison);
            assert!(!owned.exists(), "full owned scratch cleanup failed");
        }
    }
    let medians = measured.map(|mut values| {
        values.sort_by(f64::total_cmp);
        if repeats == 1 {
            values[0]
        } else {
            (values[0] + values[1]) / 2.0
        }
    });
    let receipt = json!({"event":"full_verified_comparison_statement_reuse","scope":"full verified retained heads, comparison and closure; no pack materialization/source capture/catalogue publication",
        "descriptor_digest":descriptor_digest,"debug_assertions":cfg!(debug_assertions),"sqlite_version":rusqlite::version(),"scratch_cap_bytes":FULL_SCRATCH_BYTES,"sqlite_cache_kib":32768,"minimum_free_bytes":MINIMUM_FREE_BYTES,
        "repeats_per_mode":repeats,"warmup_pairs":warmups,"balanced_order":repeats==2,"exploratory":repeats==1,"os_cache_purged":false,"cache_conditions":"fresh decode and scratch each case; existing OS cache retained",
        "comparison_includes":"manifest/cursor checks, scratch admission/schema, verified old/new scans, insertion/commits, derive_changes","closure_separately_timed":true,"all_five_tables_and_scope_exact":true,
        "reference_median_ms":medians[0],"prepared_median_ms":medians[1],"ratio":medians[0]/medians[1],"fingerprint":expected,"runs":runs});
    let fd = checked(
        openat(
            &output_fd,
            "full-statement-reuse-receipt.json",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ),
        "new full receipt required",
    );
    let file = File::from(fd);
    checked(
        serde_json::to_writer_pretty(&file, &receipt),
        "full receipt write",
    );
    checked(file.sync_all(), "full receipt sync");
    eprintln!("{receipt}");
}
