// SPDX-License-Identifier: AGPL-3.0-only

// Collection readiness (C3 R13, R66; C5 R52; B2 clarification 3): one registry
// and one reader for the three things a status, a doctor report and `/readyz`
// can say about collection.
//
// - Service failures are the six `ReadinessReasonCode` words of the supported
//   HTTP profile. They are the only thing that makes a Hub not ready. Order:
//   catalogue, lifecycle quarantine, published content, a required lease that is
//   absent, malformed, expired or terminal, then an applicable effective
//   credential status of `signed_out`.
// - Collection attention is at most one `CollectionAttentionCode`: a condition
//   the owner can resolve while the service stays up. It is read independently of
//   the lease and never changes service readiness.
// - Healthy detail is the established list of states worth showing. It never
//   repeats the attention.
//
// A disabled collector (`collector_required` false) has no attention, no detail
// and no lease demand. Everything here only reads: it takes no lease, builds no
// credential manager, sends no request and writes no row.

/// The one condition that needs the owner, shown beside a healthy service
/// (B2 clarification 3). The words are fixed; the Mac app labels them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionAttentionCode {
    /// The Owner API itself refused collection (C3 R13, R48).
    CollectorApiRefused,
    /// A configured provider refused collection (C3 R13, R49).
    CollectorProviderRefused,
    /// The Fleet API refused the application's access (C3 R13, R50).
    CollectorFleetAccessRefused,
    /// Tesla suspended the account's billing (C3 R13, R50).
    CollectorBillingSuspended,
    /// The local monthly request cap stopped Fleet polling, a zero cap included
    /// (C3 R13, R27).
    CollectorCapReached,
    /// A Fleet account configuration fault stops collection (C3 R13, R50).
    CollectorConfigError,
    /// Tesla locked the account (C3 R13, R50).
    CollectorAccountLocked,
    /// A stream run ended on an audit failure (C3 R13, R28).
    CollectorStreamAuditFailed,
    /// The credential unit waits and collects nothing for now: a token retry past
    /// the token's usability, a paused confirmation, an observer without a usable
    /// token, or a host clock before 2020 (C5 R52).
    CollectorAuthWaiting,
    /// The credential unit's own configuration is refused before any request: a
    /// region conflict, a missing region, an unpinned issuer or a bad token URL
    /// (C5 R5, R7, R52). Distinct from `CollectorConfigError`, which is Fleet
    /// account configuration.
    CollectorAuthConfigurationError,
    /// Sign in again; collection continues while the access token is usable
    /// (C5 R45, R52).
    CollectorAuthSignInRequired,
    /// The collector cadence is invalid and the managed Serve waits, collecting
    /// nothing, until the setting is corrected and the service restarted (C3 R21).
    CollectorCadenceInvalid,
    /// An identified Fleet token refresh whose outcome is unknown stops the
    /// account until the owner reconnects Fleet credentials (C5 R47, C3 R47).
    CollectorFleetRefreshOutcomeUnknown,
}

impl CollectionAttentionCode {
    /// Every attention word, in registry order.
    pub const ALL: [Self; 13] = [
        Self::CollectorApiRefused,
        Self::CollectorProviderRefused,
        Self::CollectorFleetAccessRefused,
        Self::CollectorBillingSuspended,
        Self::CollectorCapReached,
        Self::CollectorConfigError,
        Self::CollectorAccountLocked,
        Self::CollectorStreamAuditFailed,
        Self::CollectorAuthWaiting,
        Self::CollectorAuthConfigurationError,
        Self::CollectorAuthSignInRequired,
        Self::CollectorCadenceInvalid,
        Self::CollectorFleetRefreshOutcomeUnknown,
    ];

    /// The wire word, equal to the serialized value.
    pub const fn code(self) -> &'static str {
        match self {
            Self::CollectorApiRefused => "collector_api_refused",
            Self::CollectorProviderRefused => "collector_provider_refused",
            Self::CollectorFleetAccessRefused => "collector_fleet_access_refused",
            Self::CollectorBillingSuspended => "collector_billing_suspended",
            Self::CollectorCapReached => "collector_cap_reached",
            Self::CollectorConfigError => "collector_config_error",
            Self::CollectorAccountLocked => "collector_account_locked",
            Self::CollectorStreamAuditFailed => "collector_stream_audit_failed",
            Self::CollectorAuthWaiting => "collector_auth_waiting",
            Self::CollectorAuthConfigurationError => "collector_auth_configuration_error",
            Self::CollectorAuthSignInRequired => "collector_auth_sign_in_required",
            Self::CollectorCadenceInvalid => "collector_cadence_invalid",
            Self::CollectorFleetRefreshOutcomeUnknown => {
                "collector_fleet_refresh_outcome_unknown"
            }
        }
    }
}

/// The healthy-detail words, in the order they are reported (C3 R13, R66). None
/// of them is attention and none is a service reason.
pub const COLLECTOR_DETAIL_CODES: [&str; 5] = [
    "collector_cap_warning",
    "collector_rate_limited",
    "collector_vehicle_paused",
    "collector_tesla_unreachable",
    "collector_gap_unnotified",
];

/// The attention and healthy detail a test reads from the collector tables.
#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CollectorReadiness {
    pub failure: Option<CollectionAttentionCode>,
    pub detail: Vec<&'static str>,
}

/// What a reader says about the credential unit's stored status without a loaded
/// configuration (an explicitly context-free store read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CollectorAuthReadiness {
    /// The status is attention (C5 R52).
    Attention(CollectionAttentionCode),
    /// The status is, or has become, `signed_out`: a service failure.
    SignedOut,
}

/// The configuration-aware context of one readiness read (B2 clarification 3).
/// A production reader supplies the loaded collector mode so that C5 applies
/// exactly to Legacy and Provider, whatever the account row records; a
/// context-free read leaves `mode` empty and falls back to the stored account
/// mode (`fleet_polling` ignores C5, Legacy and Provider consult it, and an
/// absent account row consults it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessContext {
    /// The surface's existing collector-required flag.
    pub collector_required: bool,
    /// The evaluation time, epoch milliseconds.
    pub now_ms: i64,
    /// The loaded collector mode, or `None` for a context-free read.
    pub mode: Option<crate::config::CollectorMode>,
    /// The active cadence violation of the loaded configuration, if any. Callers
    /// pass `None` for a disabled collector and for Edge.
    pub cadence: Option<crate::config::CadenceViolation>,
}

impl ReadinessContext {
    /// An explicitly context-free read: stored-mode fallback, no cadence.
    pub fn context_free(collector_required: bool, now_ms: i64) -> Self {
        Self {
            collector_required,
            now_ms,
            mode: None,
            cadence: None,
        }
    }

    /// A read under the loaded configuration.
    pub fn configured(
        collector_required: bool,
        now_ms: i64,
        mode: crate::config::CollectorMode,
        cadence: Option<crate::config::CadenceViolation>,
    ) -> Self {
        Self {
            collector_required,
            now_ms,
            mode: Some(mode),
            cadence,
        }
    }
}

/// The three tiers and the running witness of one readiness read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectorReadinessView {
    /// Service readiness: the only tier that fails `/readyz`.
    pub service: Result<(), ReadinessFailure>,
    /// The single attention, independent of the lease and of `service`.
    pub attention: Option<CollectionAttentionCode>,
    /// Healthy detail, never repeating `attention`.
    pub detail: Vec<&'static str>,
    /// A required collector holds a coherent, unexpired lease. A terminal lease
    /// still witnesses a running process. Timestamps never prove an exact
    /// physical stop time.
    pub running: bool,
    /// The earliest epoch millisecond from which `service` may change although
    /// no row does: a usable `sign_in_required` becomes `signed_out` at its known
    /// usability bound. A reader that caches `service` must not serve it from
    /// then on.
    pub service_valid_until_ms: Option<i64>,
}

/// The lease row as stored.
struct LeaseRow {
    instance_id: String,
    state: String,
    started_at_ms: i64,
    heartbeat_at_ms: i64,
    lease_until_ms: i64,
}

impl LeaseRow {
    fn is_coherent(&self) -> bool {
        Uuid::parse_str(&self.instance_id)
            .ok()
            .is_some_and(|value| !value.is_nil())
            && self.started_at_ms >= 0
            && self.heartbeat_at_ms >= self.started_at_ms
            && self.lease_until_ms > self.heartbeat_at_ms
            && matches!(self.state.as_str(), "active" | "auth_terminal")
    }
}

/// Everything the attention and detail tiers read, in one pass.
struct CollectionFacts {
    /// `collector_account_state.state`, if the row exists.
    account_state: Option<String>,
    audit_failed: bool,
    detail: Vec<&'static str>,
    /// The applicable credential status, effective for the loaded mode, or the
    /// stored one for a context-free read. `None` when C5 does not apply or the
    /// unit has never written a status.
    auth: Option<CollectorAuthRow>,
    /// The epoch second a usable `sign_in_required` stops being usable.
    auth_usable_until: Option<i64>,
    fleet_refresh_unknown: bool,
}

fn service_failure(code: ReadinessReasonCode) -> ReadinessFailure {
    ReadinessFailure { code }
}

/// The attention a credential status gives (C5 R52), or `None` when the state
/// needs none: `ready`, `refreshing`, `api_refused` (C3 raises its own refusal
/// code from its own row, so one condition raises one code), and `signed_out`,
/// which is a service failure.
pub(crate) fn auth_attention(row: &CollectorAuthRow) -> Option<CollectionAttentionCode> {
    match row.state.as_str() {
        "configuration_error" => Some(CollectionAttentionCode::CollectorAuthConfigurationError),
        "sign_in_required" => Some(CollectionAttentionCode::CollectorAuthSignInRequired),
        "awaiting_confirmation" | "observer_waiting" | "clock_invalid" => {
            Some(CollectionAttentionCode::CollectorAuthWaiting)
        }
        "retrying" | "persistence_pending" if !row.collecting => {
            Some(CollectionAttentionCode::CollectorAuthWaiting)
        }
        _ => None,
    }
}

/// The single attention, by the fixed precedence of B2 clarification 3: an
/// active cadence fault; a C3 Owner or provider refusal; an applicable C5
/// attention; an identified Fleet unknown refresh; another C3 account stop; a
/// stream audit failure. A Fleet unknown refresh therefore outranks cap, billing
/// and Fleet access refusal, and follows cadence and Owner or provider refusal.
fn select_attention(
    cadence_invalid: bool,
    account_state: Option<&str>,
    auth: Option<CollectionAttentionCode>,
    fleet_refresh_unknown: bool,
    audit_failed: bool,
) -> Option<CollectionAttentionCode> {
    use CollectionAttentionCode as Code;
    if cadence_invalid {
        return Some(Code::CollectorCadenceInvalid);
    }
    match account_state {
        Some("owner_api_refused") => return Some(Code::CollectorApiRefused),
        Some("provider_refused") => return Some(Code::CollectorProviderRefused),
        _ => {}
    }
    if auth.is_some() {
        return auth;
    }
    if fleet_refresh_unknown {
        return Some(Code::CollectorFleetRefreshOutcomeUnknown);
    }
    match account_state {
        Some("fleet_access_refused") => return Some(Code::CollectorFleetAccessRefused),
        Some("billing_suspended") => return Some(Code::CollectorBillingSuspended),
        Some("cap_reached") => return Some(Code::CollectorCapReached),
        Some("config_error") => return Some(Code::CollectorConfigError),
        Some("account_locked") => return Some(Code::CollectorAccountLocked),
        _ => {}
    }
    audit_failed.then_some(Code::CollectorStreamAuditFailed)
}

impl HubStore {
    /// Service readiness without a loaded configuration (an explicitly
    /// context-free read): the stored account mode decides whether the credential
    /// status applies. Attention and detail are not part of it.
    pub fn service_readiness_at(
        &self,
        supervised_collector_required: bool,
        now_ms: i64,
    ) -> Result<(), ReadinessFailure> {
        self.collector_readiness_view(&ReadinessContext::context_free(
            supervised_collector_required,
            now_ms,
        ))
        .service
    }

    /// The healthy-detail reasons of a required collector (C3 R13, R66: healthy
    /// states worth showing, such as a paused car, a rate-limit wait or an
    /// unacknowledged gap). They never change `/readyz`'s status.
    pub fn collector_readiness_detail_at(
        &self,
        now_ms: i64,
    ) -> Result<Vec<&'static str>, StoreError> {
        self.collection_facts(&ReadinessContext::context_free(true, now_ms))
            .map(|facts| facts.detail)
    }

    /// The attention and healthy detail of a required, context-free collector
    /// read, as the collector tests state them.
    #[cfg(test)]
    pub(crate) fn collector_readiness_at(
        &self,
        now_ms: i64,
    ) -> Result<CollectorReadiness, StoreError> {
        let view = self.collector_readiness_view(&ReadinessContext::context_free(true, now_ms));
        if view.attention.is_none() && view.detail.is_empty() {
            // Distinguish an unreadable collector table from a quiet one.
            self.collection_facts(&ReadinessContext::context_free(true, now_ms))?;
        }
        Ok(CollectorReadiness {
            failure: view.attention,
            detail: view.detail,
        })
    }

    /// The stored credential status's readiness without a loaded configuration
    /// (C5 R52), or `None` when the state gives none.
    #[cfg(test)]
    pub(crate) fn collector_auth_readiness(
        &self,
    ) -> Result<Option<CollectorAuthReadiness>, StoreError> {
        Ok(self
            .load_collector_auth_row()?
            .map(crate::legacy_auth::effective::stored_auth_view)
            .and_then(|row| auth_readiness_of(&row)))
    }

    /// Read service readiness, attention, healthy detail and the running witness
    /// under one context (B2 clarification 3).
    ///
    /// Service failures keep the order catalogue, lease, then effective
    /// credential status. Attention and detail are read whether or not the lease
    /// exists, is expired or is terminal, but only for a required collector. A
    /// fault in an added read is a catalogue fault: it yields
    /// `catalogue_unavailable` whenever the base checks passed, whatever the
    /// lease says, and never propagates.
    pub fn collector_readiness_view(&self, context: &ReadinessContext) -> CollectorReadinessView {
        let base = self.base_service_readiness();
        if !context.collector_required {
            return CollectorReadinessView {
                service: base,
                attention: None,
                detail: Vec::new(),
                running: false,
                service_valid_until_ms: None,
            };
        }
        let lease = self.read_lease_row();
        let running = matches!(
            &lease,
            Ok(Some(row)) if row.is_coherent() && row.lease_until_ms > context.now_ms
        );
        let facts = self.collection_facts(context);
        let mut service = base;
        if service.is_ok() {
            // A fault in an added read is a catalogue fault, and the catalogue
            // precedes the lease: it is never hidden behind an absent, expired
            // or terminal lease, so a doctor that tolerates an absent lease
            // cannot pass a store whose collection facts cannot be read.
            service = match &facts {
                Err(_) => Err(service_failure(ReadinessReasonCode::CatalogueUnavailable)),
                Ok(facts) => self.lease_and_auth_service(context, &lease, facts),
            };
        }
        let (attention, detail, valid_until) = match facts {
            Ok(facts) => {
                let valid_until = facts
                    .auth
                    .as_ref()
                    .filter(|row| row.state == "sign_in_required")
                    .and(facts.auth_usable_until)
                    .map(|until| until.saturating_mul(1_000));
                let attention = select_attention(
                    context.cadence.is_some(),
                    facts.account_state.as_deref(),
                    facts.auth.as_ref().and_then(auth_attention),
                    facts.fleet_refresh_unknown,
                    facts.audit_failed,
                );
                (attention, facts.detail, valid_until)
            }
            Err(_) => (None, Vec::new(), None),
        };
        CollectorReadinessView {
            service,
            attention,
            detail,
            running,
            service_valid_until_ms: valid_until,
        }
    }

    /// The lease phase of the service check, then the applicable effective
    /// `signed_out` (C5 R52, R43): absent, malformed, expired, terminal, signed
    /// out. The caller has already turned a fault in the added facts read into
    /// `catalogue_unavailable`, ahead of this phase.
    fn lease_and_auth_service(
        &self,
        context: &ReadinessContext,
        lease: &Result<Option<LeaseRow>, ()>,
        facts: &CollectionFacts,
    ) -> Result<(), ReadinessFailure> {
        if context.now_ms < 0 {
            return Err(service_failure(ReadinessReasonCode::CatalogueUnavailable));
        }
        let row = match lease {
            Err(()) => {
                return Err(service_failure(ReadinessReasonCode::CatalogueUnavailable));
            }
            Ok(None) => return Err(service_failure(ReadinessReasonCode::CollectorAbsent)),
            Ok(Some(row)) => row,
        };
        if !row.is_coherent() {
            return Err(service_failure(ReadinessReasonCode::CatalogueUnavailable));
        }
        if row.lease_until_ms <= context.now_ms {
            return Err(service_failure(ReadinessReasonCode::CollectorStale));
        }
        if row.state == SupervisedCollectorState::AuthenticationTerminal.as_str() {
            return Err(service_failure(ReadinessReasonCode::CollectorAuthTerminal));
        }
        if facts
            .auth
            .as_ref()
            .and_then(auth_readiness_of)
            .is_some_and(|readiness| readiness == CollectorAuthReadiness::SignedOut)
        {
            return Err(service_failure(ReadinessReasonCode::CollectorAuthTerminal));
        }
        Ok(())
    }

    fn read_lease_row(&self) -> Result<Option<LeaseRow>, ()> {
        let connection = self.open_read_only_connection().map_err(|_| ())?;
        connection
            .query_row(
                "SELECT instance_id, state, started_at_ms,
                        heartbeat_at_ms, lease_until_ms
                   FROM supervised_collector_lease WHERE singleton_id = 1",
                [],
                |row| {
                    Ok(LeaseRow {
                        instance_id: row.get(0)?,
                        state: row.get(1)?,
                        started_at_ms: row.get(2)?,
                        heartbeat_at_ms: row.get(3)?,
                        lease_until_ms: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|_| ())
    }

    /// Whether the Fleet credential has an identified unresolved refresh: a
    /// started token receipt bound to the stored pair's generation and fenced
    /// (C5 R47). A receipt of another generation, or without its fence, is not
    /// that condition. Columns only: no key, no decryption.
    pub fn identified_unresolved_fleet_refresh(&self) -> Result<bool, StoreError> {
        self.open_read_only_connection()?
            .query_row(
                "SELECT EXISTS(
                    SELECT 1
                      FROM outbound_request_receipts AS r
                      JOIN fleet_refresh_receipt_bindings AS b ON b.receipt_id = r.id
                      JOIN fleet_tokens AS t
                        ON t.singleton_id = 1
                       AND t.credential_generation = b.input_credential_generation
                     WHERE r.outcome = 'started'
                       AND EXISTS(
                            SELECT 1 FROM fleet_refresh_input_fences AS f
                             WHERE f.input_credential_generation
                                   = b.input_credential_generation))",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::FleetRefreshReceipt)
    }

    /// The effective, read-only credential status the loaded `mode` applies to,
    /// or `None` when C5 does not apply or the unit has never written one. Used
    /// by status, doctor and the readiness view alike, so every surface shows the
    /// same state (B2 clarification 3).
    pub(crate) fn effective_collector_auth(
        &self,
        mode: crate::config::CollectorMode,
        now_ms: i64,
    ) -> Result<Option<crate::legacy_auth::effective::EffectiveAuth>, StoreError> {
        use crate::{config::CollectorMode, legacy_auth::state::StoppedMode};
        let stopped_mode = match mode {
            CollectorMode::Legacy => StoppedMode::Legacy,
            CollectorMode::Provider => StoppedMode::Provider,
            CollectorMode::FleetPolling => return Ok(None),
        };
        let data_dir = self
            .database_path()
            .parent()
            .ok_or(StoreError::EffectiveCollectorAuthUnavailable)?;
        crate::legacy_auth::effective::effective_auth_row(self, data_dir, stopped_mode, now_ms)
            .map_err(|error| match error {
                crate::legacy_auth::effective::EffectiveAuthError::Store(error) => error,
                _ => StoreError::EffectiveCollectorAuthUnavailable,
            })
    }

    fn collection_facts(&self, context: &ReadinessContext) -> Result<CollectionFacts, StoreError> {
        use crate::config::CollectorMode;
        let connection = self.open_read_only_connection()?;
        let account: Option<(String, String, String, Option<i64>)> = connection
            .query_row(
                "SELECT mode, state, reason_code, rate_limited_until_ms
                   FROM collector_account_state WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let mut detail = Vec::new();
        if let Some((_, state, reason, rate_limited_until)) = &account {
            if state == "collecting" && reason == "cap_warning" {
                detail.push(COLLECTOR_DETAIL_CODES[0]);
            }
            if rate_limited_until.is_some_and(|until| until > context.now_ms) {
                detail.push(COLLECTOR_DETAIL_CODES[1]);
            }
        }
        let count = |sql: &str| -> Result<i64, StoreError> {
            connection
                .query_row(sql, [], |row| row.get(0))
                .map_err(StoreError::Query)
        };
        // The in-process report comes first (C3 R28, section 8 item 6): a store
        // that refused a receipt would refuse the row that says so, so the
        // collector's own status is the source, and the stored reason code is
        // what other processes can see once a write succeeds.
        let audit_failed = self.stream_audit_failed_cars() > 0
            || count(
                "SELECT COUNT(*) FROM collector_vehicle_state
                  WHERE reason_code = 'stream_audit_failed'
                    AND hold NOT IN ('disabled', 'removed')",
            )? > 0;
        if count("SELECT COUNT(*) FROM collector_vehicle_state WHERE hold = 'paused'")? > 0 {
            detail.push(COLLECTOR_DETAIL_CODES[2]);
        }
        if count(
            "SELECT COUNT(*) FROM collection_gaps
              WHERE kind = 'tesla_unreachable' AND ended_at_ms IS NULL",
        )? > 0
        {
            detail.push(COLLECTOR_DETAIL_CODES[3]);
        }
        if count("SELECT COUNT(*) FROM collection_gaps WHERE alert = 1 AND notified_at_ms IS NULL")?
            > 0
        {
            detail.push(COLLECTOR_DETAIL_CODES[4]);
        }
        drop(connection);
        let stored_mode = account.as_ref().map(|(mode, ..)| mode.as_str());
        let fleet = match context.mode {
            Some(mode) => mode == CollectorMode::FleetPolling,
            None => stored_mode == Some("fleet_polling"),
        };
        let (auth, auth_usable_until) = match context.mode {
            Some(mode) => match self.effective_collector_auth(mode, context.now_ms)? {
                Some(effective) => (Some(effective.row), effective.usable_until),
                None => (None, None),
            },
            None if fleet => (None, None),
            None => (
                self.load_collector_auth_row()?
                    .map(crate::legacy_auth::effective::stored_auth_view),
                None,
            ),
        };
        let fleet_refresh_unknown = fleet && self.identified_unresolved_fleet_refresh()?;
        Ok(CollectionFacts {
            account_state: account.map(|(_, state, ..)| state),
            audit_failed,
            detail,
            auth,
            auth_usable_until,
            fleet_refresh_unknown,
        })
    }
}

/// What a credential status row gives to readiness, if anything.
fn auth_readiness_of(row: &CollectorAuthRow) -> Option<CollectorAuthReadiness> {
    if row.state == "signed_out" {
        return Some(CollectorAuthReadiness::SignedOut);
    }
    auth_attention(row).map(CollectorAuthReadiness::Attention)
}
