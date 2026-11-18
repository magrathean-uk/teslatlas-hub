// SPDX-License-Identifier: AGPL-3.0-only

//! Editing a sealed physical stage before it is published (M03-T21): the
//! import seam's plan for the negotiated 1.4 route. The stage stays sealed;
//! its accounting is worked out again and bounded in the same transaction,
//! so the content digest a publication derives covers exactly the edit.

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};

use super::{
    META_PAYLOAD_BYTES, META_ROW_COUNT, SourceDbStage, SourceDbStageError, SourceDbStageFormat,
    SourceDbStageState, SourceDbStageTable, encode_row, map_write_error, write_meta,
};

/// A child table whose parent and instant the seam reads without decoding
/// the row: positions (drive) and charges (charging process).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StageChildTable {
    Positions,
    Charges,
}

impl StageChildTable {
    const fn scan_sql(self) -> &'static str {
        match self {
            Self::Positions => {
                "SELECT source_id, json_extract(row_json, '$.drive_id'),
                        json_extract(row_json, '$.date_pg_us')
                   FROM stage_rows WHERE table_name = 'positions' ORDER BY source_id"
            }
            Self::Charges => {
                "SELECT source_id, json_extract(row_json, '$.charging_process_id'),
                        json_extract(row_json, '$.date_pg_us')
                   FROM stage_rows WHERE table_name = 'charges' ORDER BY source_id"
            }
        }
    }
}

/// One open edit of a sealed physical stage.
pub(crate) struct SealedStageEdit<'a> {
    transaction: Transaction<'a>,
}

impl SourceDbStage {
    /// Edits this sealed physical stage in one transaction: `edit` reads and
    /// changes rows, then the row count and payload bytes are counted again,
    /// checked against the stage's limits and recorded. Any error rolls the
    /// whole edit back.
    pub(crate) fn edit_sealed<E: From<SourceDbStageError>>(
        &mut self,
        edit: impl FnOnce(&SealedStageEdit<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        if !self.writable {
            return Err(SourceDbStageError::StageReadOnly.into());
        }
        let format = self.format()?;
        if format != SourceDbStageFormat::PhysicalV3 {
            return Err(SourceDbStageError::TableFormatMismatch {
                format: format.as_str(),
                table: "stage_rows",
            }
            .into());
        }
        let stats = self.stats()?;
        if stats.state != SourceDbStageState::Sealed {
            return Err(SourceDbStageError::StageNotSealed.into());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(SourceDbStageError::from)?;
        let editor = SealedStageEdit { transaction };
        edit(&editor)?;
        let transaction = editor.transaction;
        let (rows, bytes): (i64, i64) = transaction
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(encoded_bytes), 0) FROM stage_rows",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(SourceDbStageError::from)?;
        let rows = u64::try_from(rows).map_err(|_| SourceDbStageError::InvalidStoredAccounting)?;
        let bytes = u64::try_from(bytes).map_err(|_| SourceDbStageError::InvalidStoredAccounting)?;
        if rows > stats.limits.max_rows {
            return Err(SourceDbStageError::RowLimitExceeded {
                maximum: stats.limits.max_rows,
            }
            .into());
        }
        if bytes > stats.limits.max_stage_bytes {
            return Err(SourceDbStageError::PayloadByteLimitExceeded {
                maximum: stats.limits.max_stage_bytes,
            }
            .into());
        }
        write_meta(&transaction, META_ROW_COUNT, rows)?;
        write_meta(&transaction, META_PAYLOAD_BYTES, bytes)?;
        transaction
            .commit()
            .map_err(|error| map_write_error(error, stats.limits.max_stage_bytes))?;
        #[cfg(test)]
        crate::hub_pack::record_verification("stage-write", self.path.display(), true);
        Ok(())
    }
}

impl SealedStageEdit<'_> {
    /// Every row of one table, decoded, in id order. For the small tables
    /// only: drives, charging processes, addresses and geofences.
    pub(crate) fn rows<T: DeserializeOwned>(
        &self,
        table: SourceDbStageTable,
    ) -> Result<Vec<(i64, T)>, SourceDbStageError> {
        let mut statement = self.transaction.prepare(
            "SELECT source_id, row_json FROM stage_rows WHERE table_name = ?1 ORDER BY source_id",
        )?;
        let rows = statement
            .query_map([table.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (source_id, json) = row?;
                let value = serde_json::from_str(&json).map_err(|source| {
                    SourceDbStageError::StoredRowDecode {
                        table: table.as_str(),
                        source_id,
                        source,
                    }
                })?;
                Ok((source_id, value))
            })
            .collect::<Result<Vec<_>, SourceDbStageError>>()?;
        Ok(rows)
    }

    /// Each child row's id, parent and instant (PostgreSQL microseconds), in
    /// id order, read inside SQLite without decoding the row. `visit` must not
    /// change the stage; it collects what to change.
    pub(crate) fn for_each_child(
        &self,
        table: StageChildTable,
        visit: &mut dyn FnMut(i64, Option<i64>, i64),
    ) -> Result<(), SourceDbStageError> {
        let mut statement = self.transaction.prepare(table.scan_sql())?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            visit(row.get(0)?, row.get(1)?, row.get(2)?);
        }
        Ok(())
    }

    /// One row, decoded.
    pub(crate) fn get<T: DeserializeOwned>(
        &self,
        table: SourceDbStageTable,
        source_id: i64,
    ) -> Result<Option<T>, SourceDbStageError> {
        let json: Option<String> = self
            .transaction
            .query_row(
                "SELECT row_json FROM stage_rows WHERE table_name = ?1 AND source_id = ?2",
                params![table.as_str(), source_id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| {
            serde_json::from_str(&json).map_err(|source| SourceDbStageError::StoredRowDecode {
                table: table.as_str(),
                source_id,
                source,
            })
        })
        .transpose()
    }

    /// Writes one row, replacing any row with the same id.
    pub(crate) fn put<T: Serialize>(
        &self,
        table: SourceDbStageTable,
        source_id: i64,
        value: &T,
    ) -> Result<(), SourceDbStageError> {
        if source_id <= 0 {
            return Err(SourceDbStageError::InvalidSourceId);
        }
        let json = encode_row(value)?;
        let bytes = i64::try_from(json.len()).map_err(|_| SourceDbStageError::InvalidStoredAccounting)?;
        self.transaction.execute(
            "INSERT INTO stage_rows(table_name, source_id, row_json, encoded_bytes)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(table_name, source_id) DO UPDATE
                SET row_json = excluded.row_json, encoded_bytes = excluded.encoded_bytes",
            params![table.as_str(), source_id, json, bytes],
        )?;
        Ok(())
    }

    /// Removes one row, if present.
    pub(crate) fn delete(
        &self,
        table: SourceDbStageTable,
        source_id: i64,
    ) -> Result<(), SourceDbStageError> {
        self.transaction.execute(
            "DELETE FROM stage_rows WHERE table_name = ?1 AND source_id = ?2",
            params![table.as_str(), source_id],
        )?;
        Ok(())
    }

    /// The position whose instant is nearest `at_pg_us` (the earlier on a
    /// tie, then the lower id), if the stage holds any.
    pub(crate) fn nearest_position(&self, at_pg_us: i64) -> Result<Option<i64>, SourceDbStageError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT source_id FROM stage_rows WHERE table_name = 'positions'
                  ORDER BY abs(json_extract(row_json, '$.date_pg_us') - ?1),
                           json_extract(row_json, '$.date_pg_us'), source_id
                  LIMIT 1",
                params![at_pg_us],
                |row| row.get(0),
            )
            .optional()?)
    }
}
