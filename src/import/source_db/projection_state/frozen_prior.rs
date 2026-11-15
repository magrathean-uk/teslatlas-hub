// SPDX-License-Identifier: AGPL-3.0-only

/// A frozen copy of a projection state's current digests, read as a prior
/// lookup (M03-T24): a window rebind's base state is the prior of the seam
/// published on that base in the same catalogue transaction, before the
/// store holds it. The copy is a private temporary file, removed on drop.
pub(crate) struct FrozenProjectionStatePrior {
    connection: Connection,
    _file: tempfile::TempPath,
}

const FROZEN_PRIOR_ALIAS: &str = "frozen_prior_source";

impl SourceDbProjectionState {
    /// Freezes the state's current digests as they are now; later changes
    /// to the state do not reach the copy.
    pub(crate) fn frozen_prior(&self) -> Result<FrozenProjectionStatePrior, SourceDbProjectionStateError> {
        let file = tempfile::Builder::new()
            .prefix("projection-prior-")
            .suffix(".sqlite")
            .tempfile()
            .map_err(|source| SourceDbProjectionStateError::CreateFile {
                path: std::env::temp_dir(),
                source,
            })?
            .into_temp_path();
        let connection = Connection::open(&file).map_err(SourceDbProjectionStateError::Sqlite)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=OFF;
                 PRAGMA synchronous=OFF;
                 CREATE TABLE prior_rows (
                     entity_ordinal INTEGER NOT NULL,
                     entity_id INTEGER NOT NULL,
                     car_id INTEGER NOT NULL,
                     projection_sha256 BLOB NOT NULL,
                     PRIMARY KEY(entity_ordinal, entity_id)
                 ) STRICT, WITHOUT ROWID;",
            )
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        let mut uri = Url::from_file_path(&self.path)
            .map_err(|_| SourceDbProjectionStateError::InvalidTransferPath(self.path.clone()))?;
        uri.query_pairs_mut().append_pair("mode", "ro");
        connection
            .execute(&format!("ATTACH DATABASE ?1 AS {FROZEN_PRIOR_ALIAS}"), params![uri.as_str()])
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        let copied = connection.execute_batch(&format!(
            "INSERT INTO prior_rows(entity_ordinal, entity_id, car_id, projection_sha256)
             SELECT entity_ordinal, entity_id, car_id, projection_sha256
               FROM {FROZEN_PRIOR_ALIAS}.current_rows ORDER BY entity_ordinal, entity_id;"
        ));
        let detached = connection.execute_batch(&format!("DETACH DATABASE {FROZEN_PRIOR_ALIAS}"));
        copied.and(detached).map_err(SourceDbProjectionStateError::Sqlite)?;
        Ok(FrozenProjectionStatePrior { connection, _file: file })
    }
}

impl FrozenProjectionStatePrior {
    fn row(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<(i64, i64, i64, Vec<u8>)> {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    }
}

impl PriorProjectionStateLookup for FrozenProjectionStatePrior {
    fn digest(
        &mut self,
        entity: SourceDbProjectionStateEntity,
        id: i64,
    ) -> Result<Option<Sha256Digest>, Box<dyn Error + Send + Sync>> {
        let digest: Option<Vec<u8>> = self
            .connection
            .query_row(
                "SELECT projection_sha256 FROM prior_rows WHERE entity_ordinal = ?1 AND entity_id = ?2",
                params![i64::from(entity.ordinal()), id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(digest.map(digest_from_blob).transpose()?)
    }

    fn page_after(
        &mut self,
        after: Option<SourceDbProjectionStateCursor>,
        limit: u32,
    ) -> Result<SourceDbProjectionStateDigestPage, Box<dyn Error + Send + Sync>> {
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(Box::new(SourceDbProjectionStateError::InvalidPageSize));
        }
        let (after_entity, after_id) =
            after.map_or((-1_i64, 0_i64), |cursor| (i64::from(cursor.entity.ordinal()), cursor.id));
        let mut statement = self.connection.prepare(
            "SELECT entity_ordinal, entity_id, car_id, projection_sha256 FROM prior_rows
              WHERE (entity_ordinal, entity_id) > (?1, ?2)
              ORDER BY entity_ordinal, entity_id LIMIT ?3",
        )?;
        let raw = statement
            .query_map(params![after_entity, after_id, i64::from(limit) + 1], Self::row)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut rows = Vec::with_capacity(raw.len());
        for (ordinal, id, car_id, digest) in raw {
            rows.push(SourceDbProjectionStateDigestRow {
                entity: SourceDbProjectionStateEntity::from_ordinal(ordinal)?,
                id,
                car_id,
                digest: digest_from_blob(digest)?,
            });
        }
        let next_after = if rows.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
            rows.pop();
            rows.last().map(|row| SourceDbProjectionStateCursor { entity: row.entity, id: row.id })
        } else {
            None
        };
        Ok(SourceDbProjectionStateDigestPage { rows, next_after })
    }
}
