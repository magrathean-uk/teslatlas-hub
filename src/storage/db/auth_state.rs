// SPDX-License-Identifier: AGPL-3.0-only

// Durable status of the legacy credential unit (C5 section 3.3 and R43, R52,
// R52a, R57, R58, R62): the singleton `collector_auth_state` row of the v71
// step, the reads a restart needs (the generation the pair row holds, whether
// a generation was produced by one of the unit's own refreshes, the start time
// of the last token receipt) and the Owner-wire credential identity that C3's
// configuration fingerprint reads. The readiness the row gives is in
// `readiness.rs`.
// Everything here is plain data. The unit's state machine
// (`auth/legacy_auth/state.rs`) decides what the row says; this file stores it.
// No token, URL, vehicle or location is stored (R53).

/// The status row of `collector_auth_state` (C5 section 3.3) without its write
/// time. State and reason are the wire codes of R52, kept as text here so the
/// store needs no knowledge of the state machine; the unit parses them.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CollectorAuthRow {
    pub state: String,
    pub reason: Option<String>,
    /// R52a: 1 exactly when the state collects at the moment of the write.
    pub collecting: bool,
    /// R31: the earliest next token attempt while a retry is pending. Stored
    /// for display only (R59).
    pub retry_at: Option<i64>,
    /// R43: the generation that was current when a `sign_in_required` or
    /// `signed_out` stop was set.
    pub stopped_generation: Option<Uuid>,
    /// R43: C3 R1's mode code (`legacy`, `provider`) in force when
    /// `api_refused` was set.
    pub stopped_mode: Option<String>,
    /// R41: consecutive rejected generations, 0 to 3.
    pub fuse_consecutive: u8,
    pub fuse_generation: Option<Uuid>,
    pub fuse_last_rejection_at: Option<i64>,
    pub fuse_pause_until: Option<i64>,
    /// The digest of the access token the last counted rejection rejected (hub fix pass
    /// 3), stored for every counted rejection that is not a stop. A restart derives the
    /// forced refresh a count owes from it: owed while the stored pair still holds that
    /// token. A row migrated from before the column holds none at a count above zero; the
    /// unit fills it from the stored pair when it starts.
    pub fuse_rejected_access_digest: Option<Uuid>,
    /// R55.
    pub clock_skew: bool,
}

impl std::fmt::Debug for CollectorAuthRow {
    /// A generation identifies a refresh token (it is a digest of it), so only
    /// whether each is present is shown (R53; B2 round 3, SEC-R3-N2), as the
    /// fuse snapshot and the status do. Equality still compares the generations.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CollectorAuthRow")
            .field("state", &self.state)
            .field("reason", &self.reason)
            .field("collecting", &self.collecting)
            .field("retry_at", &self.retry_at)
            .field("has_stopped_generation", &self.stopped_generation.is_some())
            .field("stopped_mode", &self.stopped_mode)
            .field("fuse_consecutive", &self.fuse_consecutive)
            .field("has_fuse_generation", &self.fuse_generation.is_some())
            .field("fuse_last_rejection_at", &self.fuse_last_rejection_at)
            .field("fuse_pause_until", &self.fuse_pause_until)
            .field(
                "has_fuse_rejected_access_digest",
                &self.fuse_rejected_access_digest.is_some(),
            )
            .field("clock_skew", &self.clock_skew)
            .finish()
    }
}

/// What the pair row holds, read without decrypting it (C5 R44a, R62).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoredLegacyGeneration {
    /// No pair row exists.
    NoRow,
    /// A row whose `credential_generation` is NULL: written by an external
    /// writer that did not compute one. A refresh of this unit always writes
    /// one (K2), so such a row is never its own.
    Unbound,
    /// The generation of the row.
    Bound(Uuid),
}

impl HubStore {
    /// Read the status row, or `None` before the unit has run (R57 step 5).
    pub(crate) fn load_collector_auth_row(&self) -> Result<Option<CollectorAuthRow>, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT state, reason, collecting, retry_at, stopped_generation,
                        stopped_mode, fuse_consecutive, fuse_generation,
                        fuse_last_rejection_at, fuse_pause_until, clock_skew,
                        fuse_rejected_access_digest
                   FROM collector_auth_state WHERE singleton_id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, Option<String>>(11)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::Query)?
            .map(
                |(
                    state,
                    reason,
                    collecting,
                    retry_at,
                    stopped_generation,
                    stopped_mode,
                    fuse_consecutive,
                    fuse_generation,
                    fuse_last_rejection_at,
                    fuse_pause_until,
                    clock_skew,
                    fuse_rejected_access_digest,
                )| {
                    Ok(CollectorAuthRow {
                        state,
                        reason,
                        collecting: collecting != 0,
                        retry_at,
                        stopped_generation: stopped_generation
                            .as_deref()
                            .map(|value| parse_stored_uuid("collector auth stopped generation", value))
                            .transpose()?,
                        stopped_mode,
                        fuse_consecutive: u8::try_from(fuse_consecutive)
                            .map_err(|_| StoreError::InvalidStoredAuthStatus)?,
                        fuse_generation: fuse_generation
                            .as_deref()
                            .map(|value| parse_stored_uuid("collector auth fuse generation", value))
                            .transpose()?,
                        fuse_last_rejection_at,
                        fuse_pause_until,
                        fuse_rejected_access_digest: fuse_rejected_access_digest
                            .as_deref()
                            .map(|value| {
                                parse_stored_uuid("collector auth rejected access digest", value)
                            })
                            .transpose()?,
                        clock_skew: clock_skew != 0,
                    })
                },
            )
            .transpose()
    }

    /// Write the status row (R52a): one single-row upsert in its own
    /// transaction. Returns whether a row changed: a row that equals the stored
    /// one in every column but the write time is left alone, so repeating a
    /// write is idempotent and an unchanged status costs no write (R52a: never
    /// when nothing changed; R62).
    pub(crate) fn write_collector_auth_row(
        &self,
        row: &CollectorAuthRow,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_timestamp("collector auth state", now_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let changed = transaction
            .execute(
                "INSERT INTO collector_auth_state(
                    singleton_id, state, reason, collecting, retry_at,
                    stopped_generation, stopped_mode, fuse_consecutive,
                    fuse_generation, fuse_last_rejection_at, fuse_pause_until,
                    clock_skew, fuse_rejected_access_digest, updated_at_ms)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?13, ?12)
                 ON CONFLICT(singleton_id) DO UPDATE SET
                    state = excluded.state,
                    reason = excluded.reason,
                    collecting = excluded.collecting,
                    retry_at = excluded.retry_at,
                    stopped_generation = excluded.stopped_generation,
                    stopped_mode = excluded.stopped_mode,
                    fuse_consecutive = excluded.fuse_consecutive,
                    fuse_generation = excluded.fuse_generation,
                    fuse_last_rejection_at = excluded.fuse_last_rejection_at,
                    fuse_pause_until = excluded.fuse_pause_until,
                    clock_skew = excluded.clock_skew,
                    fuse_rejected_access_digest = excluded.fuse_rejected_access_digest,
                    updated_at_ms = excluded.updated_at_ms
                  WHERE state IS NOT excluded.state
                     OR reason IS NOT excluded.reason
                     OR collecting IS NOT excluded.collecting
                     OR retry_at IS NOT excluded.retry_at
                     OR stopped_generation IS NOT excluded.stopped_generation
                     OR stopped_mode IS NOT excluded.stopped_mode
                     OR fuse_consecutive IS NOT excluded.fuse_consecutive
                     OR fuse_generation IS NOT excluded.fuse_generation
                     OR fuse_last_rejection_at IS NOT excluded.fuse_last_rejection_at
                     OR fuse_pause_until IS NOT excluded.fuse_pause_until
                     OR clock_skew IS NOT excluded.clock_skew
                     OR fuse_rejected_access_digest IS NOT excluded.fuse_rejected_access_digest",
                params![
                    row.state,
                    row.reason,
                    i64::from(row.collecting),
                    row.retry_at,
                    row.stopped_generation.map(|value| value.to_string()),
                    row.stopped_mode,
                    i64::from(row.fuse_consecutive),
                    row.fuse_generation.map(|value| value.to_string()),
                    row.fuse_last_rejection_at,
                    row.fuse_pause_until,
                    i64::from(row.clock_skew),
                    now_ms,
                    row.fuse_rejected_access_digest.map(|value| value.to_string()),
                ],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(changed > 0)
    }

    /// Fill the missing rejected-access digest of the stored status row (hub fix pass 4,
    /// F4-2): a row migrated from before the column counts a rejection and holds no digest.
    /// One immediate transaction commits the fill only while the pair row still holds the
    /// ciphertexts `pair` was read from and the status row still holds a NULL digest for
    /// the fuse the caller restored (`consecutive`, `generation`). Anything else, or a
    /// store fault, is an error: the caller does not go on to refresh on a row whose
    /// obligation is not durable. It leaves the write time alone, as the fill changes no
    /// status the owner sees.
    pub(crate) fn fill_collector_auth_rejected_access_digest(
        &self,
        pair: &TeslaMateLegacyTokenStore,
        consecutive: u8,
        generation: Option<Uuid>,
        digest: Uuid,
    ) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let same_pair: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM teslamate_legacy_tokens
                  WHERE singleton_id = 1 AND access = ?1 AND refresh = ?2)",
                params![pair.access(), pair.refresh()],
                |row| row.get(0),
            )
            .map_err(StoreError::TeslaMateTokenStore)?;
        if !same_pair {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        let changed = transaction
            .execute(
                "UPDATE collector_auth_state
                    SET fuse_rejected_access_digest = ?1
                  WHERE singleton_id = 1 AND fuse_rejected_access_digest IS NULL
                    AND fuse_consecutive = ?2 AND fuse_generation IS ?3",
                params![
                    digest.to_string(),
                    i64::from(consecutive),
                    generation.map(|value| value.to_string()),
                ],
            )
            .map_err(StoreError::Query)?;
        if changed != 1 {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        transaction.commit().map_err(StoreError::Query)
    }

    /// C3 R7 (B2 clarification 3), inside the ordinary start's transaction: the
    /// stored C5 status is an `api_refused` stopped in a mode other than
    /// `requested_mode` (C3's mode code); the row's own check requires a mode
    /// on every refusal.
    fn c5_refusal_stopped_in_another_mode(
        transaction: &Transaction<'_>,
        requested_mode: &str,
    ) -> Result<bool, StoreError> {
        transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM collector_auth_state
                  WHERE singleton_id = 1 AND state = 'api_refused'
                    AND stopped_mode IS NOT ?1)",
                params![requested_mode],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    /// The C5-owned refusal lift of the ordinary start (C3 R7, C5 R44 (c),
    /// R52a transaction exception), run inside `begin_collector_run`'s immediate
    /// transaction in every mode: an existing `api_refused` row becomes the
    /// neutral ready row (collecting, no reason, retry, stopped generation or
    /// stopped mode) with `start_ms` as its write time. The fuse fields and the
    /// clock-skew flag stay; a missing row is not inserted and any other state
    /// is left alone. Returns whether a row changed.
    fn lift_collector_auth_refusal_in_transaction(
        transaction: &Transaction<'_>,
        start_ms: i64,
    ) -> Result<bool, StoreError> {
        let changed = transaction
            .execute(
                "UPDATE collector_auth_state
                    SET state = 'ready', reason = NULL, collecting = 1,
                        retry_at = NULL, stopped_generation = NULL,
                        stopped_mode = NULL, updated_at_ms = ?1
                  WHERE singleton_id = 1 AND state = 'api_refused'",
                params![start_ms],
            )
            .map_err(StoreError::Query)?;
        Ok(changed > 0)
    }

    /// The generation the pair row holds, without decrypting it. The unit
    /// compares it with its own before every token request, while stopped or
    /// waiting and at start (R44a).
    pub(crate) fn stored_legacy_credential_generation(
        &self,
    ) -> Result<StoredLegacyGeneration, StoreError> {
        let connection = self.open_read_only_connection()?;
        let row: Option<Option<String>> = connection
            .query_row(
                "SELECT credential_generation FROM teslamate_legacy_tokens
                  WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::TeslaMateTokenStore)?;
        Ok(match row {
            None => StoredLegacyGeneration::NoRow,
            Some(None) => StoredLegacyGeneration::Unbound,
            Some(Some(value)) => StoredLegacyGeneration::Bound(
                Uuid::parse_str(&value).map_err(|_| StoreError::InvalidLegacyRefreshGeneration)?,
            ),
        })
    }

    /// Whether `generation` is the output of a completed refresh binding, that
    /// is, a pair this unit's own refresh wrote (K2, R62). Any other
    /// generation that appears in the pair row came from an external writer.
    pub(crate) fn legacy_generation_is_refresh_output(
        &self,
        generation: Uuid,
    ) -> Result<bool, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM legacy_refresh_receipt_bindings
                     WHERE output_credential_generation = ?1
                )",
                params![generation.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LegacyRefreshReceipt)
    }

    /// The start time of the most recent `legacy_auth` token receipt, whatever
    /// its outcome (R58). `None` when no receipt is kept: receipts are pruned
    /// after `OUTBOUND_REQUEST_RETENTION_MS`, far beyond the 60 s the floor
    /// needs.
    pub(crate) fn latest_legacy_refresh_started_at_ms(&self) -> Result<Option<i64>, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT MAX(started_at_ms) FROM outbound_request_receipts
                  WHERE transport = 'legacy_auth' AND operation = 'token_refresh'",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LegacyRefreshReceipt)
    }

    /// The Owner-wire credential identity for C3's configuration fingerprint
    /// (B2 clarification of 2026-10-12, C3 R4 and R7): the generation of the
    /// last external credential write. It follows the completed refresh
    /// bindings from the stored `credential_generation` back to a generation
    /// that no completed binding produced, so this unit's own refreshes never
    /// change it and any external write does. A row without a generation, or
    /// no row, gives the empty string. It reads columns only, needs no
    /// sensitive access and may be called before the unit starts.
    pub(crate) fn owner_wire_credential_identity(&self) -> Result<String, StoreError> {
        let connection = self.open_read_only_connection()?;
        let current: Option<Option<String>> = connection
            .query_row(
                "SELECT credential_generation FROM teslamate_legacy_tokens
                  WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::TeslaMateTokenStore)?;
        let Some(Some(current)) = current else {
            return Ok(String::new());
        };
        let mut generation = Uuid::parse_str(&current)
            .map_err(|_| StoreError::InvalidLegacyRefreshGeneration)?;
        let mut statement = connection
            .prepare(
                "SELECT input_credential_generation FROM legacy_refresh_receipt_bindings
                  WHERE output_credential_generation = ?1",
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        // The output column is unique and never equals the input of its own
        // binding, so a walk only repeats on a damaged table. A repeat would
        // never end, so it is refused rather than guessed at.
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(generation) {
                return Err(StoreError::InvalidLegacyRefreshGeneration);
            }
            let input: Option<String> = statement
                .query_row(params![generation.to_string()], |row| row.get(0))
                .optional()
                .map_err(StoreError::LegacyRefreshReceipt)?;
            match input {
                Some(input) => {
                    generation = Uuid::parse_str(&input)
                        .map_err(|_| StoreError::InvalidLegacyRefreshGeneration)?;
                }
                None => return Ok(generation.to_string()),
            }
        }
    }
}
