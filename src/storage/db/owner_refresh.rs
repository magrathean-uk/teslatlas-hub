// SPDX-License-Identifier: AGPL-3.0-only

// M09-T16: what the durable credential facts say about the stored Owner API
// login, for the status projection (B5) and the production start admission
// (B4). Read-only, from one snapshot; no clock is compared and no secret,
// fingerprint or generation leaves this file.

/// How far refreshing the stored Owner API login has gone (B5), derived only
/// from the attempt fence, the successor generation and the auth stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerRefreshProgress {
    /// No refresh was sent for this login since it was stored.
    NoneSent,
    /// A send for the stored generation is recorded and still unresolved.
    AttemptRecorded,
    /// The stored generation is a successor the Hub stored from a refresh.
    Completed,
    /// Refreshing has stopped: rejected, or sent with an unknown outcome.
    SignInRequired,
}

impl OwnerRefreshProgress {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoneSent => "none_sent",
            Self::AttemptRecorded => "attempt_recorded",
            Self::Completed => "completed",
            Self::SignInRequired => "sign_in_required",
        }
    }
}

/// The Owner API generations the stored one descends from by completed
/// refreshes, the stored one included (`UNION` ends any cycle).
const OWNER_LINEAGE_SQL: &str = "WITH RECURSIVE lineage(generation) AS (
        SELECT ?1
        UNION
        SELECT a.input_generation FROM credential_refresh_attempts a
          JOIN outbound_request_receipts r ON r.id = a.receipt_id
          JOIN lineage l ON a.output_generation = l.generation
         WHERE a.account_id = 1 AND a.slot = 'owner_api' AND r.outcome = 'success')";

impl HubStore {
    /// B5 (M09-T16): the refresh progress of the stored Owner API login, or
    /// `None` when no login is stored. Precedence: a stop, then an unresolved
    /// send, then a completed successor, then nothing sent.
    pub(crate) fn owner_refresh_progress(&self) -> Result<Option<OwnerRefreshProgress>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let version: i32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(StoreError::CredentialPairStore)?;
        if version < CREDENTIAL_FORMAT_SCHEMA_VERSION {
            return Ok(None);
        }
        let transaction = connection.unchecked_transaction().map_err(StoreError::CredentialPairStore)?;
        let generation: Option<String> = transaction
            .query_row(
                "SELECT generation FROM credential_pairs WHERE account_id = 1 AND slot = 'owner_api'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::CredentialPairStore)?;
        let Some(generation) = generation else { return Ok(None) };
        let fact = |sql: &str| -> Result<bool, StoreError> {
            transaction
                .query_row(sql, [&generation], |row| row.get(0))
                .map_err(StoreError::CredentialRefreshReceipt)
        };
        // The auth stop names this pair, or a send for it closed without a
        // successor while its token stays fenced (a released send is deleted).
        let stopped = fact(
            "SELECT EXISTS(SELECT 1 FROM collector_auth_state WHERE singleton_id = 1
                AND state IN ('sign_in_required', 'signed_out')
                AND stopped_generation = ?1 COLLATE NOCASE)",
        )? || fact(
            "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts a
                JOIN outbound_request_receipts r ON r.id = a.receipt_id
               WHERE a.account_id = 1 AND a.slot = 'owner_api' AND a.input_generation = ?1
                 AND a.fence_id IS NOT NULL
                 AND r.outcome NOT IN ('started', 'success', 'cancelled'))",
        )?;
        if stopped {
            return Ok(Some(OwnerRefreshProgress::SignInRequired));
        }
        if fact(
            "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts a
                JOIN outbound_request_receipts r ON r.id = a.receipt_id
               WHERE a.account_id = 1 AND a.slot = 'owner_api' AND a.input_generation = ?1
                 AND r.outcome = 'started')",
        )? {
            return Ok(Some(OwnerRefreshProgress::AttemptRecorded));
        }
        if fact(
            "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts a
                JOIN outbound_request_receipts r ON r.id = a.receipt_id
               WHERE a.account_id = 1 AND a.slot = 'owner_api' AND a.output_generation = ?1
                 AND r.outcome = 'success')",
        )? {
            return Ok(Some(OwnerRefreshProgress::Completed));
        }
        Ok(Some(OwnerRefreshProgress::NoneSent))
    }

    /// B4 (M09-T16): whether a stored `signed_out` stop still holds the
    /// stored Owner API pair at start. It does not when the pair is a proven
    /// replacement: a stored generation that is neither the stopped one nor
    /// descended from it by a completed refresh (so an owner sign-in or a
    /// take-over wrote it after the stop), with no refresh unresolved. Any
    /// other `signed_out` row, one without a stopped generation included,
    /// still holds. Nothing is written: the unit clears the stale stop itself
    /// when it starts (R44a), and no other refusal is touched.
    pub(crate) fn owner_sign_out_stop_holds(&self) -> Result<bool, StoreError> {
        let connection = self.open_read_only_connection()?;
        let transaction = connection.unchecked_transaction().map_err(StoreError::CredentialPairStore)?;
        let row: Option<(String, Option<String>)> = transaction
            .query_row(
                "SELECT state, stopped_generation FROM collector_auth_state WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((state, stopped)) = row else { return Ok(false) };
        if state != "signed_out" {
            return Ok(false);
        }
        let Some(stopped) = stopped else { return Ok(true) };
        let generation: Option<String> = transaction
            .query_row(
                "SELECT generation FROM credential_pairs WHERE account_id = 1 AND slot = 'owner_api'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::CredentialPairStore)?;
        let Some(generation) = generation else { return Ok(true) };
        let unresolved: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts a
                    JOIN outbound_request_receipts r ON r.id = a.receipt_id
                   WHERE a.account_id = 1 AND a.slot = 'owner_api' AND r.outcome = 'started')",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::CredentialRefreshReceipt)?;
        if unresolved {
            return Ok(true);
        }
        transaction
            .query_row(
                &format!("{OWNER_LINEAGE_SQL} SELECT EXISTS(SELECT 1 FROM lineage WHERE generation = ?2 COLLATE NOCASE)"),
                params![generation, stopped],
                |row| row.get(0),
            )
            .map_err(StoreError::CredentialRefreshReceipt)
    }
}
