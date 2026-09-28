// SPDX-License-Identifier: AGPL-3.0-only

//! Wire-neutral PhysicalV3 comparison. The caller still publishes full packs;
//! this scratch index never changes the catalogue or source snapshot.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::protocol::{CursorKey, ProtocolLimits, SyncManifest};
use crate::sync::hub_pack::{
    PhysicalTypedRow, ProjectionBinding, ProjectionPackError, available_bytes,
    ensure_private_staging_directory, scan_verified_physical_pack_2_2,
};

#[derive(Debug, Error)]
pub(crate) enum PhysicalCompareError {
    #[error("physical comparison pack validation failed: {0}")]
    Pack(#[from] ProjectionPackError),
    #[error("physical comparison manifest validation failed: {0}")]
    Protocol(#[from] crate::protocol::ProtocolError),
    #[error("physical comparison SQLite failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("physical comparison scratch file failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("physical comparison scratch capacity is insufficient")]
    Capacity,
    #[error("physical comparison binding or sequence is invalid")]
    Binding,
}

/// Holds only exact row digests, pack ordinals, and dependency edges on disk.
/// Drop closes and removes the private scratch database, including on error.
#[allow(dead_code)] // Foundation; publication/HTTP wiring follows Protocol 1.4.
pub(crate) struct PhysicalDeltaComparison {
    connection: Connection,
    scratch: NamedTempFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalRootWitness {
    pub root_type: &'static str,
    pub id: i64,
    pub base_state: &'static str,
    pub target_state: &'static str,
    pub target_child_count: i64,
}

impl PhysicalDeltaComparison {
    pub(crate) fn prepare_wire_scope(&self) -> Result<(), PhysicalCompareError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS impacted_roots (
                root_type TEXT NOT NULL,id INTEGER NOT NULL,
                PRIMARY KEY(root_type,id)
             ) WITHOUT ROWID;
             INSERT OR IGNORE INTO impacted_roots
               SELECT 'drive',id FROM affected_projected WHERE table_name='drives';
             INSERT OR IGNORE INTO impacted_roots
               SELECT 'charge',id FROM affected_projected WHERE table_name='charges';
             INSERT OR IGNORE INTO affected_projected(table_name,id)
               SELECT 'positions',e.to_id FROM changed_raw c JOIN edges e
                 ON e.from_table='drives' AND e.from_id=c.id AND e.to_table='positions'
               WHERE c.table_name='drives';
             INSERT OR IGNORE INTO affected_projected(table_name,id)
               SELECT 'charge_samples',e.from_id FROM impacted_roots r JOIN edges e
                 ON r.root_type='charge' AND e.side=1 AND e.from_table='charges'
                    AND e.to_table='charging_processes' AND e.to_id=r.id;
             INSERT OR IGNORE INTO target_context(table_name,id)
               SELECT 'positions',a.id FROM affected_projected a JOIN row_versions v
                 ON v.side=1 AND v.table_name='positions' AND v.id=a.id
               WHERE a.table_name='positions';
             INSERT OR IGNORE INTO target_context(table_name,id)
               SELECT 'drives',r.id FROM impacted_roots r JOIN row_versions v
                 ON v.side=1 AND v.table_name='drives' AND v.id=r.id
               WHERE r.root_type='drive';
             INSERT OR IGNORE INTO target_context(table_name,id)
               SELECT 'charging_processes',r.id FROM impacted_roots r JOIN row_versions v
                 ON v.side=1 AND v.table_name='charging_processes' AND v.id=r.id
               WHERE r.root_type='charge';
             INSERT OR IGNORE INTO target_context(table_name,id)
               SELECT 'positions',e.from_id FROM impacted_roots r JOIN edges e
                 ON r.root_type='drive' AND e.side=1 AND e.from_table='positions'
                    AND e.to_table='drives' AND e.to_id=r.id;
             INSERT OR IGNORE INTO target_context(table_name,id)
               SELECT 'charges',e.from_id FROM impacted_roots r JOIN edges e
                 ON r.root_type='charge' AND e.side=1 AND e.from_table='charges'
                    AND e.to_table='charging_processes' AND e.to_id=r.id;",
        )?;
        // Follow target soft-parent edges to a fixed point. Every insertion is
        // into the page-limited scratch database; no unbounded CTE sorter.
        loop {
            let inserted = self.connection.execute(
                "INSERT OR IGNORE INTO target_context(table_name,id)
                 SELECT e.to_table,e.to_id FROM target_context c JOIN edges e
                   ON e.side=1 AND e.from_table=c.table_name AND e.from_id=c.id
                 JOIN row_versions v ON v.side=1 AND v.table_name=e.to_table AND v.id=e.to_id",
                [],
            )?;
            if inserted == 0 {
                break;
            }
        }
        self.connection.execute_batch(
            "UPDATE target_context SET pack_ordinal=(
                SELECT v.pack_ordinal FROM row_versions v
                WHERE v.side=1 AND v.table_name=target_context.table_name AND v.id=target_context.id);",
        )?;
        Ok(())
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(crate) fn visit_roots(
        &self,
        mut visit: impl FnMut(PhysicalRootWitness) -> Result<(), PhysicalCompareError>,
    ) -> Result<(), PhysicalCompareError> {
        let mut statement = self.connection.prepare(
            "SELECT r.root_type,r.id,
                COALESCE((SELECT CASE WHEN v.closed_drive=1 THEN 'closed' ELSE 'open' END
                  FROM row_versions v WHERE v.side=0 AND v.table_name=CASE r.root_type
                    WHEN 'drive' THEN 'drives' ELSE 'charging_processes' END AND v.id=r.id),'absent'),
                COALESCE((SELECT CASE WHEN v.closed_drive=1 THEN 'closed' ELSE 'open' END
                  FROM row_versions v WHERE v.side=1 AND v.table_name=CASE r.root_type
                    WHEN 'drive' THEN 'drives' ELSE 'charging_processes' END AND v.id=r.id),'absent'),
                (SELECT COUNT(*) FROM edges e WHERE e.side=1
                    AND e.from_table=CASE r.root_type WHEN 'drive' THEN 'positions' ELSE 'charges' END
                    AND e.to_table=CASE r.root_type WHEN 'drive' THEN 'drives' ELSE 'charging_processes' END
                    AND e.to_id=r.id)
             FROM impacted_roots r ORDER BY r.root_type,r.id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let kind: String = row.get(0)?;
            let base: String = row.get(2)?;
            let target: String = row.get(3)?;
            let root_type = if kind == "drive" { "drive" } else { "charge" };
            let state = |value: &str| match value {
                "closed" => "closed",
                "open" => "open",
                _ => "absent",
            };
            visit(PhysicalRootWitness {
                root_type,
                id: row.get(1)?,
                base_state: state(&base),
                target_state: state(&target),
                target_child_count: row.get(4)?,
            })?;
        }
        Ok(())
    }

    pub(crate) fn affected_months(&self) -> Result<Vec<String>, PhysicalCompareError> {
        let mut statement = self.connection.prepare(
            "SELECT v.month_date_pg_us FROM affected_projected a
             JOIN row_versions v ON a.id=v.id AND a.table_name=v.table_name
             WHERE v.table_name IN ('drives','positions') AND v.month_date_pg_us IS NOT NULL",
        )?;
        let mut months = BTreeSet::new();
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let pg_us: i64 = row.get(0)?;
            let unix_ns = (i128::from(pg_us) + 946_684_800_000_000) * 1_000;
            let datetime = time::OffsetDateTime::from_unix_timestamp_nanos(unix_ns)
                .map_err(|_| PhysicalCompareError::Binding)?;
            months.insert(format!(
                "{:04}-{:02}",
                datetime.year(),
                u8::from(datetime.month())
            ));
            if months.len() > 120 {
                return Err(PhysicalCompareError::Capacity);
            }
        }
        Ok(months.into_iter().collect())
    }
    /// `max_scratch_bytes` is an enforced SQLite page ceiling, not an estimate.
    /// Free-space admission also reserves one maximum decoded pack and the
    /// caller's existing minimum-free floor before any old/new pack scan.
    pub(crate) fn compare(
        old: &SyncManifest,
        new: &SyncManifest,
        binding: &ProjectionBinding,
        cursor_key: &CursorKey,
        packs_dir: &Path,
        max_scratch_bytes: u64,
        minimum_free_bytes: u64,
    ) -> Result<Self, PhysicalCompareError> {
        let limits = ProtocolLimits::hub_sync_v1_1_3_schema_2_2();
        for manifest in [old, new] {
            manifest.validate_with_limits(limits)?;
            manifest.validate_terminal_cursor(cursor_key)?;
            if manifest.schema != crate::protocol::HUB_PROJECTION_SCHEMA_V3
                || manifest.mode != crate::protocol::TransferMode::FullSnapshot
                || manifest.installation_id != binding.installation_id
                || manifest.account_id != binding.account_id
                || manifest.vehicle_id != binding.vehicle_id
                || manifest.generation != binding.generation
                || manifest.chunks.is_empty()
            {
                return Err(PhysicalCompareError::Binding);
            }
        }
        if new.head_sequence <= old.head_sequence || max_scratch_bytes < 4096 {
            return Err(PhysicalCompareError::Binding);
        }
        let staging = packs_dir.join(".staging");
        ensure_private_staging_directory(&staging)?;
        let required = max_scratch_bytes
            .checked_add(limits.max_uncompressed_pack_bytes)
            .and_then(|n| n.checked_add(minimum_free_bytes))
            .ok_or(PhysicalCompareError::Capacity)?;
        if available_bytes(&staging)? < required {
            return Err(PhysicalCompareError::Capacity);
        }
        let scratch = tempfile::Builder::new()
            .prefix("physical-compare-")
            .suffix(".sqlite")
            .tempfile_in(&staging)?;
        let mut connection = Connection::open(scratch.path())?;
        connection.execute_batch(
            "PRAGMA trusted_schema=OFF; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;
             PRAGMA temp_store=MEMORY; PRAGMA automatic_index=OFF;
             PRAGMA cache_size=-32768; PRAGMA page_size=4096;",
        )?;
        let page_limit =
            i64::try_from(max_scratch_bytes / 4096).map_err(|_| PhysicalCompareError::Capacity)?;
        connection.pragma_update(None, "max_page_count", page_limit)?;
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
        for (side, manifest) in [(0_i64, old), (1_i64, new)] {
            for pack in &manifest.chunks {
                let path = packs_dir
                    .join("sha256")
                    .join(format!("{}.sqlite.zst", pack.sha256));
                let transaction = connection.transaction()?;
                scan_verified_physical_pack_2_2(pack, manifest, binding, &path, |row| {
                    insert_row(&transaction, side, i64::from(pack.ordinal), row)
                })?;
                transaction.commit()?;
                if available_bytes(&staging)? < minimum_free_bytes {
                    return Err(PhysicalCompareError::Capacity);
                }
            }
        }
        derive_changes(&connection, binding.selected_car_id)?;
        Ok(Self {
            connection,
            scratch,
        })
    }

    pub(crate) fn scratch_path(&self) -> &Path {
        self.scratch.path()
    }

    #[cfg(test)]
    pub(crate) fn scratch_plan_and_limit(
        &self,
    ) -> Result<(Vec<String>, i64, u64), PhysicalCompareError> {
        let mut statement = self.connection.prepare(
            "EXPLAIN QUERY PLAN SELECT table_name,id,pack_ordinal FROM target_context
             ORDER BY pack_ordinal,table_name,id",
        )?;
        let plan = statement
            .query_map([], |row| row.get::<_, String>(3))?
            .collect::<Result<Vec<_>, _>>()?;
        let page_limit: i64 = self
            .connection
            .query_row("PRAGMA max_page_count", [], |row| row.get(0))?;
        let bytes = std::fs::metadata(self.scratch.path())?.len();
        Ok((plan, page_limit, bytes))
    }

    pub(crate) fn visit_changed_raw(
        &self,
        mut visit: impl FnMut(&str, i64, bool) -> Result<(), PhysicalCompareError>,
    ) -> Result<(), PhysicalCompareError> {
        let mut statement = self
            .connection
            .prepare("SELECT table_name,id,removed FROM changed_raw ORDER BY table_name,id")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let table: String = row.get(0)?;
            visit(&table, row.get(1)?, row.get::<_, i64>(2)? != 0)?;
        }
        Ok(())
    }

    /// The final flag means raw target absence. A surviving open drive is a
    /// recompute intent: the App's public projection policy removes its drive
    /// row and detaches surviving positions. It is not a raw tombstone.
    pub(crate) fn visit_affected_projected(
        &self,
        mut visit: impl FnMut(&str, i64, bool) -> Result<(), PhysicalCompareError>,
    ) -> Result<(), PhysicalCompareError> {
        let mut statement = self.connection.prepare(
            "SELECT a.table_name,a.id,
                    CASE WHEN a.table_name='drives' THEN NOT EXISTS(
                      SELECT 1 FROM row_versions r WHERE r.side=1 AND r.table_name='drives'
                        AND r.id=a.id)
                    ELSE NOT EXISTS(
                      SELECT 1 FROM row_versions r WHERE r.side=1
                        AND r.table_name=CASE a.table_name
                          WHEN 'cars' THEN 'cars' WHEN 'car_settings' THEN 'cars'
                          WHEN 'charges' THEN 'charging_processes'
                          WHEN 'charge_samples' THEN 'charges'
                          WHEN 'car_states' THEN 'states' WHEN 'car_updates' THEN 'updates'
                          ELSE a.table_name END AND r.id=a.id) END
               FROM affected_projected a ORDER BY a.table_name,a.id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let table: String = row.get(0)?;
            visit(&table, row.get(1)?, row.get::<_, i64>(2)? != 0)?;
        }
        Ok(())
    }

    /// Raw target rows required as full context for affected projected roots.
    /// These rows do not themselves authorize a projected write.
    pub(crate) fn visit_target_context(
        &self,
        mut visit: impl FnMut(&str, i64, u32) -> Result<(), PhysicalCompareError>,
    ) -> Result<(), PhysicalCompareError> {
        let mut statement = self.connection.prepare(
            "SELECT table_name,id,pack_ordinal FROM target_context
             ORDER BY pack_ordinal,table_name,id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let table: String = row.get(0)?;
            let ordinal: i64 = row.get(2)?;
            visit(
                &table,
                row.get(1)?,
                u32::try_from(ordinal).map_err(|_| PhysicalCompareError::Binding)?,
            )?;
        }
        Ok(())
    }
}

fn insert_row(
    tx: &rusqlite::Transaction<'_>,
    side: i64,
    ordinal: i64,
    row: PhysicalTypedRow,
) -> Result<(), ProjectionPackError> {
    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO row_versions
         (side,table_name,id,digest,pack_ordinal,closed_drive,update_start_pg_us,month_date_pg_us)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                side,
                row.table,
                row.id,
                row.digest.as_slice(),
                ordinal,
                row.closed_drive.map(i64::from),
                row.update_start_pg_us,
                row.month_date_pg_us,
            ],
        )
        .map_err(ProjectionPackError::IntegrityCheck)?;
    if inserted == 0 {
        let existing: (Vec<u8>, Option<i64>) = tx
            .query_row(
                "SELECT digest,closed_drive FROM row_versions
             WHERE side=?1 AND table_name=?2 AND id=?3",
                params![side, row.table, row.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(ProjectionPackError::IntegrityCheck)?;
        if existing.0 != row.digest || existing.1 != row.closed_drive.map(i64::from) {
            return Err(ProjectionPackError::Invalid(
                "repeated physical row differs across packs".to_owned(),
            ));
        }
        return Ok(());
    }
    for (to_table, to_id) in row.edges {
        tx.execute(
            "INSERT OR IGNORE INTO edges(side,from_table,from_id,to_table,to_id)
             VALUES (?1,?2,?3,?4,?5)",
            params![side, row.table, row.id, to_table, to_id],
        )
        .map_err(ProjectionPackError::IntegrityCheck)?;
    }
    Ok(())
}

fn derive_changes(db: &Connection, car_id: i64) -> Result<(), rusqlite::Error> {
    db.execute_batch(
        "INSERT INTO changed_raw(table_name,id,removed)
         SELECT old.table_name,old.id,CASE WHEN new.id IS NULL THEN 1 ELSE 0 END
         FROM row_versions old LEFT JOIN row_versions new
           ON new.side=1 AND new.table_name=old.table_name AND new.id=old.id
         WHERE old.side=0 AND (new.id IS NULL OR new.digest != old.digest);
         INSERT OR IGNORE INTO changed_raw(table_name,id,removed)
         SELECT new.table_name,new.id,0 FROM row_versions new
         LEFT JOIN row_versions old
           ON old.side=0 AND old.table_name=new.table_name AND old.id=new.id
         WHERE new.side=1 AND old.id IS NULL;

         INSERT OR IGNORE INTO affected_projected(table_name,id)
         SELECT CASE table_name WHEN 'charging_processes' THEN 'charges'
           WHEN 'charges' THEN 'charge_samples' WHEN 'states' THEN 'car_states'
           WHEN 'updates' THEN 'car_updates' ELSE table_name END,id
         FROM changed_raw WHERE table_name IN
           ('drives','positions','charging_processes','charges','states','updates');

         INSERT OR IGNORE INTO affected_projected(table_name,id)
         SELECT CASE e.from_table WHEN 'drives' THEN 'drives' ELSE 'charges' END,e.from_id
         FROM changed_raw c JOIN edges e ON e.to_table=c.table_name AND e.to_id=c.id
         WHERE c.table_name IN ('positions','addresses','geofences')
           AND e.from_table IN ('drives','charging_processes');
         INSERT OR IGNORE INTO affected_projected(table_name,id)
         SELECT 'positions',e.from_id FROM changed_raw c JOIN edges e
           ON e.to_table='drives' AND e.to_id=c.id AND e.from_table='positions'
         WHERE c.table_name='drives';
         INSERT OR IGNORE INTO affected_projected(table_name,id)
         SELECT 'charge_samples',e.from_id FROM changed_raw c JOIN edges e
           ON e.to_table='charging_processes' AND e.to_id=c.id AND e.from_table='charges'
         WHERE c.table_name='charging_processes';
         INSERT OR IGNORE INTO affected_projected(table_name,id)
         SELECT 'charges',e.to_id FROM changed_raw c JOIN edges e
           ON e.from_table='charges' AND e.from_id=c.id AND e.to_table='charging_processes'
         WHERE c.table_name='charges';
         INSERT OR IGNORE INTO affected_projected(table_name,id)
         SELECT 'drives',e.to_id FROM changed_raw c JOIN edges e
           ON e.from_table='positions' AND e.from_id=c.id AND e.to_table='drives'
         WHERE c.table_name='positions';",
    )?;
    let car_dirty: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM changed_raw
         WHERE table_name IN ('cars','car_settings','updates'))",
        [],
        |r| r.get(0),
    )?;
    if car_dirty {
        db.execute(
            "INSERT OR IGNORE INTO affected_projected VALUES ('cars',?1)",
            [car_id],
        )?;
    }
    let settings_dirty: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM changed_raw
         WHERE table_name IN ('cars','car_settings'))",
        [],
        |r| r.get(0),
    )?;
    if settings_dirty {
        db.execute(
            "INSERT OR IGNORE INTO affected_projected VALUES ('car_settings',?1)",
            [car_id],
        )?;
    }
    db.execute_batch(
        "INSERT OR IGNORE INTO target_context(table_name,id)
         SELECT c.table_name,c.id FROM changed_raw c
         JOIN row_versions r ON r.side=1 AND r.table_name=c.table_name AND r.id=c.id;
         INSERT OR IGNORE INTO target_context(table_name,id)
         SELECT CASE a.table_name WHEN 'car_settings' THEN 'cars'
             WHEN 'charges' THEN 'charging_processes'
             WHEN 'charge_samples' THEN 'charges' WHEN 'car_states' THEN 'states'
             WHEN 'car_updates' THEN 'updates' ELSE a.table_name END,a.id
         FROM affected_projected a JOIN row_versions r ON r.side=1
          AND r.table_name=CASE a.table_name WHEN 'car_settings' THEN 'cars'
             WHEN 'charges' THEN 'charging_processes'
             WHEN 'charge_samples' THEN 'charges' WHEN 'car_states' THEN 'states'
             WHEN 'car_updates' THEN 'updates' ELSE a.table_name END AND r.id=a.id;
         INSERT OR IGNORE INTO target_context(table_name,id)
         SELECT 'positions',e.from_id FROM affected_projected a JOIN edges e
           ON e.side=1 AND e.from_table='positions' AND e.to_table='drives' AND e.to_id=a.id
         WHERE a.table_name='drives';
         INSERT OR IGNORE INTO target_context(table_name,id)
         SELECT 'charges',e.from_id FROM affected_projected a JOIN edges e
           ON e.side=1 AND e.from_table='charges'
             AND e.to_table='charging_processes' AND e.to_id=a.id
         WHERE a.table_name='charges';
         CREATE TABLE context_seed (
           table_name TEXT NOT NULL,id INTEGER NOT NULL,
           PRIMARY KEY(table_name,id)
         ) WITHOUT ROWID;
         INSERT INTO context_seed SELECT table_name,id FROM target_context;
         INSERT OR IGNORE INTO target_context(table_name,id)
         SELECT e.to_table,e.to_id FROM context_seed c JOIN edges e
           ON e.side=1 AND e.from_table=c.table_name AND e.from_id=c.id
         JOIN row_versions r ON r.side=1 AND r.table_name=e.to_table AND r.id=e.to_id;
         DROP TABLE context_seed;
         INSERT OR IGNORE INTO target_context(table_name,id)
         SELECT table_name,id FROM row_versions WHERE side=1
           AND table_name IN ('global_settings','cars','car_settings')
           AND EXISTS(SELECT 1 FROM affected_projected);
         INSERT OR IGNORE INTO target_context(table_name,id)
         SELECT 'updates',id FROM row_versions
         WHERE side=1 AND table_name='updates'
           AND EXISTS(SELECT 1 FROM affected_projected WHERE table_name='cars')
         ORDER BY update_start_pg_us DESC,id DESC LIMIT 1;
         UPDATE target_context SET pack_ordinal=(
           SELECT r.pack_ordinal FROM row_versions r
           WHERE r.side=1 AND r.table_name=target_context.table_name
             AND r.id=target_context.id);",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_compare_repeated_root_is_deduplicated_and_conflict_fails_closed() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE row_versions (
               side INTEGER NOT NULL, table_name TEXT NOT NULL,id INTEGER NOT NULL,
               digest BLOB NOT NULL,pack_ordinal INTEGER NOT NULL,closed_drive INTEGER,
               update_start_pg_us INTEGER,month_date_pg_us INTEGER,
               PRIMARY KEY(side,table_name,id)
             ) WITHOUT ROWID;
             CREATE TABLE edges (
               side INTEGER NOT NULL,from_table TEXT NOT NULL,from_id INTEGER NOT NULL,
               to_table TEXT NOT NULL,to_id INTEGER NOT NULL,
               PRIMARY KEY(from_table,from_id,side,to_table,to_id)
             ) WITHOUT ROWID;",
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        let root = PhysicalTypedRow {
            table: "cars",
            id: 10,
            digest: [0x51; 32],
            edges: vec![("car_settings", 500)],
            closed_drive: None,
            update_start_pg_us: None,
            month_date_pg_us: None,
        };
        insert_row(&tx, 0, 0, root.clone()).unwrap();
        insert_row(&tx, 0, 1, root.clone()).unwrap();
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM row_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let edge_count: i64 = tx
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(edge_count, 1);
        let mut conflicting = root;
        conflicting.digest[0] ^= 1;
        assert!(matches!(insert_row(&tx,0,2,conflicting),
            Err(ProjectionPackError::Invalid(message))
            if message == "repeated physical row differs across packs"));
    }
}
