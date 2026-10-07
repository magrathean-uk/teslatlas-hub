// SPDX-License-Identifier: AGPL-3.0-only

//! Opt-in, positions-only scratch experiment. No production format is changed.

use super::*;
use serde_json::json;
use std::{fs::File, time::Instant};

const ROWS: u64 = 50_000;
const PAGE: u32 = 1_000;
const ROW_BYTES: usize = 1024 * 1024;
const PAGE_BYTES: usize = 16 * 1024 * 1024;
const PAYLOAD_BYTES: u64 = 128 * 1024 * 1024;
const DISK_BYTES: u64 = 256 * 1024 * 1024;
const FREE_BYTES: u64 = 30 * 1024 * 1024 * 1024;

fn checked<T, E>(value: Result<T, E>) -> T {
    value.unwrap_or_else(|_| panic!("positions experiment failed (details withheld)"))
}

fn limits() -> TeslaMateStageLimits {
    TeslaMateStageLimits {
        max_rows: ROWS,
        max_stage_bytes: DISK_BYTES,
        minimum_free_bytes: FREE_BYTES,
    }
}

// Scoped cleanup also runs when a parity assertion or write fails.
struct Scratch {
    stage: Option<TeslaMateStage>,
    compressed: Option<Connection>,
    file: Option<tempfile::NamedTempFile>,
    directory: tempfile::TempDir,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(stage) = self.stage.take() {
            let _ = stage.discard();
        }
        drop(self.compressed.take());
        drop(self.file.take());
        // TempDir removes only this case's newly created directory/sidecars.
    }
}

impl Scratch {
    fn new(output: &Path, compressed: bool) -> Self {
        assert!(checked(available_bytes(output)) >= FREE_BYTES + 2 * DISK_BYTES);
        let directory = checked(
            tempfile::Builder::new()
                .prefix("positions-case-")
                .tempdir_in(output),
        );
        let mut case = Self {
            stage: None,
            compressed: None,
            file: None,
            directory,
        };
        if !compressed {
            case.stage = Some(checked(TeslaMateStage::create_physical_v3(
                case.directory.path().join("imports"),
                limits(),
            )));
        } else {
            let file = checked(
                tempfile::Builder::new()
                    .suffix(".sqlite")
                    .tempfile_in(case.directory.path()),
            );
            let db = checked(Connection::open(file.path()));
            checked(configure_writable_connection(&db, limits()));
            // Positions-only counterpart: compressed BLOB replaces TEXT. JSON
            // validity is checked after bounded decompression, never skipped.
            checked(db.execute_batch("CREATE TABLE stage_meta(key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
                CREATE TABLE stage_rows(table_name TEXT NOT NULL CHECK(table_name='positions'),
                source_id INTEGER NOT NULL CHECK(source_id>0), row_zstd BLOB NOT NULL CHECK(length(row_zstd)<=1052672),
                encoded_bytes INTEGER NOT NULL CHECK(encoded_bytes>=0 AND encoded_bytes<=1048576),
                PRIMARY KEY(table_name,source_id)) STRICT, WITHOUT ROWID;"));
            let tx = checked(db.unchecked_transaction());
            for (key, value) in [
                (META_STATE, "open".to_owned()),
                (META_ROW_COUNT, "0".to_owned()),
                (META_PAYLOAD_BYTES, "0".to_owned()),
                (META_FORMAT, "test-positions-row-zstd-level3".to_owned()),
                (META_MAX_ROWS, ROWS.to_string()),
                (META_MAX_STAGE_BYTES, DISK_BYTES.to_string()),
                (META_MINIMUM_FREE_BYTES, FREE_BYTES.to_string()),
            ] {
                checked(write_meta(&tx, key, value));
            }
            checked(tx.commit());
            case.compressed = Some(db);
            case.file = Some(file);
        }
        case
    }

    fn db(&self) -> &Connection {
        self.stage
            .as_ref()
            .map(|v| &v.connection)
            .unwrap_or_else(|| self.compressed.as_ref().expect("scratch connection"))
    }

    fn bytes(&self) -> u64 {
        let pages: i64 = checked(self.db().query_row("PRAGMA page_count", [], |r| r.get(0)));
        let pages = checked(u64::try_from(pages));
        assert!(pages * 4096 <= DISK_BYTES);
        let path = self
            .stage
            .as_ref()
            .map(|v| v.path.as_path())
            .unwrap_or_else(|| self.file.as_ref().expect("scratch file").path());
        let actual = checked(fs::metadata(path)).len();
        assert!(
            actual <= DISK_BYTES && actual == pages * 4096,
            "actual main SQLite file cap required"
        );
        pages * 4096
    }
}

fn input_page(db: &Connection, after: i64) -> Vec<(i64, String)> {
    let mut query = checked(db.prepare(
        "SELECT source_id,row_json FROM stage_rows
        WHERE table_name='positions' AND source_id>?1 ORDER BY source_id LIMIT ?2",
    ));
    let rows = checked(query.query_map(params![after, PAGE], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    }));
    let mut page = Vec::new();
    let mut bytes = 0;
    for row in rows {
        let row = checked(row);
        assert!(row.0 > after && row.1.len() <= ROW_BYTES);
        bytes += row.1.len();
        assert!(bytes <= PAGE_BYTES);
        page.push(row);
    }
    page
}

#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    rows: u64,
    payload: u64,
    digest: String,
}

// Both validation and readback walk bounded keyset pages. Hashing length and ID
// as well as the unchanged bytes detects omissions, ordering and boundaries.
fn fingerprint(db: &Connection, compressed: bool) -> Fingerprint {
    fingerprint_profile(db, compressed).0
}

fn fingerprint_profile(db: &Connection, compressed: bool) -> (Fingerprint, [f64; 4]) {
    // Detailed read/decompress/JSON/hash timings include per-row clock overhead
    // equally in both modes; the enclosing wall time is reported separately.
    let mut phases = [0.0; 4];
    let mut digest = Sha256::new();
    digest.update(b"positions-scratch-identical-json/v1\0");
    let mut after = 0_i64;
    let mut count = 0_u64;
    let mut payload = 0_u64;
    loop {
        let sql = if compressed {
            "SELECT source_id,row_zstd,encoded_bytes FROM stage_rows WHERE table_name='positions' AND source_id>?1 ORDER BY source_id LIMIT ?2"
        } else {
            "SELECT source_id,CAST(row_json AS BLOB),encoded_bytes FROM stage_rows WHERE table_name='positions' AND source_id>?1 ORDER BY source_id LIMIT ?2"
        };
        let mut statement = checked(db.prepare(sql));
        let mut rows = checked(statement.query(params![after, PAGE]));
        let mut decoder = checked(zstd::bulk::Decompressor::new());
        let mut page_bytes = 0;
        let before = after;
        loop {
            let started = Instant::now();
            let Some(row) = checked(rows.next()) else {
                break;
            };
            let id: i64 = checked(row.get(0));
            let blob: Vec<u8> = checked(row.get(1));
            let length: i64 = checked(row.get(2));
            let length = checked(u64::try_from(length));
            phases[0] += started.elapsed().as_secs_f64() * 1000.0;
            assert!(id > after && length <= ROW_BYTES as u64 && blob.len() <= ROW_BYTES + 4096);
            let started = Instant::now();
            let decoded = if compressed {
                checked(decoder.decompress(&blob, length as usize))
            } else {
                blob
            };
            phases[1] += started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(decoded.len() as u64, length, "decoded length mismatch");
            let started = Instant::now();
            let value: Value = checked(serde_json::from_slice(&decoded));
            assert!(value.is_object(), "position must be JSON object");
            phases[2] += started.elapsed().as_secs_f64() * 1000.0;
            page_bytes += decoded.len();
            assert!(page_bytes <= PAGE_BYTES);
            count += 1;
            payload = checked(payload.checked_add(length).ok_or(()));
            assert!(count <= ROWS && payload <= PAYLOAD_BYTES);
            let started = Instant::now();
            digest.update(id.to_be_bytes());
            digest.update(length.to_be_bytes());
            digest.update(&decoded);
            phases[3] += started.elapsed().as_secs_f64() * 1000.0;
            after = id;
        }
        if before == after {
            break;
        }
    }
    (
        Fingerprint {
            rows: count,
            payload,
            digest: hex::encode(digest.finalize()),
        },
        phases,
    )
}

fn exact_rows(input: &Connection, scratch: &Connection, compressed: bool) {
    let mut after = 0;
    loop {
        let reference = input_page(input, after);
        let sql = if compressed {
            "SELECT source_id,row_zstd,encoded_bytes FROM stage_rows WHERE table_name='positions' AND source_id>?1 ORDER BY source_id LIMIT ?2"
        } else {
            "SELECT source_id,CAST(row_json AS BLOB),encoded_bytes FROM stage_rows WHERE table_name='positions' AND source_id>?1 ORDER BY source_id LIMIT ?2"
        };
        let mut statement = checked(scratch.prepare(sql));
        let mut rows = checked(statement.query(params![after, PAGE]));
        let mut decoder = checked(zstd::bulk::Decompressor::new());
        for (id, json) in &reference {
            let row = checked(rows.next()).expect("matching page row required");
            let actual_id: i64 = checked(row.get(0));
            let blob: Vec<u8> = checked(row.get(1));
            let length: i64 = checked(row.get(2));
            let length = checked(u64::try_from(length));
            assert!(
                actual_id == *id && length == json.len() as u64,
                "exact ordered ID and length required"
            );
            let bytes = if compressed {
                checked(decoder.decompress(&blob, json.len()))
            } else {
                blob
            };
            assert!(
                bytes == json.as_bytes(),
                "every row byte must match original"
            );
        }
        assert!(
            checked(rows.next()).is_none(),
            "exact page boundary required"
        );
        let Some(last) = reference.last() else {
            break;
        };
        after = last.0;
    }
}

fn integrity(db: &Connection) {
    let mut query = checked(db.prepare("PRAGMA integrity_check"));
    let values = checked(
        checked(query.query_map([], |r| r.get::<_, String>(0))).collect::<Result<Vec<_>, _>>(),
    );
    assert!(
        values.len() == 1 && values[0] == "ok",
        "full integrity required"
    );
}

fn accounting(db: &Connection, compressed: bool, expected: &Fingerprint) -> Fingerprint {
    let (count, payload): (i64, i64) = checked(db.query_row(
        "SELECT COUNT(*),COALESCE(SUM(encoded_bytes),0) FROM stage_rows",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ));
    let count = checked(u64::try_from(count));
    let payload = checked(u64::try_from(payload));
    assert!(count == expected.rows && payload == expected.payload);
    assert_eq!(checked(parse_meta_u64(db, META_ROW_COUNT)), count);
    assert_eq!(checked(parse_meta_u64(db, META_PAYLOAD_BYTES)), payload);
    assert_eq!(checked(read_meta(db, META_STATE)), "sealed");
    fingerprint(db, compressed)
}

fn case(input: &TeslaMateStage, output: &Path, compressed: bool, expected: &Fingerprint) -> Value {
    let mut scratch = Scratch::new(output, compressed);
    let started = Instant::now();
    let mut after = 0;
    let mut count = 0_u64;
    let mut payload = 0_u64;
    loop {
        let page = input_page(&input.connection, after);
        if page.is_empty() {
            break;
        }
        after = page.last().expect("nonempty page").0;
        count += page.len() as u64;
        payload += page.iter().map(|v| v.1.len() as u64).sum::<u64>();
        assert!(count <= ROWS && payload <= PAYLOAD_BYTES);
        assert!(checked(available_bytes(output)) >= FREE_BYTES + DISK_BYTES);
        if let Some(stage) = scratch.stage.as_mut() {
            checked(stage.insert_encoded_json_page(TeslaMateStageTable::Positions, page));
        } else {
            // Reuse one compression context for all rows in this bounded page.
            let mut compressor = checked(zstd::bulk::Compressor::new(3));
            let db = scratch.compressed.as_mut().expect("compressed scratch");
            let tx = checked(db.transaction_with_behavior(TransactionBehavior::Immediate));
            let mut insert =
                checked(tx.prepare("INSERT INTO stage_rows VALUES('positions',?1,?2,?3)"));
            for (id, json) in page {
                let bytes = checked(compressor.compress(json.as_bytes()));
                checked(insert.execute(params![id, bytes, json.len() as i64]));
            }
            drop(insert);
            checked(write_meta(&tx, META_ROW_COUNT, count));
            checked(write_meta(&tx, META_PAYLOAD_BYTES, payload));
            checked(tx.commit());
        }
    }
    let insertion_ms = started.elapsed().as_secs_f64() * 1000.0;
    // Separating seal metadata from validation lets each full integrity and
    // decoded accounting phase be timed once. This exists only in this test.
    let started = Instant::now();
    let tx = checked(scratch.db().unchecked_transaction());
    checked(write_meta(&tx, META_STATE, "sealed"));
    checked(tx.commit());
    let seal_commit_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    integrity(scratch.db());
    let integrity_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let actual = accounting(scratch.db(), compressed, expected);
    let decoded_accounting_digest_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(&actual, expected, "full decoded row parity");
    let started = Instant::now();
    let (readback, read_phases) = fingerprint_profile(scratch.db(), compressed);
    let ordered_page_readback_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(&readback, expected, "ordered page readback parity");
    let started = Instant::now();
    exact_rows(&input.connection, scratch.db(), compressed);
    let outside_timing_exact_byte_parity_ms = started.elapsed().as_secs_f64() * 1000.0;
    let result = json!({"mode":if compressed {"row_zstd_level3"} else {"production_json"},
        "insertion_ms":insertion_ms,"seal_commit_ms":seal_commit_ms,"integrity_ms":integrity_ms,
        "decoded_accounting_digest_ms":decoded_accounting_digest_ms,"ordered_page_readback_ms":ordered_page_readback_ms,
        "ordered_read_fetch_ms":read_phases[0],"ordered_read_decompress_ms":read_phases[1],
        "ordered_read_json_validate_ms":read_phases[2],"ordered_read_hash_ms":read_phases[3],
        "outside_timing_exact_byte_parity_ms":outside_timing_exact_byte_parity_ms,
        "sqlite_allocated_bytes":scratch.bytes(),"rows":actual.rows,"logical_payload_bytes":actual.payload,
        "logical_digest":actual.digest,"exact_parity":true});
    let directory = scratch.directory.path().to_owned();
    drop(scratch);
    assert!(!directory.exists(), "owned scratch cleanup required");
    result
}

#[test]
#[ignore = "explicit bounded immutable positions compression measurement"]
fn measure_positions_row_compression() {
    let input = PathBuf::from(
        std::env::var_os("TESLATLAS_HUB_BENCH_POSITIONS_STAGE")
            .expect("private prepared subset required"),
    );
    let output = PathBuf::from(
        std::env::var_os("TESLATLAS_HUB_BENCH_POSITIONS_OUTPUT").expect("private output required"),
    );
    let output_fd = checked(crate::runtime::development_event_log::validated_directory(
        &output,
    ));
    assert!(
        checked(fs::read_dir(&output)).next().is_none(),
        "fresh empty output required"
    );
    let repeats: u32 = std::env::var("TESLATLAS_HUB_BENCH_POSITIONS_REPEATS")
        .unwrap_or_else(|_| "4".into())
        .parse()
        .expect("bounded repeats");
    assert!(matches!(repeats, 4 | 6), "balanced four or six repeats");
    let input = checked(TeslaMateStage::open_sealed(input));
    let stats = checked(input.stats());
    assert!(
        input.format == TeslaMateStageFormat::PhysicalV3
            && stats.row_count == ROWS
            && stats.payload_bytes <= PAYLOAD_BYTES
    );
    let positions: i64 = checked(input.connection.query_row(
        "SELECT COUNT(*) FROM stage_rows WHERE table_name='positions'",
        [],
        |r| r.get(0),
    ));
    let positions = checked(u64::try_from(positions));
    assert_eq!(positions, ROWS, "positions-only subset required");
    let oversized:i64=checked(input.connection.query_row("SELECT COUNT(*) FROM stage_rows WHERE encoded_bytes>?1 OR length(CAST(row_json AS BLOB))!=encoded_bytes",[ROW_BYTES as i64],|r|r.get(0)));
    let oversized = checked(u64::try_from(oversized));
    assert_eq!(
        oversized, 0,
        "bounded original byte lengths required before decoding"
    );
    let expected = fingerprint(&input.connection, false);
    assert!(expected.rows == stats.row_count && expected.payload == stats.payload_bytes);
    let identity = checked(fstat(&input.file_descriptor));
    let mut warmups = Vec::new();
    for compressed in [false, true] {
        warmups.push(case(&input, &output, compressed, &expected));
    }
    let mut runs = Vec::new();
    for round in 0..repeats {
        for compressed in if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let mut result = case(&input, &output, compressed, &expected);
            result["round"] = json!(round);
            result["first_in_pair"] = json!(compressed == (round % 2 != 0));
            runs.push(result);
        }
    }
    checked(input.verify_path_identity());
    let final_identity = checked(fstat(&input.file_descriptor));
    assert!(
        identity.st_dev == final_identity.st_dev
            && identity.st_ino == final_identity.st_ino
            && identity.st_size == final_identity.st_size
            && identity.st_mtime == final_identity.st_mtime
            && identity.st_mtime_nsec == final_identity.st_mtime_nsec,
        "input must remain stable"
    );
    assert_eq!(
        fingerprint(&input.connection, false),
        expected,
        "source unchanged"
    );
    let mut medians = json!({});
    for mode in ["production_json", "row_zstd_level3"] {
        for metric in [
            "insertion_ms",
            "seal_commit_ms",
            "integrity_ms",
            "decoded_accounting_digest_ms",
            "ordered_page_readback_ms",
            "ordered_read_fetch_ms",
            "ordered_read_decompress_ms",
            "ordered_read_json_validate_ms",
            "ordered_read_hash_ms",
            "sqlite_allocated_bytes",
        ] {
            let mut values: Vec<f64> = runs
                .iter()
                .filter(|v| v["mode"] == mode)
                .map(|v| v[metric].as_f64().expect("numeric measurement"))
                .collect();
            values.sort_by(f64::total_cmp);
            medians[mode][metric] =
                json!((values[values.len() / 2 - 1] + values[values.len() / 2]) / 2.0);
        }
    }
    let receipt = json!({"event":"positions_row_compression_experiment","debug_assertions":cfg!(debug_assertions),
        "sqlite_version":rusqlite::version(),"rows":expected.rows,"logical_payload_bytes":expected.payload,
        "logical_digest":expected.digest,"page_rows":PAGE,"page_bytes_cap":PAGE_BYTES,"row_bytes_cap":ROW_BYTES,
        "scratch_disk_cap_bytes":DISK_BYTES,"minimum_free_bytes":FREE_BYTES,"repeats_per_mode":repeats,
        "warmups":warmups,"runs":runs,"medians":medians,"os_cache_purged":false,"dictionary":"none",
        "durable_page_transactions":"DELETE/FULL","scratch_cleanup_verified":true,
        "scope":"positions-only storage component; compressed schema checks JSON after bounded decoding; no production API/format or whole import change"});
    let fd = checked(openat(
        &output_fd,
        "positions-compression-receipt.json",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    ));
    let file = File::from(fd);
    checked(serde_json::to_writer_pretty(&file, &receipt));
    checked(file.sync_all());
    eprintln!(
        "positions compression receipt written; rows={ROWS}; cases={}",
        runs.len() + warmups.len()
    );
}
