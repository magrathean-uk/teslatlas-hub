// SPDX-License-Identifier: AGPL-3.0-only

//! The Owner API requests that carry the legacy credential (C5 part P4: R15,
//! R26, R29, R30, R40, R41, R41a, R42, R48, R55, R61).
//!
//! Every request made here records the generation it carried and its scope, and
//! a 401 is answered by what the fuse decides (`legacy_auth::rejection`): a
//! request that carried a superseded pair is re-sent once with the current one;
//! one that met a pair known to be expired, or a remedy already under way, fails
//! with `AuthUnavailable`; evidence is counted once per generation; a car's own
//! 401 is confirmed by one account-wide read before the account is blamed; and a
//! counted rejection forces a refresh, pauses, or signs the account out.
//!
//! The authentication rules stop at 401. A 403 on a read is returned unchanged
//! for C3's classifier (R48); a 403 on a resident action fails the action and
//! nothing else, because an action's refusal is no evidence that reads are
//! withdrawn (F13).

use reqwest::header::DATE;
use serde::de::IgnoredAny;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::*;
use crate::legacy_auth::{RejectionAction, RequestScope, authority::stop_error, state::AuthStop};

/// What an answer said besides its body: the server's `Date` (R55) and whether
/// the status was 2xx (R42), which holds even when the body is unusable.
#[derive(Default)]
pub(super) struct AnswerMeta {
    pub(super) date: Option<String>,
    pub(super) success: bool,
}

/// The pair a request carried (R40). The access token is kept, zeroised, only
/// until the answer has been judged: a refresh that kept the refresh token
/// leaves the generation unchanged while the access token changes, and the
/// token is what tells a request that carried the pair in use from one that
/// carried an older one.
struct Carried {
    generation: Uuid,
    access_token: Zeroizing<String>,
}

impl Carried {
    fn of(auth: &LegacyAuthManager) -> Self {
        Self {
            generation: auth.generation(),
            access_token: Zeroizing::new(auth.current_access_token().to_owned()),
        }
    }

    /// Whether the unit still holds the pair this request carried.
    fn is_current(&self, auth: &LegacyAuthManager) -> bool {
        auth.generation() == self.generation
            && auth.current_access_token() == self.access_token.as_str()
    }
}

/// What the request does after a 401 has been handled.
enum Next {
    /// Send it again, once, with the pair now in use (R61).
    Resend,
    /// It fails with this error.
    Fail(OwnerApiAuthError),
}

/// What a confirmation read found (R41a).
enum Confirmation {
    /// A 2xx: the account accepts the token, so the 401 was about one car.
    Disproved,
    /// A 401: the account refuses the token, which is a counted rejection.
    Rejected,
    /// Anything else: not evidence either way. The answer's own error is kept for
    /// the collector (R41a, C5-S4); `None` when the read was not sent at all (one
    /// inside the 60 s spacing).
    Inconclusive(Option<OwnerApiError>),
}

/// `AuthUnavailable` as the error of a request (R29, R40, R56).
fn unavailable(retry_at: Option<i64>) -> OwnerApiAuthError {
    OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
        LegacyAuthError::AuthUnavailable { retry_at },
    ))
}

impl OwnerApi {
    /// One authenticated read of the Owner API (C5 R15, R26, R29, R30, R40, R42,
    /// R61): the pair is made ready first, the request carries the current
    /// access token, and a 401 is handled by the rules of R40 and R41.
    pub(super) async fn get_envelope_with_legacy_auth_url_fused<T>(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        endpoint: Url,
        scope: RequestScope,
        power_gate: Option<&StreamPowerGate>,
    ) -> Result<T, OwnerApiAuthError>
    where
        T: DeserializeOwned,
    {
        let mut resent = false;
        loop {
            let (carried, answer) = self
                .send_legacy_read::<T>(auth, fuse, &endpoint, scope, power_gate)
                .await?;
            match answer {
                Ok(value) => return Ok(value),
                Err(OwnerApiError::HttpStatus(401)) => {
                    match self
                        .handle_unauthorised(auth, fuse, scope, &carried)
                        .await?
                    {
                        // R61: a request that failed with a 401 is re-sent at most
                        // once in total, whichever rule asks.
                        Next::Resend if !resent => resent = true,
                        Next::Resend => return Err(unavailable(None)),
                        Next::Fail(error) => return Err(error),
                    }
                }
                Err(error) => return Err(OwnerApiAuthError::Owner(error)),
            }
        }
    }

    /// Make the pair ready, send one read, and record what the answer showed
    /// about the clock (R55) and, for a 2xx, about the fuse (R42).
    async fn send_legacy_read<T>(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        endpoint: &Url,
        scope: RequestScope,
        power_gate: Option<&StreamPowerGate>,
    ) -> Result<(Carried, Result<T, OwnerApiError>), OwnerApiAuthError>
    where
        T: DeserializeOwned,
    {
        auth.sync_fuse(fuse);
        self.prepare_legacy_auth(auth).await?;
        // A pair written under the unit while it waited resets the fuse (R42).
        auth.sync_fuse(fuse);
        if power_gate.is_some_and(|gate| !gate.is_confirmed()) {
            return Err(OwnerApiAuthError::Owner(
                OwnerApiError::StreamPowerNotConfirmed,
            ));
        }
        let request =
            self.envelope_request(auth.access_token_for_sensitive_use()?, endpoint.clone());
        auth.assert_sensitive_access()?;
        let carried = Carried::of(auth);
        if scope == RequestScope::Account {
            fuse.note_account_generation(carried.generation);
        }
        let mut meta = AnswerMeta::default();
        // The request is counted as sent here, and only here (Codex R08).
        fuse.note_send();
        let answer = self.execute_envelope_request_with(request, &mut meta).await;
        self.note_answer(auth, fuse, scope, &carried, &meta);
        Ok((carried, answer))
    }

    /// R42, R55: what an answer says to the unit before the caller reads it. A
    /// 2xx that carried the pair now in use disproves rejection and clears the
    /// car's mark (R41a); the server's `Date` feeds the skew flag.
    fn note_answer(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        scope: RequestScope,
        carried: &Carried,
        meta: &AnswerMeta,
    ) {
        auth.note_server_date(meta.date.as_deref());
        if meta.success && carried.is_current(auth) {
            fuse.record_success(carried.generation, scope);
        }
        auth.sync_fuse(fuse);
    }

    /// Handle a 401 (R40, R41, R41a): the fuse decides, and the decision is
    /// carried out here.
    async fn handle_unauthorised(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        scope: RequestScope,
        carried: &Carried,
    ) -> Result<Next, OwnerApiAuthError> {
        let now = auth.clock();
        let action = if carried.is_current(auth) {
            let context = auth.rejection_context(scope, carried.generation, now);
            fuse.record_rejection(now, &context)
        } else {
            // R40 step 1: the pair has been replaced since the request was made.
            RejectionAction::RetryWithCurrent
        };
        auth.sync_fuse(fuse);
        if action == RejectionAction::Confirm {
            return match self.confirm(auth, fuse, carried).await? {
                Confirmation::Disproved => {
                    // The account accepts the token: this car's own access was
                    // refused, and the mark spares the next 401 of the car a read.
                    if let Some(vehicle) = scope.vehicle() {
                        fuse.mark_vehicle_rejected(vehicle, carried.generation);
                    }
                    auth.sync_fuse(fuse);
                    Ok(Next::Fail(OwnerApiAuthError::Auth(
                        LegacyAuthManagerError::Auth(LegacyAuthError::VehicleAccessRejected),
                    )))
                }
                Confirmation::Rejected => {
                    // The account refuses the token: a counted rejection, and the
                    // original request follows its action (R41a).
                    let now = auth.clock();
                    let context =
                        auth.rejection_context(RequestScope::Account, carried.generation, now);
                    let counted = fuse.count_rejection(now, &context);
                    auth.sync_fuse(fuse);
                    self.carry_out(auth, fuse, scope, carried, counted, now)
                        .await
                }
                Confirmation::Inconclusive(answer) => {
                    // R41a: the request fails `AuthUnavailable`, and the answer of
                    // the read goes to the collector's own classification like any
                    // list answer (C5-S4).
                    if let Some(error) = answer {
                        fuse.note_confirmation_error(error);
                    }
                    let retry_at = fuse.confirmation_retry_at().or_else(|| {
                        Some(auth.clock().saturating_add(
                            crate::legacy_auth::rejection::CONFIRMATION_SPACING_SECONDS,
                        ))
                    });
                    Ok(Next::Fail(unavailable(retry_at)))
                }
            };
        }
        self.carry_out(auth, fuse, scope, carried, action, now)
            .await
    }

    /// Carry out a decision of the fuse other than `Confirm`.
    async fn carry_out(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        scope: RequestScope,
        carried: &Carried,
        action: RejectionAction,
        now: i64,
    ) -> Result<Next, OwnerApiAuthError> {
        // A resident action is never sent again: repeating a mutation after an
        // ambiguous answer would be unsafe, so where a read would be re-sent the
        // action fails and the caller decides.
        let resend = if matches!(scope, RequestScope::Action(_)) {
            Next::Fail(unavailable(None))
        } else {
            Next::Resend
        };
        let next = match action {
            RejectionAction::RetryWithCurrent => resend,
            RejectionAction::NotCounted => Next::Fail(OwnerApiAuthError::Auth(
                LegacyAuthManagerError::Auth(auth.unavailable_error(now)),
            )),
            RejectionAction::Coalesced => {
                // R40 step 3: the request waits for the action under way, and is
                // re-sent once if it produced a newer pair.
                auth.wait_for_exchange().await;
                auth.sync_fuse(fuse);
                if carried.is_current(auth) {
                    Next::Fail(OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
                        auth.unavailable_error(auth.clock()),
                    )))
                } else {
                    resend
                }
            }
            RejectionAction::VehicleRejected(_) => Next::Fail(OwnerApiAuthError::Auth(
                LegacyAuthManagerError::Auth(LegacyAuthError::VehicleAccessRejected),
            )),
            RejectionAction::ForceRefreshThenRetry => {
                let refreshed = auth
                    .refresh_now(&self.legacy_auth_http_client(), SystemTime::now())
                    .await;
                auth.sync_fuse(fuse);
                match refreshed {
                    Ok(()) if !carried.is_current(auth) => resend,
                    Ok(()) => Next::Fail(OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
                        auth.unavailable_error(auth.clock()),
                    ))),
                    Err(error)
                        if error.is_sensitive_access_failure()
                            || error.is_terminal_auth_failure() =>
                    {
                        return Err(error.into());
                    }
                    Err(_) => Next::Fail(OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
                        auth.unavailable_error(auth.clock()),
                    ))),
                }
            }
            RejectionAction::PauseThenRefresh { until }
            | RejectionAction::PauseThenConfirm { until } => {
                // Requests and stream admissions pause until `until`; the forced
                // refresh, or the confirming request, follows it (R41, R8a).
                Next::Fail(unavailable(Some(until)))
            }
            RejectionAction::Stop(reason) => {
                auth.stop_account(reason);
                auth.sync_fuse(fuse);
                Next::Fail(OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
                    stop_error(AuthStop::SignedOut(reason)),
                )))
            }
            RejectionAction::ObserverWait => {
                auth.observer_rejected();
                auth.sync_fuse(fuse);
                Next::Fail(unavailable(Some(auth.clock().saturating_add(
                    crate::legacy_auth::state::EXTERNAL_WRITE_RECHECK_SECONDS,
                ))))
            }
            // The caller handles `Confirm` before it gets here.
            RejectionAction::Confirm => Next::Fail(unavailable(None)),
        };
        Ok(next)
    }

    /// One confirmation read (R41a): one account-wide read with the pair in use.
    /// At most one starts per 60 s; one inside that interval is inconclusive and
    /// leaves the fuse unchanged.
    async fn confirm(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        carried: &Carried,
    ) -> Result<Confirmation, OwnerApiAuthError> {
        if fuse.begin_confirmation(auth.clock()).is_err() {
            return Ok(Confirmation::Inconclusive(None));
        }
        let read = self.confirmation_read(auth, fuse, carried).await;
        fuse.finish_confirmation();
        read
    }

    async fn confirmation_read(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        carried: &Carried,
    ) -> Result<Confirmation, OwnerApiAuthError> {
        let endpoint = self
            .base_url
            .endpoint("api/1/products")
            .map_err(OwnerApiAuthError::Owner)?;
        let request = self.envelope_request(auth.access_token_for_sensitive_use()?, endpoint);
        auth.assert_sensitive_access()?;
        let mut meta = AnswerMeta::default();
        let answer: Result<IgnoredAny, OwnerApiError> =
            self.execute_envelope_request_with(request, &mut meta).await;
        // A 2xx is a success whatever its body, and resets the fuse (R42).
        self.note_answer(auth, fuse, RequestScope::Account, carried, &meta);
        Ok(match answer {
            _ if meta.success => Confirmation::Disproved,
            Err(OwnerApiError::HttpStatus(401)) => Confirmation::Rejected,
            Err(error) => Confirmation::Inconclusive(Some(error)),
            // A 2xx is matched above; no other `Ok` exists.
            Ok(_) => Confirmation::Disproved,
        })
    }

    /// R40, the stream case: a stream `Rejected` transition carries no
    /// generation and is never counted by itself. If the rejection was already
    /// disproved for the pair in use it is ignored; otherwise one confirmation
    /// read decides: a 2xx marks the pair as cleared, a 401 is a counted
    /// rejection and gets the action of R41. Nothing is re-sent.
    pub(crate) async fn confirm_stream_rejection(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
    ) -> Result<(), OwnerApiAuthError> {
        auth.sync_fuse(fuse);
        let carried = Carried::of(auth);
        if fuse.is_blown() || fuse.stream_cleared(carried.generation) {
            return Ok(());
        }
        if fuse.begin_confirmation(auth.clock()).is_err() {
            return Ok(());
        }
        let read = self.confirmation_read(auth, fuse, &carried).await;
        fuse.finish_confirmation();
        match read? {
            Confirmation::Disproved => {
                fuse.note_stream_cleared(carried.generation);
                Ok(())
            }
            Confirmation::Rejected => {
                let now = auth.clock();
                let context =
                    auth.rejection_context(RequestScope::Account, carried.generation, now);
                let counted = fuse.count_rejection(now, &context);
                auth.sync_fuse(fuse);
                match self
                    .carry_out(auth, fuse, RequestScope::Account, &carried, counted, now)
                    .await
                {
                    Err(error) => Err(error),
                    Ok(_) => Ok(()),
                }
            }
            Confirmation::Inconclusive(answer) => {
                if let Some(error) = answer {
                    fuse.note_confirmation_error(error);
                }
                Ok(())
            }
        }
    }

    /// Make the pair ready before a request (C5 R15, R26, R29, R30, R43): the
    /// manager waits for an exchange in flight, applies its result, and sends a
    /// token request first only when one is due (an imported pair on its first
    /// need, a schedule that has come, a retry that has come, the refresh that
    /// follows a confirmation pause). A failed or refused attempt does not stop
    /// the request while the current token is still usable (R30, R45): the token
    /// read that follows decides. What does stop it is a lost sensitive
    /// authority, and a stop of the account (a sign-out, a refusal, a
    /// configuration error), which every request sees.
    pub(super) async fn prepare_legacy_auth(
        &self,
        auth: &mut LegacyAuthManager,
    ) -> Result<(), OwnerApiAuthError> {
        match auth
            .refresh_if_due(&self.legacy_auth_http_client(), SystemTime::now())
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if error.is_sensitive_access_failure() => Err(error.into()),
            Err(error) if error.is_terminal_auth_failure() => Err(error.into()),
            Err(_) => Ok(()),
        }
    }

    /// Execute one explicit legacy Owner API action using the resident
    /// credential owner. A rejected action is never retried: repeating a
    /// mutation after an ambiguous response would be unsafe. A 401 is still
    /// evidence (R40) and is handled like a read's; a 403 fails the action and
    /// reports nothing as refusal evidence (R48, F13).
    pub(crate) async fn execute_vehicle_action_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
        action: LegacyVehicleAction,
    ) -> Result<LegacyVehicleActionResult, OwnerApiAuthError> {
        let scope = RequestScope::Action(vehicle_id);
        // C3 R48, R49 (B2 clarification 3): a standing refusal for the configured
        // mode, or an unreadable account row, rejects a new action before any
        // authentication is prepared and again before a token is handed out; the
        // caller holds the shared locks, so a queued caller is checked after it
        // acquires them.
        auth.assert_resident_admission()?;
        auth.sync_fuse(fuse);
        self.prepare_legacy_auth(auth).await?;
        auth.assert_resident_admission()?;
        auth.sync_fuse(fuse);
        let endpoint = self.vehicle_action_endpoint(vehicle_id, action)?;
        let body = action.request_body_bytes()?;
        let request = self
            .client
            .post(endpoint)
            .header(ACCEPT, ACCEPT_JSON.clone())
            .header(CONTENT_TYPE, ACCEPT_JSON.clone())
            .bearer_auth(auth.access_token_for_sensitive_use()?)
            .body(body);
        auth.assert_sensitive_access()?;
        let carried = Carried::of(auth);
        let mut meta = AnswerMeta::default();
        fuse.note_send();
        let result = self
            .execute_vehicle_action_request_with(request, action, &mut meta)
            .await;
        self.note_answer(auth, fuse, scope, &carried, &meta);
        match result {
            Err(OwnerApiError::HttpStatus(401)) => {
                match self
                    .handle_unauthorised(auth, fuse, scope, &carried)
                    .await?
                {
                    // `carry_out` never asks to re-send an action.
                    Next::Resend => Err(unavailable(None)),
                    Next::Fail(error) => Err(error),
                }
            }
            // R48: an action's 403 is a failed action and nothing more.
            Err(OwnerApiError::Forbidden { .. }) => {
                Err(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(403)))
            }
            Err(error) => Err(OwnerApiAuthError::Owner(error)),
            Ok(report) => Ok(report),
        }
    }

    pub(crate) async fn list_vehicles_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
    ) -> Result<Vec<Vehicle>, OwnerApiAuthError> {
        let endpoint = self
            .base_url
            .endpoint("api/1/products")
            .map_err(OwnerApiAuthError::Owner)?;
        let envelope: ResponseEnvelope<Vec<ProductWire>> = self
            .get_envelope_with_legacy_auth_url_fused(
                auth,
                fuse,
                endpoint,
                RequestScope::Account,
                None,
            )
            .await?;
        let mut vehicles = parse_vehicle_list(envelope).map_err(OwnerApiAuthError::Owner)?;
        ProviderSecrets::new(&[
            auth.access_token_for_scrubbing()?,
            auth.refresh_token_for_sensitive_use()?,
        ])
        .scrub_vehicle_list(&mut vehicles);
        Ok(vehicles)
    }

    pub(crate) async fn vehicle_data_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
        power_gate: Option<&StreamPowerGate>,
    ) -> Result<VehicleData, OwnerApiAuthError> {
        let endpoint = self.vehicle_data_endpoint(vehicle_id)?;
        let envelope: Value = self
            .get_envelope_with_legacy_auth_url_fused(
                auth,
                fuse,
                endpoint,
                RequestScope::Vehicle(vehicle_id),
                power_gate,
            )
            .await?;
        // Refresh happens only before a request, so the pair read here is exactly
        // the pair that was sent last and the one a provider could echo.
        let secrets = ProviderSecrets::new(&[
            auth.access_token_for_scrubbing()?,
            auth.refresh_token_for_sensitive_use()?,
        ]);
        parse_vehicle_data(vehicle_id, envelope, &secrets).map_err(OwnerApiAuthError::Owner)
    }

    pub(crate) async fn vehicle_probe_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
    ) -> Result<bool, OwnerApiAuthError> {
        let endpoint = self.vehicle_probe_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused(
                auth,
                fuse,
                endpoint,
                RequestScope::Vehicle(vehicle_id),
                None,
            )
            .await?;
        Ok(envelope
            .response
            .get("in_service")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    pub(crate) async fn vehicle_state_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
    ) -> Result<String, OwnerApiAuthError> {
        let endpoint = self.vehicle_probe_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused(
                auth,
                fuse,
                endpoint,
                RequestScope::Vehicle(vehicle_id),
                None,
            )
            .await?;
        let state = parse_vehicle_state(envelope.response).map_err(OwnerApiAuthError::Owner)?;
        // The state is stored as the vehicle's source state and served as its
        // current state, so a credential echoed into it is replaced here, before
        // the scheduler or the store sees it.
        let secrets = ProviderSecrets::new(&[
            auth.access_token_for_scrubbing()?,
            auth.refresh_token_for_sensitive_use()?,
        ]);
        Ok(secrets.scrub_vehicle_state(state))
    }

    /// One bounded read with the headers the answer carried (R55) and whether
    /// its status was 2xx (R42). A 2xx whose body cannot be used is still a 2xx.
    pub(super) async fn execute_envelope_request_with<T>(
        &self,
        request: reqwest::RequestBuilder,
        meta: &mut AnswerMeta,
    ) -> Result<T, OwnerApiError>
    where
        T: DeserializeOwned,
    {
        #[cfg(test)]
        crate::api::test_egress_guard::refuse_production_request(&request);
        let response = request.send().await.map_err(classify_transport_error)?;
        meta.date = response
            .headers()
            .get(DATE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if !response.status().is_success() {
            return Err(owner_error_for_response(response).await);
        }
        meta.success = true;
        let bytes = read_limited_response(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| OwnerApiError::InvalidResponseEnvelope)
    }
}
