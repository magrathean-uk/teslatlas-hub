// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    /// Atomically replace the sole persisted TeslaMate token pair.
    pub fn replace_teslamate_legacy_tokens(
        &self,
        tokens: &TeslaMateLegacyTokenStore,
    ) -> Result<(), StoreError> {
        let replacement_generation = tokens
            .credential_generation()
            .map(|value| value.to_string());
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if let Some(generation) = replacement_generation.as_ref() {
            let consumed: bool = transaction
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM legacy_refresh_input_fences
                         WHERE input_credential_generation = ?1
                    )",
                    params![generation],
                    |row| row.get(0),
                )
                .map_err(StoreError::LegacyRefreshReceipt)?;
            if consumed {
                return Err(StoreError::LegacyRefreshOutcomeUnknown);
            }
        }
        let unresolved_inputs = transaction
            .prepare(
                "SELECT b.input_credential_generation
                   FROM legacy_refresh_receipt_bindings AS b
                   JOIN outbound_request_receipts AS r ON r.id = b.receipt_id
                  WHERE r.outcome = 'started'",
            )
            .map_err(StoreError::LegacyRefreshReceipt)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(StoreError::LegacyRefreshReceipt)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if !unresolved_inputs.is_empty() {
            let current_generation: Option<String> = transaction
                .query_row(
                    "SELECT credential_generation FROM teslamate_legacy_tokens
                      WHERE singleton_id = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::TeslaMateTokenStore)?
                .flatten();
            if unresolved_inputs.len() != 1
                || current_generation.as_ref() != unresolved_inputs.first()
                || replacement_generation.is_none()
                || replacement_generation.as_ref() == unresolved_inputs.first()
            {
                return Err(StoreError::LegacyRefreshOutcomeUnknown);
            }
            // An explicit setup/import with a different plaintext refresh
            // authority is the operator recovery boundary. Random encryption
            // nonces cannot make the same consumed refresh token look new.
            let completed_at_ms = outbound_request_clock_ms()?;
            transaction
                .execute(
                    "UPDATE outbound_request_receipts
                        SET completed_at_ms = MAX(?1, started_at_ms),
                            duration_ms = MAX(?1, started_at_ms) - started_at_ms,
                            outcome = 'cancelled'
                      WHERE outcome = 'started'
                        AND id IN (
                            SELECT receipt_id FROM legacy_refresh_receipt_bindings
                        )",
                    params![completed_at_ms],
                )
                .map_err(StoreError::LegacyRefreshReceipt)?;
        }
        transaction
            .execute(
                "INSERT INTO teslamate_legacy_tokens(
                    singleton_id, access, refresh, expires_at, next_refresh_at,
                    credential_generation
                 ) VALUES (1, ?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(singleton_id) DO UPDATE SET
                    access = excluded.access,
                    refresh = excluded.refresh,
                    expires_at = excluded.expires_at,
                    next_refresh_at = excluded.next_refresh_at,
                    credential_generation = excluded.credential_generation",
                params![
                    tokens.access(),
                    tokens.refresh(),
                    tokens.expires_at(),
                    tokens.next_refresh_at(),
                    replacement_generation,
                ],
            )
            .map_err(StoreError::TeslaMateTokenStore)?;
        transaction
            .commit()
            .map_err(StoreError::TeslaMateTokenStore)
    }

    /// Bind a legacy row to the deterministic plaintext refresh identity after
    /// authenticated decryption. This is the one-time v51-to-v52 upgrade path.
    pub(crate) fn bind_teslamate_legacy_credential_generation(
        &self,
        tokens: &TeslaMateLegacyTokenStore,
        credential_generation: Uuid,
    ) -> Result<(), StoreError> {
        if credential_generation.is_nil() {
            return Err(StoreError::InvalidLegacyRefreshGeneration);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current: Option<(Vec<u8>, Vec<u8>, Option<String>)> = transaction
            .query_row(
                "SELECT access, refresh, credential_generation
                   FROM teslamate_legacy_tokens WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::TeslaMateTokenStore)?;
        let Some((access, refresh, stored_generation)) = current else {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        };
        let expected = credential_generation.to_string();
        if access != tokens.access() || refresh != tokens.refresh() {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        match stored_generation {
            Some(stored) if stored == expected => {}
            Some(_) => return Err(StoreError::LegacyRefreshOutcomeUnknown),
            None => {
                let changed = transaction
                    .execute(
                        "UPDATE teslamate_legacy_tokens
                            SET credential_generation = ?1
                          WHERE singleton_id = 1 AND credential_generation IS NULL
                            AND access = ?2 AND refresh = ?3",
                        params![expected, tokens.access(), tokens.refresh()],
                    )
                    .map_err(StoreError::TeslaMateTokenStore)?;
                if changed != 1 {
                    return Err(StoreError::LegacyRefreshOutcomeUnknown);
                }
            }
        }
        transaction
            .commit()
            .map_err(StoreError::TeslaMateTokenStore)
    }

    /// Reserve the single-use legacy refresh input before any token HTTP
    /// request. The receipt, input fence, and binding commit together.
    pub(crate) fn begin_legacy_refresh(
        &self,
        input_generation: Uuid,
    ) -> Result<OutboundRequestReceiptId, StoreError> {
        if input_generation.is_nil() {
            return Err(StoreError::InvalidLegacyRefreshGeneration);
        }
        let started_at_ms = outbound_request_clock_ms()?;
        let correlation_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let mut connection = self.open_for_credential_commit()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current_generation: Option<String> = transaction
            .query_row(
                "SELECT credential_generation FROM teslamate_legacy_tokens WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::TeslaMateTokenStore)?
            .flatten();
        if current_generation.as_deref() != Some(input_generation.to_string().as_str()) {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        let unresolved: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1
                      FROM outbound_request_receipts AS r
                      JOIN legacy_refresh_receipt_bindings AS b
                        ON b.receipt_id = r.id
                     WHERE r.transport = 'legacy_auth'
                       AND r.operation = 'token_refresh'
                       AND r.outcome = 'started'
                )",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if unresolved {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        let fenced: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM legacy_refresh_input_fences
                     WHERE input_credential_generation = ?1
                )",
                params![input_generation.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if fenced {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        ensure_outbound_request_capacity(&transaction)?;
        let fence_count: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM legacy_refresh_input_fences",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if fence_count >= MAX_LEGACY_REFRESH_INPUT_FENCES {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        transaction
            .execute(
                "INSERT INTO outbound_request_receipts(
                    correlation_id, started_at_ms, vehicle_tesla_id, transport,
                    operation, safety_class, precondition, outcome
                 ) VALUES (?1, ?2, NULL, 'legacy_auth', 'token_refresh',
                           'non_wake_endpoint', 'not_required', 'started')",
                params![correlation_id.to_string(), started_at_ms],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        let receipt_id = OutboundRequestReceiptId(transaction.last_insert_rowid());
        transaction
            .execute(
                "INSERT INTO legacy_refresh_input_fences(input_credential_generation)
                 VALUES (?1)",
                params![input_generation.to_string()],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        transaction
            .execute(
                "INSERT INTO legacy_refresh_receipt_bindings(
                    receipt_id, attempt_id, input_credential_generation
                 ) VALUES (?1, ?2, ?3)",
                params![
                    receipt_id.0,
                    attempt_id.to_string(),
                    input_generation.to_string()
                ],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        transaction
            .commit()
            .map_err(StoreError::LegacyRefreshReceipt)?;
        Ok(receipt_id)
    }

    /// Atomically publish the encrypted successor and close its refresh
    /// receipt. A crash before this commit leaves the input fenced and the
    /// started receipt unresolved, so restart refuses another refresh.
    pub(crate) fn complete_legacy_refresh(
        &self,
        receipt_id: OutboundRequestReceiptId,
        input_generation: Uuid,
        output_generation: Uuid,
        tokens: &TeslaMateLegacyTokenStore,
    ) -> Result<(), StoreError> {
        if receipt_id.0 <= 0
            || input_generation.is_nil()
            || output_generation.is_nil()
            || input_generation == output_generation
            || tokens.credential_generation() != Some(output_generation)
        {
            return Err(StoreError::InvalidLegacyRefreshGeneration);
        }
        let mut connection = self.open_for_credential_commit()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let stored_input: String = transaction
            .query_row(
                "SELECT b.input_credential_generation
                   FROM legacy_refresh_receipt_bindings AS b
                   JOIN outbound_request_receipts AS r ON r.id = b.receipt_id
                  WHERE b.receipt_id = ?1 AND r.outcome = 'started'",
                params![receipt_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::LegacyRefreshReceipt)?
            .ok_or(StoreError::LegacyRefreshOutcomeUnknown)?;
        if stored_input != input_generation.to_string() {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        let current_generation: Option<String> = transaction
            .query_row(
                "SELECT credential_generation FROM teslamate_legacy_tokens WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::TeslaMateTokenStore)?
            .flatten();
        if current_generation.as_deref() != Some(input_generation.to_string().as_str()) {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        let output_consumed: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM legacy_refresh_input_fences
                     WHERE input_credential_generation = ?1
                )",
                params![output_generation.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if output_consumed {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        transaction
            .execute(
                "INSERT INTO teslamate_legacy_tokens(
                    singleton_id, access, refresh, expires_at, next_refresh_at,
                    credential_generation
                 ) VALUES (1, ?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(singleton_id) DO UPDATE SET
                    access = excluded.access,
                    refresh = excluded.refresh,
                    expires_at = excluded.expires_at,
                    next_refresh_at = excluded.next_refresh_at,
                    credential_generation = excluded.credential_generation",
                params![
                    tokens.access(),
                    tokens.refresh(),
                    tokens.expires_at(),
                    tokens.next_refresh_at(),
                    output_generation.to_string(),
                ],
            )
            .map_err(StoreError::TeslaMateTokenStore)?;
        let bound = transaction
            .execute(
                "UPDATE legacy_refresh_receipt_bindings
                    SET output_credential_generation = ?2
                  WHERE receipt_id = ?1 AND output_credential_generation IS NULL",
                params![receipt_id.0, output_generation.to_string()],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if bound != 1 {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        let completed_at_ms = outbound_request_clock_ms()?.max(
            transaction
                .query_row(
                    "SELECT started_at_ms FROM outbound_request_receipts WHERE id = ?1",
                    params![receipt_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(StoreError::LegacyRefreshReceipt)?,
        );
        let completed = transaction
            .execute(
                "UPDATE outbound_request_receipts
                    SET completed_at_ms = ?2, duration_ms = ?2 - started_at_ms,
                        outcome = 'success', http_status = 200
                  WHERE id = ?1 AND outcome = 'started'",
                params![receipt_id.0, completed_at_ms],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if completed != 1 {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        transaction
            .commit()
            .map_err(StoreError::LegacyRefreshReceipt)
    }

    /// Close a refresh intent only when no network request was sent.
    pub(crate) fn cancel_unsent_legacy_refresh(
        &self,
        receipt_id: OutboundRequestReceiptId,
        input_generation: Uuid,
    ) -> Result<(), StoreError> {
        let mut connection = self.open_for_credential_commit()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let deleted = transaction
            .execute(
                "DELETE FROM legacy_refresh_receipt_bindings
                  WHERE receipt_id = ?1 AND input_credential_generation = ?2
                    AND EXISTS(
                        SELECT 1 FROM outbound_request_receipts
                         WHERE id = ?1 AND outcome = 'started'
                    )",
                params![receipt_id.0, input_generation.to_string()],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if deleted != 1 {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        transaction
            .execute(
                "DELETE FROM legacy_refresh_input_fences WHERE input_credential_generation = ?1",
                params![input_generation.to_string()],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        let completed_at_ms = outbound_request_clock_ms()?;
        transaction
            .execute(
                "UPDATE outbound_request_receipts
                    SET completed_at_ms = MAX(?2, started_at_ms),
                        duration_ms = MAX(?2, started_at_ms) - started_at_ms,
                        outcome = 'cancelled'
                  WHERE id = ?1 AND outcome = 'started'",
                params![receipt_id.0, completed_at_ms],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        transaction
            .commit()
            .map_err(StoreError::LegacyRefreshReceipt)
    }

    /// Close a refresh attempt whose request was sent and whose answer was a
    /// complete one, recording what happened (C5 R51, decision D1).
    ///
    /// `cancel_unsent_legacy_refresh` records `cancelled` with no status and is
    /// documented for attempts that sent nothing, so a sent attempt with a
    /// complete error answer had no truthful closing call. This function takes
    /// the same guarantees as the cancel: in one immediate transaction it
    /// requires a `started` receipt bound to `input_generation` (else
    /// `LegacyRefreshOutcomeUnknown`), sets `outcome`, `http_status`,
    /// `retry_after_seconds`, `completed_at_ms` and `duration_ms`, and, when
    /// `release_input` is true, deletes the binding and the input fence exactly
    /// as the cancel does. When it is false the binding and the fence stay, so
    /// `begin_legacy_refresh` refuses that input forever and
    /// `replace_teslamate_legacy_tokens` refuses to import it again: the store
    /// itself records the token as dead (R45 row 8, R50).
    ///
    /// Only the five outcomes a sent request can end in are accepted:
    /// `transport_error`, `timeout`, `http_error`, `authentication_rejected` and
    /// `success`. Rows 4 and 6 of the outcome table are not closed at all, so
    /// the attempt stays unresolved (R51).
    pub(crate) fn close_legacy_refresh_attempt(
        &self,
        receipt_id: OutboundRequestReceiptId,
        input_generation: Uuid,
        outcome: OutboundRequestOutcome,
        http_status: Option<u16>,
        retry_after_seconds: Option<u64>,
        release_input: bool,
    ) -> Result<(), StoreError> {
        if receipt_id.0 <= 0 || input_generation.is_nil() {
            return Err(StoreError::InvalidLegacyRefreshGeneration);
        }
        if !matches!(
            outcome,
            OutboundRequestOutcome::TransportError
                | OutboundRequestOutcome::Timeout
                | OutboundRequestOutcome::HttpError
                | OutboundRequestOutcome::AuthenticationRejected
                | OutboundRequestOutcome::Success
        ) {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        // The columns' own CHECKs would refuse these too; failing here keeps
        // the error typed and the transaction unopened.
        if http_status.is_some_and(|status| !(100..=599).contains(&status)) {
            return Err(StoreError::InvalidOutboundRequestHttpStatus);
        }
        let retry_after = retry_after_seconds
            .map(i64::try_from)
            .transpose()
            .map_err(|_| StoreError::InvalidOutboundRequestRetryAfter)?;
        let mut connection = self.open_for_credential_commit()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let bound: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1
                      FROM legacy_refresh_receipt_bindings AS b
                      JOIN outbound_request_receipts AS r ON r.id = b.receipt_id
                     WHERE b.receipt_id = ?1
                       AND b.input_credential_generation = ?2
                       AND r.transport = 'legacy_auth'
                       AND r.operation = 'token_refresh'
                       AND r.outcome = 'started'
                )",
                params![receipt_id.0, input_generation.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if !bound {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        let completed_at_ms = outbound_request_clock_ms()?;
        let closed = transaction
            .execute(
                "UPDATE outbound_request_receipts
                    SET completed_at_ms = MAX(?2, started_at_ms),
                        duration_ms = MAX(?2, started_at_ms) - started_at_ms,
                        outcome = ?3, http_status = ?4, retry_after_seconds = ?5
                  WHERE id = ?1 AND outcome = 'started'",
                params![
                    receipt_id.0,
                    completed_at_ms,
                    outcome.as_str(),
                    http_status,
                    retry_after
                ],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if closed != 1 {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        if release_input {
            transaction
                .execute(
                    "DELETE FROM legacy_refresh_receipt_bindings
                      WHERE receipt_id = ?1 AND input_credential_generation = ?2",
                    params![receipt_id.0, input_generation.to_string()],
                )
                .map_err(StoreError::LegacyRefreshReceipt)?;
            transaction
                .execute(
                    "DELETE FROM legacy_refresh_input_fences
                      WHERE input_credential_generation = ?1",
                    params![input_generation.to_string()],
                )
                .map_err(StoreError::LegacyRefreshReceipt)?;
        }
        transaction
            .commit()
            .map_err(StoreError::LegacyRefreshReceipt)
    }

    /// Publish the new access token of an answer that kept the refresh token
    /// and close its receipt, in one transaction (C5 R45 row 3, R23, R62).
    ///
    /// The refresh token was not consumed (F6), so the new pair has the
    /// generation of its input (a generation identifies the refresh token) and
    /// `complete_legacy_refresh` cannot record it: that function requires an
    /// output that differs from the input. Closing the attempt in one
    /// transaction and writing the pair in another left a window in which an
    /// external credential write was overwritten by the older refresh (C5 R24,
    /// R44a). Here the row is written only while it still holds `generation`, in
    /// the same transaction that closes the receipt as `success` and releases
    /// the input, so an external write makes this fail with
    /// `LegacyRefreshOutcomeUnknown` and its pair stays (the caller re-reads the
    /// row and adopts it, R24). It requires a `started` receipt bound to
    /// `generation` exactly as the cancel does.
    pub(crate) fn complete_unrotated_legacy_refresh(
        &self,
        receipt_id: OutboundRequestReceiptId,
        generation: Uuid,
        tokens: &TeslaMateLegacyTokenStore,
    ) -> Result<(), StoreError> {
        if receipt_id.0 <= 0
            || generation.is_nil()
            || tokens.credential_generation() != Some(generation)
        {
            return Err(StoreError::InvalidLegacyRefreshGeneration);
        }
        let mut connection = self.open_for_credential_commit()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let bound: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1
                      FROM legacy_refresh_receipt_bindings AS b
                      JOIN outbound_request_receipts AS r ON r.id = b.receipt_id
                     WHERE b.receipt_id = ?1
                       AND b.input_credential_generation = ?2
                       AND r.transport = 'legacy_auth'
                       AND r.operation = 'token_refresh'
                       AND r.outcome = 'started'
                )",
                params![receipt_id.0, generation.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if !bound {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        let written = transaction
            .execute(
                "UPDATE teslamate_legacy_tokens
                    SET access = ?1, refresh = ?2, expires_at = ?3, next_refresh_at = ?4
                  WHERE singleton_id = 1 AND credential_generation = ?5",
                params![
                    tokens.access(),
                    tokens.refresh(),
                    tokens.expires_at(),
                    tokens.next_refresh_at(),
                    generation.to_string(),
                ],
            )
            .map_err(StoreError::TeslaMateTokenStore)?;
        if written != 1 {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        let completed_at_ms = outbound_request_clock_ms()?;
        let closed = transaction
            .execute(
                "UPDATE outbound_request_receipts
                    SET completed_at_ms = MAX(?2, started_at_ms),
                        duration_ms = MAX(?2, started_at_ms) - started_at_ms,
                        outcome = 'success', http_status = 200
                  WHERE id = ?1 AND outcome = 'started'",
                params![receipt_id.0, completed_at_ms],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        if closed != 1 {
            return Err(StoreError::LegacyRefreshOutcomeUnknown);
        }
        transaction
            .execute(
                "DELETE FROM legacy_refresh_receipt_bindings
                  WHERE receipt_id = ?1 AND input_credential_generation = ?2",
                params![receipt_id.0, generation.to_string()],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        transaction
            .execute(
                "DELETE FROM legacy_refresh_input_fences WHERE input_credential_generation = ?1",
                params![generation.to_string()],
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        transaction
            .commit()
            .map_err(StoreError::LegacyRefreshReceipt)
    }

    /// The pair row and one refresh receipt, read in one consistent snapshot (C5
    /// R24, R62, CL-14). An equal-generation successor is proved by its row and its
    /// own receipt together: two separate reads could see the receipt closed by an
    /// attempt and the row of a writer that came after it.
    pub(crate) fn legacy_pair_and_receipt_snapshot(
        &self,
        receipt_id: OutboundRequestReceiptId,
    ) -> Result<LegacyPairSnapshot, StoreError> {
        let mut connection = self.open_read_only_connection()?;
        let transaction = connection.transaction().map_err(StoreError::Begin)?;
        let row: Option<(Vec<u8>, Vec<u8>, i64, i64, Option<String>)> = transaction
            .query_row(
                "SELECT access, refresh, expires_at, next_refresh_at, credential_generation
                   FROM teslamate_legacy_tokens WHERE singleton_id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::TeslaMateTokenStore)?;
        let receipt: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT transport, operation, outcome
                   FROM outbound_request_receipts WHERE id = ?1",
                params![receipt_id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::LegacyRefreshReceipt)?;
        let row = row
            .map(|(access, refresh, expires_at, next_refresh_at, generation)| {
                let generation = generation
                    .map(|value| Uuid::parse_str(&value))
                    .transpose()
                    .map_err(|_| StoreError::InvalidLegacyRefreshGeneration)?;
                TeslaMateLegacyTokenStore::new(
                    access,
                    refresh,
                    expires_at,
                    next_refresh_at,
                    generation,
                )
            })
            .transpose()?;
        Ok(LegacyPairSnapshot {
            row,
            receipt: receipt.map(|(transport, operation, outcome)| LegacyReceiptFacts {
                transport,
                operation,
                outcome,
            }),
        })
    }

    /// Test only: every token receipt, oldest first, as `(outcome, http_status,
    /// retry_after_seconds, has_binding)`.
    #[cfg(test)]
    pub(crate) fn legacy_token_receipts_for_test(
        &self,
    ) -> Result<Vec<(String, Option<i64>, Option<i64>, bool)>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT r.outcome, r.http_status, r.retry_after_seconds,
                        EXISTS(SELECT 1 FROM legacy_refresh_receipt_bindings AS b
                                WHERE b.receipt_id = r.id)
                   FROM outbound_request_receipts AS r
                  WHERE r.transport = 'legacy_auth' AND r.operation = 'token_refresh'
                  ORDER BY r.id",
            )
            .map_err(StoreError::LegacyRefreshReceipt)?;
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(StoreError::LegacyRefreshReceipt)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::LegacyRefreshReceipt)
    }

    /// Test only: the generations the input fences hold.
    #[cfg(test)]
    pub(crate) fn legacy_fenced_inputs_for_test(&self) -> Result<Vec<Uuid>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare("SELECT input_credential_generation FROM legacy_refresh_input_fences")
            .map_err(StoreError::LegacyRefreshReceipt)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(StoreError::LegacyRefreshReceipt)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::LegacyRefreshReceipt)?;
        rows.iter()
            .map(|value| {
                Uuid::parse_str(value).map_err(|_| StoreError::InvalidLegacyRefreshGeneration)
            })
            .collect()
    }

    pub fn has_unresolved_legacy_refresh(&self) -> Result<bool, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1
                      FROM outbound_request_receipts AS r
                      JOIN legacy_refresh_receipt_bindings AS b
                        ON b.receipt_id = r.id
                     WHERE r.transport = 'legacy_auth'
                       AND r.operation = 'token_refresh'
                       AND r.outcome = 'started'
                )",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LegacyRefreshReceipt)
    }

    /// Load the sole persisted TeslaMate token pair without decrypting it.
    pub fn load_teslamate_legacy_tokens(
        &self,
    ) -> Result<Option<TeslaMateLegacyTokenStore>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let row: Option<(Vec<u8>, Vec<u8>, i64, i64, Option<String>)> = connection
            .query_row(
                "SELECT access, refresh, expires_at, next_refresh_at, credential_generation
                 FROM teslamate_legacy_tokens WHERE singleton_id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::TeslaMateTokenStore)?;
        row.map(
            |(access, refresh, expires_at, next_refresh_at, generation)| {
                let generation = generation
                    .map(|value| Uuid::parse_str(&value))
                    .transpose()
                    .map_err(|_| StoreError::InvalidLegacyRefreshGeneration)?;
                TeslaMateLegacyTokenStore::new(
                    access,
                    refresh,
                    expires_at,
                    next_refresh_at,
                    generation,
                )
            },
        )
        .transpose()
    }
}

/// The pair row and one receipt as one snapshot saw them (see
/// `HubStore::legacy_pair_and_receipt_snapshot`).
pub(crate) struct LegacyPairSnapshot {
    pub(crate) row: Option<TeslaMateLegacyTokenStore>,
    pub(crate) receipt: Option<LegacyReceiptFacts>,
}

/// What a receipt says, without its timing: its transport, operation and outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LegacyReceiptFacts {
    transport: String,
    operation: String,
    outcome: String,
}

impl LegacyReceiptFacts {
    /// The attempt is still open: the token may have been consumed and nothing has
    /// closed it.
    pub(crate) fn is_started(&self) -> bool {
        self.outcome == "started"
    }

    /// A completed Legacy token refresh that succeeded: the closing the atomic
    /// write of a successor makes.
    pub(crate) fn is_token_refresh_success(&self) -> bool {
        self.transport == "legacy_auth"
            && self.operation == "token_refresh"
            && self.outcome == "success"
    }
}
