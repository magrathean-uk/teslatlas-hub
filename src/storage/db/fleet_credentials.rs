// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    pub fn replace_fleet_tokens(&self, tokens: &FleetTokenStore) -> Result<(), StoreError> {
        self.replace_fleet_tokens_inner(tokens, false)
    }

    /// Replace Fleet ciphertext and remove the superseded bytes from the live
    /// SQLite database and its local WAL. Used only by the one-time key split.
    pub(crate) fn replace_fleet_tokens_and_scrub(
        &self,
        tokens: &FleetTokenStore,
    ) -> Result<(), StoreError> {
        self.replace_fleet_tokens_inner(tokens, true)
    }

    fn replace_fleet_tokens_inner(
        &self,
        tokens: &FleetTokenStore,
        scrub_superseded: bool,
    ) -> Result<(), StoreError> {
        let replacement_generation = tokens
            .credential_generation()
            .map(|value| value.to_string());
        let mut connection = self.open()?;
        if scrub_superseded {
            connection
                .execute_batch("PRAGMA secure_delete = ON;")
                .map_err(StoreError::FleetTokenStore)?;
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if let Some(generation) = replacement_generation.as_ref() {
            let consumed: bool = transaction
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM fleet_refresh_input_fences
                         WHERE input_credential_generation = ?1
                    )",
                    params![generation],
                    |row| row.get(0),
                )
                .map_err(StoreError::FleetRefreshReceipt)?;
            if consumed {
                return Err(StoreError::FleetRefreshOutcomeUnknown);
            }
        }
        let unresolved_inputs = transaction
            .prepare(
                "SELECT b.input_credential_generation
                   FROM fleet_refresh_receipt_bindings AS b
                   JOIN outbound_request_receipts AS r ON r.id = b.receipt_id
                  WHERE r.outcome = 'started'",
            )
            .map_err(StoreError::FleetRefreshReceipt)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(StoreError::FleetRefreshReceipt)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::FleetRefreshReceipt)?;
        if !unresolved_inputs.is_empty() {
            let current_generation: Option<String> = transaction
                .query_row(
                    "SELECT credential_generation FROM fleet_tokens
                      WHERE singleton_id = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::FleetTokenStore)?
                .flatten();
            if unresolved_inputs.len() != 1
                || current_generation.as_ref() != unresolved_inputs.first()
                || replacement_generation.is_none()
                || replacement_generation.as_ref() == unresolved_inputs.first()
            {
                return Err(StoreError::FleetRefreshOutcomeUnknown);
            }
            let completed_at_ms = outbound_request_clock_ms()?;
            transaction
                .execute(
                    "UPDATE outbound_request_receipts
                        SET completed_at_ms = MAX(?1, started_at_ms),
                            duration_ms = MAX(?1, started_at_ms) - started_at_ms,
                            outcome = 'cancelled'
                      WHERE outcome = 'started' AND id IN (
                        SELECT receipt_id FROM fleet_refresh_receipt_bindings)",
                    params![completed_at_ms],
                )
                .map_err(StoreError::FleetRefreshReceipt)?;
        }
        transaction
            .execute(
                "INSERT INTO fleet_tokens(
                    singleton_id, access, refresh, client_id, region,
                    expires_at, next_refresh_at, credential_generation
                 ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(singleton_id) DO UPDATE SET
                    access = excluded.access,
                    refresh = excluded.refresh,
                    client_id = excluded.client_id,
                    region = excluded.region,
                    expires_at = excluded.expires_at,
                    next_refresh_at = excluded.next_refresh_at,
                    credential_generation = excluded.credential_generation",
                params![
                    tokens.access(),
                    tokens.refresh(),
                    tokens.client_id(),
                    tokens.region(),
                    tokens.expires_at(),
                    tokens.next_refresh_at(),
                    replacement_generation,
                ],
            )
            .map_err(StoreError::FleetTokenStore)?;
        transaction.commit().map_err(StoreError::FleetTokenStore)?;
        if scrub_superseded {
            let (busy, remaining, checkpointed): (i64, i64, i64) = connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(StoreError::FleetTokenStore)?;
            if busy != 0 || remaining != checkpointed {
                return Err(StoreError::FleetCredentialScrubIncomplete);
            }
        }
        Ok(())
    }

    pub fn load_fleet_tokens(&self) -> Result<Option<FleetTokenStore>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let row = connection
            .query_row(
                "SELECT access, refresh, client_id, region, expires_at, next_refresh_at,
                        credential_generation
                   FROM fleet_tokens WHERE singleton_id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::FleetTokenStore)?;
        row.map(
            |(access, refresh, client_id, region, expires_at, next_refresh_at, generation)| {
                let generation = generation
                    .map(|value| Uuid::parse_str(&value))
                    .transpose()
                    .map_err(|_| StoreError::InvalidFleetTokenStore)?;
                FleetTokenStore::new(
                    access,
                    refresh,
                    client_id,
                    region,
                    expires_at,
                    next_refresh_at,
                    generation,
                )
            },
        )
        .transpose()
    }

    pub(crate) fn bind_fleet_credential_generation(
        &self,
        tokens: &FleetTokenStore,
        generation: Uuid,
    ) -> Result<(), StoreError> {
        if generation.is_nil() {
            return Err(StoreError::InvalidFleetRefreshGeneration);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let row: Option<(Vec<u8>, Vec<u8>, Option<String>)> = transaction
            .query_row(
                "SELECT access, refresh, credential_generation
                   FROM fleet_tokens WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::FleetTokenStore)?;
        let (access, refresh, stored_generation) =
            row.ok_or(StoreError::FleetRefreshOutcomeUnknown)?;
        if access != tokens.access() || refresh != tokens.refresh() {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        match stored_generation {
            Some(stored) if stored == generation.to_string() => {}
            Some(_) => return Err(StoreError::FleetRefreshOutcomeUnknown),
            None => {
                let unresolved: bool = transaction
                    .query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM outbound_request_receipts AS r
                            JOIN fleet_refresh_receipt_bindings AS b
                              ON b.receipt_id = r.id
                            WHERE r.outcome = 'started')",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::FleetRefreshReceipt)?;
                if unresolved {
                    return Err(StoreError::FleetRefreshOutcomeUnknown);
                }
                let updated = transaction
                    .execute(
                        "UPDATE fleet_tokens SET credential_generation = ?1
                          WHERE singleton_id = 1 AND credential_generation IS NULL
                            AND access = ?2 AND refresh = ?3",
                        params![generation.to_string(), tokens.access(), tokens.refresh()],
                    )
                    .map_err(StoreError::FleetTokenStore)?;
                if updated != 1 {
                    return Err(StoreError::FleetRefreshOutcomeUnknown);
                }
            }
        }
        transaction.commit().map_err(StoreError::FleetTokenStore)
    }

    pub(crate) fn begin_fleet_refresh(
        &self,
        input_generation: Uuid,
    ) -> Result<OutboundRequestReceiptId, StoreError> {
        if input_generation.is_nil() {
            return Err(StoreError::InvalidFleetRefreshGeneration);
        }
        let started_at_ms = outbound_request_clock_ms()?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current: Option<String> = transaction
            .query_row(
                "SELECT credential_generation FROM fleet_tokens WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::FleetTokenStore)?
            .flatten();
        if current.as_deref() != Some(input_generation.to_string().as_str()) {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        let unavailable: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM outbound_request_receipts AS r
                    JOIN fleet_refresh_receipt_bindings AS b ON b.receipt_id = r.id
                    WHERE r.outcome = 'started'
                 ) OR EXISTS(
                    SELECT 1 FROM fleet_refresh_input_fences
                    WHERE input_credential_generation = ?1
                 )",
                params![input_generation.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        if unavailable {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        ensure_outbound_request_capacity(&transaction)?;
        let fence_count: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM fleet_refresh_input_fences",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        if fence_count >= MAX_LEGACY_REFRESH_INPUT_FENCES {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        transaction
            .execute(
                "INSERT INTO outbound_request_receipts(
                    correlation_id, started_at_ms, vehicle_tesla_id, transport,
                    operation, safety_class, precondition, outcome
                 ) VALUES (?1, ?2, NULL, 'fleet_api', 'token_refresh',
                           'non_wake_endpoint', 'not_required', 'started')",
                params![Uuid::new_v4().to_string(), started_at_ms],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        let receipt_id = OutboundRequestReceiptId(transaction.last_insert_rowid());
        transaction
            .execute(
                "INSERT INTO fleet_refresh_input_fences(input_credential_generation)
                 VALUES (?1)",
                params![input_generation.to_string()],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        transaction
            .execute(
                "INSERT INTO fleet_refresh_receipt_bindings(
                    receipt_id, attempt_id, input_credential_generation
                 ) VALUES (?1, ?2, ?3)",
                params![
                    receipt_id.0,
                    Uuid::new_v4().to_string(),
                    input_generation.to_string()
                ],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        transaction
            .commit()
            .map_err(StoreError::FleetRefreshReceipt)?;
        Ok(receipt_id)
    }

    pub(crate) fn cancel_unsent_fleet_refresh(
        &self,
        receipt_id: OutboundRequestReceiptId,
        input_generation: Uuid,
    ) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let deleted = transaction
            .execute(
                "DELETE FROM fleet_refresh_receipt_bindings
                  WHERE receipt_id = ?1 AND input_credential_generation = ?2
                    AND EXISTS(SELECT 1 FROM outbound_request_receipts
                               WHERE id = ?1 AND outcome = 'started')",
                params![receipt_id.0, input_generation.to_string()],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        if deleted != 1 {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        transaction
            .execute(
                "DELETE FROM fleet_refresh_input_fences
                  WHERE input_credential_generation = ?1",
                params![input_generation.to_string()],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        let completed_at_ms = outbound_request_clock_ms()?;
        let completed = transaction
            .execute(
                "UPDATE outbound_request_receipts
                    SET completed_at_ms = MAX(?2, started_at_ms),
                        duration_ms = MAX(?2, started_at_ms) - started_at_ms,
                        outcome = 'cancelled'
                  WHERE id = ?1 AND outcome = 'started'",
                params![receipt_id.0, completed_at_ms],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        if completed != 1 {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        transaction
            .commit()
            .map_err(StoreError::FleetRefreshReceipt)
    }

    /// Terminalize a refresh attempt whose provider response proves the input
    /// refresh token was not consumed. The generation may then be retried.
    pub(crate) fn complete_retryable_fleet_refresh_failure(
        &self,
        receipt_id: OutboundRequestReceiptId,
        input_generation: Uuid,
        completion: &OutboundRequestCompletion,
    ) -> Result<(), StoreError> {
        completion.validate()?;
        if receipt_id.0 <= 0
            || input_generation.is_nil()
            || !matches!(
                completion.outcome,
                OutboundRequestOutcome::HttpError
                    | OutboundRequestOutcome::TransportError
                    | OutboundRequestOutcome::AuthenticationRejected
            )
        {
            return Err(StoreError::InvalidFleetRefreshFailure);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let stored_input: Option<String> = transaction
            .query_row(
                "SELECT b.input_credential_generation
                   FROM fleet_refresh_receipt_bindings AS b
                   JOIN outbound_request_receipts AS r ON r.id = b.receipt_id
                  WHERE b.receipt_id = ?1 AND r.outcome = 'started'",
                params![receipt_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::FleetRefreshReceipt)?;
        let current: Option<String> = transaction
            .query_row(
                "SELECT credential_generation FROM fleet_tokens WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::FleetTokenStore)?
            .flatten();
        let input = input_generation.to_string();
        if stored_input.as_deref() != Some(input.as_str())
            || current.as_deref() != Some(input.as_str())
        {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        let deleted = transaction
            .execute(
                "DELETE FROM fleet_refresh_input_fences
                  WHERE input_credential_generation = ?1",
                params![input],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        if deleted != 1 {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        let completed_at_ms = outbound_request_clock_ms()?.max(
            transaction
                .query_row(
                    "SELECT started_at_ms FROM outbound_request_receipts WHERE id = ?1",
                    params![receipt_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(StoreError::FleetRefreshReceipt)?,
        );
        let completed = transaction
            .execute(
                "UPDATE outbound_request_receipts
                    SET completed_at_ms = ?2, duration_ms = ?2 - started_at_ms,
                        outcome = ?3, http_status = ?4, retry_after_seconds = ?5
                  WHERE id = ?1 AND outcome = 'started'",
                params![
                    receipt_id.0,
                    completed_at_ms,
                    completion.outcome.as_str(),
                    completion.http_status,
                    completion
                        .retry_after_seconds
                        .map(i64::try_from)
                        .transpose()
                        .map_err(|_| StoreError::InvalidOutboundRequestRetryAfter)?
                ],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        if completed != 1 {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        prune_expired_outbound_request_receipts(&transaction)?;
        transaction
            .commit()
            .map_err(StoreError::FleetRefreshReceipt)
    }

    pub(crate) fn complete_fleet_refresh(
        &self,
        receipt_id: OutboundRequestReceiptId,
        input_generation: Uuid,
        output_generation: Uuid,
        tokens: &FleetTokenStore,
    ) -> Result<(), StoreError> {
        if receipt_id.0 <= 0
            || input_generation.is_nil()
            || output_generation.is_nil()
            || input_generation == output_generation
            || tokens.credential_generation() != Some(output_generation)
        {
            return Err(StoreError::InvalidFleetRefreshGeneration);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let stored_input: String = transaction
            .query_row(
                "SELECT b.input_credential_generation
                   FROM fleet_refresh_receipt_bindings AS b
                   JOIN outbound_request_receipts AS r ON r.id = b.receipt_id
                  WHERE b.receipt_id = ?1 AND r.outcome = 'started'",
                params![receipt_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::FleetRefreshReceipt)?
            .ok_or(StoreError::FleetRefreshOutcomeUnknown)?;
        let current: Option<String> = transaction
            .query_row(
                "SELECT credential_generation FROM fleet_tokens WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::FleetTokenStore)?
            .flatten();
        if stored_input != input_generation.to_string()
            || current.as_deref() != Some(input_generation.to_string().as_str())
        {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        let output_consumed: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM fleet_refresh_input_fences
                               WHERE input_credential_generation = ?1)",
                params![output_generation.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        if output_consumed {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        let updated = transaction
            .execute(
                "UPDATE fleet_tokens SET access = ?1, refresh = ?2, client_id = ?3,
                        region = ?4, expires_at = ?5, next_refresh_at = ?6,
                        credential_generation = ?7
                  WHERE singleton_id = 1 AND credential_generation = ?8",
                params![
                    tokens.access(),
                    tokens.refresh(),
                    tokens.client_id(),
                    tokens.region(),
                    tokens.expires_at(),
                    tokens.next_refresh_at(),
                    output_generation.to_string(),
                    input_generation.to_string(),
                ],
            )
            .map_err(StoreError::FleetTokenStore)?;
        if updated != 1 {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        let bound = transaction
            .execute(
                "UPDATE fleet_refresh_receipt_bindings
                    SET output_credential_generation = ?2
                  WHERE receipt_id = ?1 AND output_credential_generation IS NULL",
                params![receipt_id.0, output_generation.to_string()],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        if bound != 1 {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        let completed_at_ms = outbound_request_clock_ms()?.max(
            transaction
                .query_row(
                    "SELECT started_at_ms FROM outbound_request_receipts WHERE id = ?1",
                    params![receipt_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(StoreError::FleetRefreshReceipt)?,
        );
        let completed = transaction
            .execute(
                "UPDATE outbound_request_receipts
                    SET completed_at_ms = ?2, duration_ms = ?2 - started_at_ms,
                        outcome = 'success', http_status = 200
                  WHERE id = ?1 AND outcome = 'started'",
                params![receipt_id.0, completed_at_ms],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        if completed != 1 {
            return Err(StoreError::FleetRefreshOutcomeUnknown);
        }
        transaction
            .commit()
            .map_err(StoreError::FleetRefreshReceipt)
    }

    pub fn has_unresolved_fleet_refresh(&self) -> Result<bool, StoreError> {
        self.open_read_only_connection()?
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM outbound_request_receipts AS r
                    JOIN fleet_refresh_receipt_bindings AS b ON b.receipt_id = r.id
                    WHERE r.outcome = 'started')",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::FleetRefreshReceipt)
    }

    pub fn clear_fleet_tokens(&self) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let completed_at_ms = outbound_request_clock_ms()?;
        transaction
            .execute(
                "UPDATE outbound_request_receipts
                    SET completed_at_ms = MAX(?1, started_at_ms),
                        duration_ms = MAX(?1, started_at_ms) - started_at_ms,
                        outcome = 'cancelled'
                  WHERE outcome = 'started' AND id IN (
                    SELECT receipt_id FROM fleet_refresh_receipt_bindings)",
                params![completed_at_ms],
            )
            .map_err(StoreError::FleetRefreshReceipt)?;
        transaction
            .execute("DELETE FROM fleet_tokens WHERE singleton_id = 1", [])
            .map_err(StoreError::FleetTokenStore)?;
        transaction.commit().map_err(StoreError::FleetTokenStore)
    }

    /// Delete the sole persisted TeslaMate token pair.
    pub fn clear_teslamate_legacy_tokens(&self) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
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
        transaction
            .execute(
                "DELETE FROM teslamate_legacy_tokens WHERE singleton_id = 1",
                [],
            )
            .map_err(StoreError::TeslaMateTokenStore)?;
        transaction
            .commit()
            .map_err(StoreError::TeslaMateTokenStore)
    }
}
