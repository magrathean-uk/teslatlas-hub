// SPDX-License-Identifier: AGPL-3.0-only

//! Durable provider refusal state, the back-off schedule and the pre-claim gate
//! (C7 R58 to C7 R63).
//!
//! A provider that answers 403 or 429 is refusing this client. The refusal is
//! kept as one versioned record in the store's metadata, bound to the endpoint
//! that refused, so a pause survives a restart and never blocks another
//! endpoint. Time is always the caller's `now_ms`; nothing here reads a clock.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{
    GeocoderError, SharedLimiter, global_limiter,
    request::{is_public_host, request_interval},
    slot_free,
};
use crate::{
    config::GeocoderConfig,
    db::{AddressLookupBinding, HubStore, StoreError},
};

/// C7 K-BACKOFF-FIRST: the pause after a first refusal, doubled per refusal.
const BACKOFF_FIRST_SECONDS: u64 = 60;
/// C7 K-BACKOFF-MAX: the longest pause, and the bound past which a stored
/// pause counts as damaged.
const BACKOFF_MAX_SECONDS: u64 = 86_400;
const BACKOFF_MAX_MS: i64 = (BACKOFF_MAX_SECONDS as i64) * 1_000;
/// C7 K-POLICY-URL: the usage policy of the public host.
pub(crate) const POLICY_URL: &str = "https://operations.osmfoundation.org/policies/nominatim/";
/// The only record version this code reads and writes.
const STATE_VERSION: u8 = 1;

/// C7 R58: `min(max(K-BACKOFF-FIRST x 2^(n - 1), retry_after), K-BACKOFF-MAX)`
/// with saturating arithmetic. A requested delay longer than the schedule is
/// honoured and a shorter one never shortens it.
pub(super) fn backoff_pause(refusals: u32, retry_after: Option<u64>) -> Duration {
    // 2^20 minutes already exceeds the cap, so a larger exponent changes nothing.
    let doublings = refusals.saturating_sub(1).min(20);
    let scheduled = BACKOFF_FIRST_SECONDS.saturating_mul(1_u64 << doublings);
    Duration::from_secs(
        scheduled
            .max(retry_after.unwrap_or(0))
            .min(BACKOFF_MAX_SECONDS),
    )
}

/// C7 R60: the stored refusal record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProviderState {
    v: u8,
    pub(super) endpoint: String,
    pub(super) host: String,
    pub(crate) status: u16,
    pub(crate) refusals: u32,
    pub(crate) since_ms: i64,
    pub(crate) until_ms: i64,
}

impl ProviderState {
    /// The record text, or `None` when it is not a valid version-1 record: not
    /// JSON, another version, a field of the wrong type or out of range (a
    /// status other than 403 and 429, no refusals, a pause that ends before it
    /// began). Such a value is treated as absent and replaced at the next write.
    fn parse(text: &str) -> Option<Self> {
        let state: Self = serde_json::from_str(text).ok()?;
        (state.v == STATE_VERSION
            && matches!(state.status, 403 | 429)
            && state.refusals >= 1
            && state.until_ms >= state.since_ms)
            .then_some(state)
    }

    fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// C7 R59: the end of the pause when it is in force for `endpoint` at
    /// `now_ms`. A record for another endpoint, a pause that has ended and one
    /// that runs longer than the cap past now (a clock change or a damaged
    /// value) are not in force.
    pub(super) fn pause_end(&self, endpoint: &str, now_ms: i64) -> Option<i64> {
        (self.endpoint == endpoint
            && now_ms < self.until_ms
            && self.until_ms <= now_ms.saturating_add(BACKOFF_MAX_MS))
        .then_some(self.until_ms)
    }

    /// Whether the record belongs to `endpoint` (the binding's endpoint text).
    fn is_for(&self, endpoint: &str) -> bool {
        self.endpoint == endpoint
    }
}

/// C7 R60: the stored record, `None` when there is no row or its value is not
/// a valid record. An unreadable store is an error.
pub(super) fn load_state(store: &HubStore) -> Result<Option<ProviderState>, StoreError> {
    Ok(store
        .geocoder_provider_state()?
        .and_then(|text| ProviderState::parse(&text)))
}

/// C7 R58: store the refusal that `status` (403 or 429) just reported by the
/// endpoint of `binding`, continuing the count and the start of `prior` when it
/// is for this endpoint, and emit the one warning event.
pub(super) fn record_refusal(
    store: &HubStore,
    binding: &AddressLookupBinding,
    prior: Option<&ProviderState>,
    status: u16,
    retry_after: Option<u64>,
    now_ms: i64,
) -> Result<(), GeocoderError> {
    let (refusals, since_ms) = match prior {
        Some(prior) if prior.is_for(&binding.endpoint) => {
            (prior.refusals.saturating_add(1), prior.since_ms)
        }
        _ => (1, now_ms),
    };
    let pause_ms =
        i64::try_from(backoff_pause(refusals, retry_after).as_millis()).unwrap_or(i64::MAX);
    let state = ProviderState {
        v: STATE_VERSION,
        endpoint: binding.endpoint.clone(),
        host: binding.endpoint_host.clone(),
        status,
        refusals,
        since_ms,
        until_ms: now_ms.saturating_add(pause_ms),
    };
    let text = state.encode().map_err(|error| {
        GeocoderError::Cache(StoreError::Query(rusqlite::Error::ToSqlConversionFailure(
            Box::new(error),
        )))
    })?;
    store
        .set_geocoder_provider_state(Some(&text))
        .map_err(GeocoderError::Cache)?;
    tracing::warn!(
        host = %state.host,
        status = state.status,
        refusals = state.refusals,
        retry_at_ms = state.until_ms,
        "geocoder provider refused requests"
    );
    Ok(())
}

/// C7 R62: delete the record `prior` after an answer that was not a refusal and
/// emit the one information event. A failed delete is ignored: the stale record
/// is past its pause or for another endpoint, so it blocks nothing, and the next
/// answer deletes it.
pub(super) fn clear_state(store: &HubStore, prior: &ProviderState) {
    if store.set_geocoder_provider_state(None).is_ok() {
        tracing::info!(host = %prior.host, "geocoder provider answered again");
    }
}

/// C7 R61: whether an enrichment step may claim a job now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LookupGate {
    /// A request could start now.
    Open,
    /// The next request slot of the endpoint is later than now.
    Paced,
    /// The provider refused this client and the pause ends at `until_ms`.
    BackingOff { until_ms: i64 },
}

/// The endpoint and binding a configuration describes, or `None` when either
/// cannot be built (the kept path then reports the fault on a job).
fn configured_binding(config: &GeocoderConfig) -> Option<(url::Url, AddressLookupBinding)> {
    let endpoint = config.endpoint_url(false).ok()?;
    let language = config.validated_language().ok()?;
    let binding = AddressLookupBinding::nominatim(&endpoint, &language).ok()?;
    Some((endpoint, binding))
}

/// C7 R61: the gate over the process-wide limiter and the real clock.
pub(crate) fn lookup_gate(config: &GeocoderConfig, store: &HubStore, now_ms: i64) -> LookupGate {
    lookup_gate_with(&global_limiter(), config, store, now_ms, Instant::now())
}

/// C7 R61: the gate with its limiter and its instant given. A configuration
/// that does not build, an unreadable record and a poisoned limiter all give
/// `Open`, because the wrapper fails closed on the same faults.
pub(super) fn lookup_gate_with(
    limiter: &SharedLimiter,
    config: &GeocoderConfig,
    store: &HubStore,
    now_ms: i64,
    now: Instant,
) -> LookupGate {
    let Some((endpoint, binding)) = configured_binding(config) else {
        return LookupGate::Open;
    };
    let Ok(record) = load_state(store) else {
        return LookupGate::Open;
    };
    if let Some(until_ms) = record.and_then(|state| state.pause_end(&binding.endpoint, now_ms)) {
        return LookupGate::BackingOff { until_ms };
    }
    match slot_free(limiter, request_interval(&endpoint), now) {
        Ok(false) => LookupGate::Paced,
        Ok(true) | Err(_) => LookupGate::Open,
    }
}

/// C7 R63: what `doctor` shows about the configured endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderStatus {
    /// The configured endpoint's host, or `None` when the endpoint is refused.
    pub(crate) endpoint_host: Option<String>,
    pub(crate) public_host: bool,
    /// The stored record when it is for the configured endpoint.
    pub(crate) refusal: Option<ProviderState>,
    /// The pause is in force for the configured endpoint at the given time.
    pub(crate) backing_off: bool,
}

/// C7 R63: read the configured endpoint's status at `now_ms`. It reads the store
/// and writes nothing; an unreadable record shows as no record, because the
/// report must not fail on a back-off that concerns optional enrichment only.
pub(crate) fn provider_status(
    config: &GeocoderConfig,
    store: &HubStore,
    now_ms: i64,
) -> ProviderStatus {
    let endpoint = config.endpoint_url(false).ok();
    let binding = configured_binding(config).map(|(_, binding)| binding);
    let refusal = binding.as_ref().and_then(|binding| {
        load_state(store)
            .ok()
            .flatten()
            .filter(|state| state.is_for(&binding.endpoint))
    });
    let backing_off = binding.as_ref().is_some_and(|binding| {
        refusal
            .as_ref()
            .is_some_and(|state| state.pause_end(&binding.endpoint, now_ms).is_some())
    });
    ProviderStatus {
        endpoint_host: endpoint
            .as_ref()
            .and_then(|endpoint| endpoint.host_str().map(ToOwned::to_owned)),
        public_host: endpoint.as_ref().is_some_and(is_public_host),
        refusal,
        backing_off,
    }
}
