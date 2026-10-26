// SPDX-License-Identifier: AGPL-3.0-only

//! Parity of the test-only original inserter against the production prepared inserter.

use super::*;
use rusqlite::types::ValueRef;
use serde::Serialize;
use sha2::{Digest, Sha256};

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
