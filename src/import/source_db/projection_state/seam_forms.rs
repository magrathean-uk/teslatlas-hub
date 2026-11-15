// SPDX-License-Identifier: AGPL-3.0-only

// The import seam's published forms in the successor state (M03-T19).
//
// The durable state of a successor holds, for positions, charge samples and
// charges, the digest of the form an import published, not of the raw
// source row: a row the seam gives another parent or other bounds is held in
// that form, and a row the seam leaves out holds a marker digest. A later
// import whose plan changes compares against that, so a row whose published
// form changed is restated or tombstoned whether or not its source row
// changed, and an unchanged replay finds every digest equal.

/// The table that keeps candidate payloads while the source is read. It is
/// dropped when the forms are applied, before the spool is transferred.
const SEAM_RETAINED_TABLE: &str = "seam_retained";

/// Held seam rows taken out of a state: entity ordinal, id, car id and
/// canonical payload.
#[derive(Debug)]
pub(crate) struct SeamRetainedRows(Vec<(i64, i64, i64, String)>);

/// What the import seam makes of one retained row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SeamForm {
    /// Published as recorded.
    Recorded,
    /// Left out of the packs.
    LeftOut,
    /// Published in this form: its canonical payload and digest.
    Published {
        payload: Vec<u8>,
        digest: Sha256Digest,
    },
}

/// What applying the published forms did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SeamFormsApplied {
    /// Changed source rows the seam leaves out, as the parity report counts
    /// them: positions, charges and charge samples.
    pub(crate) positions_left_out: u64,
    pub(crate) charges_left_out: u64,
    pub(crate) charge_samples_left_out: u64,
    /// Rows an earlier import published that the seam now leaves out
    /// (entity, id, car): clients receive their tombstones.
    pub(crate) withdrawn: Vec<(SourceDbProjectionStateEntity, i64, i64)>,
}

/// The bytes of a left-out marker that say the row was left out.
const SEAM_LEFT_OUT_PREFIX_BYTES: usize = 16;

/// The digest a left-out row holds in the durable state: a prefix that says
/// the row was left out, then a part bound to the source row's own digest.
/// A later import tells an unchanged left-out row (the same marker) from one
/// whose source changed (another marker with the same prefix), so a changed
/// source row the seam still leaves out is counted as seam-skipped, and a row
/// clients never held is never withdrawn from them. The prefix is the start
/// of the earlier fixed marker, so a state that holds one still reads as left
/// out.
pub(crate) fn seam_left_out_digest(
    entity: SourceDbProjectionStateEntity,
    source: &Sha256Digest,
) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update([0]);
    hasher.update(b"import seam: left out source");
    hasher.update([0]);
    hasher.update(entity.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(source.as_bytes());
    let bound: [u8; 32] = hasher.finalize().into();
    let mut marker = seam_left_out_prefix(entity);
    marker[SEAM_LEFT_OUT_PREFIX_BYTES..].copy_from_slice(&bound[..32 - SEAM_LEFT_OUT_PREFIX_BYTES]);
    Sha256Digest::from_bytes(marker)
}

/// Whether `digest` is a left-out marker of `entity`, whatever source row it
/// was bound to.
fn is_seam_left_out_digest(
    entity: SourceDbProjectionStateEntity,
    digest: &Sha256Digest,
) -> bool {
    digest.as_bytes()[..SEAM_LEFT_OUT_PREFIX_BYTES]
        == seam_left_out_prefix(entity)[..SEAM_LEFT_OUT_PREFIX_BYTES]
}

fn seam_left_out_prefix(entity: SourceDbProjectionStateEntity) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update([0]);
    hasher.update(b"import seam: left out");
    hasher.update([0]);
    hasher.update(entity.as_str().as_bytes());
    hasher.finalize().into()
}

/// The entities whose published form the seam can change.
const SEAM_FORM_ENTITIES: [SourceDbProjectionStateEntity; 3] = [
    SourceDbProjectionStateEntity::Position,
    SourceDbProjectionStateEntity::Charge,
    SourceDbProjectionStateEntity::ChargeSample,
];

impl SourceDbProjectionState {
    /// Keeps the canonical payload of one candidate row while the source is
    /// read, whatever its digest; `create` makes the table first.
    pub(crate) fn retain_seam_candidate(
        &mut self,
        create: bool,
        entity: SourceDbProjectionStateEntity,
        id: i64,
        car_id: i64,
        payload: &[u8],
    ) -> Result<(), SourceDbProjectionStateError> {
        self.require_open()?;
        validate_row_identity(id, car_id)?;
        let payload = std::str::from_utf8(payload)
            .map_err(SourceDbProjectionStateError::CanonicalPayloadUtf8)?;
        if create {
            self.connection
                .execute_batch(&format!(
                    "CREATE TABLE IF NOT EXISTS {SEAM_RETAINED_TABLE} (
                         entity_ordinal INTEGER NOT NULL,
                         entity_id INTEGER NOT NULL,
                         car_id INTEGER NOT NULL,
                         payload_json TEXT NOT NULL,
                         PRIMARY KEY(entity_ordinal, entity_id)
                     ) STRICT, WITHOUT ROWID"
                ))
                .map_err(SourceDbProjectionStateError::Sqlite)?;
        }
        self.connection
            .prepare_cached(&format!(
                "INSERT INTO {SEAM_RETAINED_TABLE}(entity_ordinal, entity_id, car_id, payload_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(entity_ordinal, entity_id) DO NOTHING"
            ))
            .and_then(|mut statement| {
                statement.execute(params![i64::from(entity.ordinal()), id, car_id, payload])
            })
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        Ok(())
    }

    /// Applies the seam's published forms to the sealed state: every changed
    /// or retained position, charge and charge sample is held in the form the
    /// packs publish (`form`), changed only when that form differs from the
    /// one `prior` records; a left-out row holds the marker digest. Without
    /// `scan` (a plan that publishes every row as recorded) nothing is read.
    /// The retained payloads are then dropped. The state stays sealed.
    pub(crate) fn apply_seam_forms(
        &mut self,
        scan: bool,
        prior: &mut dyn PriorProjectionStateLookup,
        form: &mut dyn FnMut(
            SourceDbProjectionStateEntity,
            i64,
            i64,
            &[u8],
        ) -> Result<SeamForm, SourceDbProjectionStateError>,
    ) -> Result<SeamFormsApplied, SourceDbProjectionStateError> {
        self.require_sealed()?;
        let retained: bool = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                params![SEAM_RETAINED_TABLE],
                |row| row.get(0),
            )
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        let result = if scan {
            self.apply_seam_forms_in_transaction(retained, prior, form)
        } else {
            self.drop_seam_retained(retained).map(|()| SeamFormsApplied::default())
        };
        let finish = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
        let finished = self
            .connection
            .execute_batch(finish)
            .map_err(SourceDbProjectionStateError::Sqlite);
        let applied = result?;
        finished?;
        let (changed_rows, changed_bytes): (i64, i64) = self
            .connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(payload_bytes), 0) FROM changed_rows",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        self.changed_row_count = u64::try_from(changed_rows)
            .map_err(|_| SourceDbProjectionStateError::InvalidStoredAccounting)?;
        self.changed_payload_bytes = u64::try_from(changed_bytes)
            .map_err(|_| SourceDbProjectionStateError::InvalidStoredAccounting)?;
        Ok(applied)
    }

    fn apply_seam_forms_in_transaction(
        &mut self,
        retained: bool,
        prior: &mut dyn PriorProjectionStateLookup,
        form: &mut dyn FnMut(
            SourceDbProjectionStateEntity,
            i64,
            i64,
            &[u8],
        ) -> Result<SeamForm, SourceDbProjectionStateError>,
    ) -> Result<SeamFormsApplied, SourceDbProjectionStateError> {
        let mut applied = SeamFormsApplied::default();
        let source = if retained {
            format!(
                "SELECT entity_ordinal, entity_id, car_id, payload_json, 1 FROM changed_rows
                  WHERE entity_ordinal IN (?1, ?2, ?3)
                 UNION ALL
                 SELECT kept.entity_ordinal, kept.entity_id, kept.car_id, kept.payload_json, 0
                   FROM {SEAM_RETAINED_TABLE} AS kept
                  WHERE NOT EXISTS (SELECT 1 FROM changed_rows AS changed
                                     WHERE changed.entity_ordinal = kept.entity_ordinal
                                       AND changed.entity_id = kept.entity_id)"
            )
        } else {
            "SELECT entity_ordinal, entity_id, car_id, payload_json, 1 FROM changed_rows
              WHERE entity_ordinal IN (?1, ?2, ?3)"
                .to_owned()
        };
        let page_sql = format!(
            "SELECT * FROM ({source}) WHERE (entity_ordinal, entity_id) > (?4, ?5)
              ORDER BY 1, 2 LIMIT 1000"
        );
        let ordinals = SEAM_FORM_ENTITIES.map(|entity| i64::from(entity.ordinal()));
        let mut after = (i64::MIN, i64::MIN);
        loop {
            let page: Vec<(i64, i64, i64, String, bool)> = {
                let mut statement = self
                    .connection
                    .prepare_cached(&page_sql)
                    .map_err(SourceDbProjectionStateError::Sqlite)?;
                statement
                    .query_map(
                        params![ordinals[0], ordinals[1], ordinals[2], after.0, after.1],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                    )
                    .map_err(SourceDbProjectionStateError::Sqlite)?
                    .collect::<Result<_, _>>()
                    .map_err(SourceDbProjectionStateError::Sqlite)?
            };
            let Some(last) = page.last() else {
                break;
            };
            after = (last.0, last.1);
            for (ordinal, id, car_id, payload, changed) in page {
                let entity = SourceDbProjectionStateEntity::from_ordinal(ordinal)?;
                // A row published as recorded needs nothing: unchanged, its
                // digest is the prior one; changed, it stays changed.
                let form = form(entity, id, car_id, payload.as_bytes())?;
                if form == SeamForm::Recorded {
                    continue;
                }
                let prior_digest = prior
                    .digest(entity, id)
                    .map_err(SourceDbProjectionStateError::PriorLookup)?;
                match form {
                    SeamForm::Recorded => {}
                    SeamForm::LeftOut => {
                        // The current digest is still the source row's own.
                        let (_, source) = self.current_seam_digest(ordinal, id)?;
                        let marker = seam_left_out_digest(entity, &source);
                        // Clients hold it only when an earlier import
                        // published it.
                        if prior_digest
                            .is_some_and(|digest| !is_seam_left_out_digest(entity, &digest))
                        {
                            applied.withdrawn.push((entity, id, car_id));
                        }
                        // Counted when the source row is new or changed since
                        // the last import, whether or not it was left out then.
                        if changed && prior_digest != Some(marker) {
                            match entity {
                                SourceDbProjectionStateEntity::Position => {
                                    applied.positions_left_out += 1;
                                }
                                SourceDbProjectionStateEntity::Charge => {
                                    applied.charges_left_out += 1;
                                }
                                _ => applied.charge_samples_left_out += 1,
                            }
                        }
                        self.hold_seam_form(ordinal, id, car_id, marker, None)?;
                    }
                    SeamForm::Published { payload, digest } => {
                        let published = (prior_digest != Some(digest)).then_some(payload.as_slice());
                        self.hold_seam_form(ordinal, id, car_id, digest, published)?;
                    }
                }
            }
        }
        self.drop_seam_retained(retained)?;
        Ok(applied)
    }

    /// Takes the held seam rows out of the state, so the state's transfer
    /// sees only its own tables, for [`Self::restore_seam_retained`]: a
    /// window rebind (DEC-150) transfers its base state before its seam.
    pub(crate) fn take_seam_retained(&mut self) -> Result<SeamRetainedRows, SourceDbProjectionStateError> {
        let retained: bool = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                params![SEAM_RETAINED_TABLE],
                |row| row.get(0),
            )
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        if !retained {
            return Ok(SeamRetainedRows(Vec::new()));
        }
        let rows = {
            let mut statement = self
                .connection
                .prepare(&format!(
                    "SELECT entity_ordinal, entity_id, car_id, payload_json FROM {SEAM_RETAINED_TABLE}
                      ORDER BY entity_ordinal, entity_id"
                ))
                .map_err(SourceDbProjectionStateError::Sqlite)?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
                .map_err(SourceDbProjectionStateError::Sqlite)?
                .collect::<Result<Vec<(i64, i64, i64, String)>, _>>()
                .map_err(SourceDbProjectionStateError::Sqlite)?
        };
        self.drop_seam_retained(true)?;
        Ok(SeamRetainedRows(rows))
    }

    /// Puts rows [`Self::take_seam_retained`] took back.
    pub(crate) fn restore_seam_retained(
        &mut self,
        rows: SeamRetainedRows,
    ) -> Result<(), SourceDbProjectionStateError> {
        if rows.0.is_empty() {
            return Ok(());
        }
        self.connection
            .execute_batch(&format!(
                    "CREATE TABLE IF NOT EXISTS {SEAM_RETAINED_TABLE} (
                         entity_ordinal INTEGER NOT NULL,
                         entity_id INTEGER NOT NULL,
                         car_id INTEGER NOT NULL,
                         payload_json TEXT NOT NULL,
                         PRIMARY KEY(entity_ordinal, entity_id)
                     ) STRICT, WITHOUT ROWID"
                ))
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        let mut insert = self
            .connection
            .prepare(&format!(
                "INSERT INTO {SEAM_RETAINED_TABLE}(entity_ordinal, entity_id, car_id, payload_json)
                 VALUES (?1, ?2, ?3, ?4)"
            ))
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        for (ordinal, id, car_id, payload) in rows.0 {
            insert
                .execute(params![ordinal, id, car_id, payload])
                .map_err(SourceDbProjectionStateError::Sqlite)?;
        }
        Ok(())
    }

    fn drop_seam_retained(&self, retained: bool) -> Result<(), SourceDbProjectionStateError> {
        if retained {
            self.connection
                .execute_batch(&format!("DROP TABLE {SEAM_RETAINED_TABLE}"))
                .map_err(SourceDbProjectionStateError::Sqlite)?;
        }
        Ok(())
    }

    /// The current row's car and digest.
    fn current_seam_digest(
        &self,
        ordinal: i64,
        id: i64,
    ) -> Result<(i64, Sha256Digest), SourceDbProjectionStateError> {
        let (car_id, digest): (i64, Vec<u8>) = self
            .connection
            .query_row(
                "SELECT car_id, projection_sha256 FROM current_rows
                  WHERE entity_ordinal = ?1 AND entity_id = ?2",
                params![ordinal, id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        Ok((car_id, digest_from_blob(digest)?))
    }

    /// Holds one row with `digest`, changed with `payload` when given and
    /// unchanged otherwise.
    fn hold_seam_form(
        &mut self,
        ordinal: i64,
        id: i64,
        car_id: i64,
        digest: Sha256Digest,
        payload: Option<&[u8]>,
    ) -> Result<(), SourceDbProjectionStateError> {
        let (stored_car_id, _) = self.current_seam_digest(ordinal, id)?;
        if stored_car_id != car_id {
            return Err(SourceDbProjectionStateError::InvalidStoredAccounting);
        }
        self.connection
            .execute(
                "DELETE FROM changed_rows WHERE entity_ordinal = ?1 AND entity_id = ?2",
                params![ordinal, id],
            )
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        self.connection
            .execute(
                "UPDATE current_rows SET projection_sha256 = ?3
                  WHERE entity_ordinal = ?1 AND entity_id = ?2",
                params![ordinal, id, digest.as_bytes().as_slice()],
            )
            .map_err(SourceDbProjectionStateError::Sqlite)?;
        if let Some(payload) = payload {
            let payload = std::str::from_utf8(payload)
                .map_err(SourceDbProjectionStateError::CanonicalPayloadUtf8)?;
            let payload_bytes = i64::try_from(payload.len())
                .map_err(|_| SourceDbProjectionStateError::InvalidStoredAccounting)?;
            self.connection
                .execute(
                    "INSERT INTO changed_rows(entity_ordinal, entity_id, car_id, projection_sha256,
                         payload_json, payload_bytes)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        ordinal,
                        id,
                        car_id,
                        digest.as_bytes().as_slice(),
                        payload,
                        payload_bytes
                    ],
                )
                .map_err(SourceDbProjectionStateError::Sqlite)?;
        }
        Ok(())
    }
}
