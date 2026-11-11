// SPDX-License-Identifier: AGPL-3.0-only
use super::*;

/// TRANSITIONAL: V2 displays these fixed codes, never issuer descriptions.
#[expect(dead_code, reason = "TRANSITIONAL: V2 sign-in error cases.")]
#[derive(Debug)]
pub(crate) enum SignInError { StateMismatch, Refused(SignInRefusalCode), Expired, AnswerInvalid, Unavailable, BindingInvalid, SettingsInvalid, Cancelled, ServicePauseFailed }
/// TRANSITIONAL: R69's complete RFC 6749 refusal allowlist.
#[expect(dead_code, reason = "TRANSITIONAL: V2 issuer refusal codes.")]
#[derive(Debug)]
pub(crate) enum SignInRefusalCode { InvalidRequest, UnauthorizedClient, AccessDenied, UnsupportedResponseType, InvalidScope, ServerError, TemporarilyUnavailable, Other }
#[expect(dead_code, reason = "TRANSITIONAL: V2 refusal-code conversion.")]
impl SignInRefusalCode {
    pub(in crate::auth) fn from_issuer_code(code: &str) -> Self {
        match code {
            "invalid_request" => Self::InvalidRequest, "unauthorized_client" => Self::UnauthorizedClient,
            "access_denied" => Self::AccessDenied, "unsupported_response_type" => Self::UnsupportedResponseType,
            "invalid_scope" => Self::InvalidScope, "server_error" => Self::ServerError,
            "temporarily_unavailable" => Self::TemporarilyUnavailable, _ => Self::Other,
        }
    }
    pub(in crate::auth) fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request", Self::UnauthorizedClient => "unauthorized_client",
            Self::AccessDenied => "access_denied", Self::UnsupportedResponseType => "unsupported_response_type",
            Self::InvalidScope => "invalid_scope", Self::ServerError => "server_error",
            Self::TemporarilyUnavailable => "temporarily_unavailable", Self::Other => "other",
        }
    }
}
impl SignInError {
    pub(in crate::auth) fn code(&self) -> &'static str {
        match self {
            Self::StateMismatch => "sign_in_state_mismatch", Self::Refused(_) => "sign_in_refused", Self::Expired => "sign_in_expired",
            Self::AnswerInvalid => "sign_in_answer_invalid", Self::Unavailable => "sign_in_unavailable", Self::BindingInvalid => "sign_in_binding_invalid",
            Self::SettingsInvalid => "sign_in_settings_invalid", Self::Cancelled => "sign_in_cancelled", Self::ServicePauseFailed => "sign_in_service_pause_failed",
        }
    }
}

/// TRANSITIONAL: consumed by the V2 redemption flow.
#[expect(dead_code, reason = "TRANSITIONAL: V2 sign-in session and secret state.")]
pub(crate) struct SignInSession {
    pub(super) id: Uuid, pub(super) slot: CredentialSlot,
    pub(super) grant_secret: Zeroizing<[u8; 32]>, pub(super) state: Zeroizing<[u8; 32]>,
    pub(super) expires_at_ms: i64,
}
#[expect(dead_code, reason = "TRANSITIONAL: V2 session accessors.")]
impl SignInSession {
    pub(in crate::auth) fn id(&self) -> Uuid { self.id }
    pub(in crate::auth) fn slot(&self) -> CredentialSlot { self.slot }
    pub(in crate::auth) fn grant_secret(&self) -> &[u8; 32] { &self.grant_secret }
    pub(in crate::auth) fn state(&self) -> &[u8; 32] { &self.state }
    pub(in crate::auth) fn expires_at_ms(&self) -> i64 { self.expires_at_ms }
}
impl std::fmt::Debug for SignInSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignInSession").field("slot", &self.slot).finish_non_exhaustive()
    }
}
/// TRANSITIONAL: only the V2 validated redemption constructs this value.
#[cfg_attr(not(test), expect(dead_code, reason = "TRANSITIONAL: V2 validated redemption."))]
pub(crate) struct RedeemedGrant {
    pub(super) session: SignInSession, pub(super) access: Zeroizing<String>, pub(super) refresh: Zeroizing<String>,
    pub(super) expires_in: u64, pub(super) client_id: Option<String>, pub(super) region: Option<crate::fleet_api::FleetRegion>,
}
impl RedeemedGrant {
    #[cfg_attr(not(test), expect(dead_code, reason = "TRANSITIONAL: V2 redemption constructor."))]
    pub(in crate::auth) fn from_validated_redemption(session: SignInSession, access: Zeroizing<String>, refresh: Zeroizing<String>, expires_in: u64, client_id: Option<String>, region: Option<crate::fleet_api::FleetRegion>) -> Result<Self, CredentialStoreError> {
        if canonical_token(access.as_bytes()) != access.as_bytes() || canonical_token(refresh.as_bytes()) != refresh.as_bytes() {
            return Err(CredentialStoreError::SignIn(SignInError::AnswerInvalid));
        }
        if access.len() > super::super::credential_envelope::MAX_TOKEN_PLAINTEXT_BYTES || refresh.len() > super::super::credential_envelope::MAX_TOKEN_PLAINTEXT_BYTES { return Err(CredentialStoreError::TokenTooLarge); }
        if access.is_empty() { return Err(CredentialStoreError::EmptyAccessToken); }
        if refresh.is_empty() { return Err(CredentialStoreError::EmptyRefreshToken); }
        if session.slot == CredentialSlot::FleetApi {
            if !(60..=365 * 24 * 3600).contains(&expires_in) { return Err(CredentialStoreError::SignIn(SignInError::AnswerInvalid)); }
            crate::fleet_api::FleetClientId::parse(client_id.as_deref().unwrap_or_default()).map_err(|_| CredentialStoreError::SignIn(SignInError::SettingsInvalid))?;
            if region.is_none() { return Err(CredentialStoreError::SignIn(SignInError::SettingsInvalid)); }
        } else if client_id.is_some() || region.is_some() { return Err(CredentialStoreError::InvalidOrigin); }
        Ok(Self { session, access, refresh, expires_in, client_id, region })
    }
}
impl std::fmt::Debug for RedeemedGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedeemedGrant").field("slot", &self.session.slot).field("access_len", &self.access.len()).field("refresh_len", &self.refresh.len()).finish()
    }
}
/// TRANSITIONAL: an aftercare receipt with no authority to revoke the pair.
#[cfg_attr(not(test), expect(dead_code, reason = "TRANSITIONAL: V2 grant receipt."))]
pub(crate) struct StoredHubGrant { pub(super) slot: CredentialSlot, pub(super) generation: Uuid }
#[cfg_attr(not(test), expect(dead_code, reason = "TRANSITIONAL: V2 grant receipt accessors."))]
impl StoredHubGrant {
    pub(in crate::auth) fn new(slot: CredentialSlot, generation: Uuid) -> Self { Self { slot, generation } }
    #[cfg_attr(test, expect(dead_code, reason = "TRANSITIONAL: V2 grant slot accessor."))]
    pub(in crate::auth) fn slot(&self) -> CredentialSlot { self.slot }
    pub(in crate::auth) fn generation(&self) -> Uuid { self.generation }
}
impl std::fmt::Debug for StoredHubGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("StoredHubGrant").field("slot", &self.slot).finish_non_exhaustive() }
}
pub(crate) struct CredentialEntry {
    pub(super) slot: CredentialSlot, pub(super) origin: CredentialOrigin,
    pub(super) owner: RefreshOwner, pub(super) access: Zeroizing<String>, pub(super) refresh: Zeroizing<String>,
    pub(super) legacy: LegacyCandidates, pub(super) client_id: Option<String>, pub(super) region: Option<String>,
    pub(super) expires_at: i64, pub(super) next_refresh_at: i64,
}
impl std::fmt::Debug for CredentialEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialEntry").field("slot", &self.slot).field("origin", &self.origin)
            .field("refresh_owner", &self.owner).field("access_len", &self.access.len()).field("refresh_len", &self.refresh.len()).finish()
    }
}
impl CredentialEntry {
    fn build(access: Zeroizing<String>, refresh: Zeroizing<String>, origin: CredentialOrigin) -> Result<Self, CredentialStoreError> {
        let legacy = LegacyCandidates::for_secret_parts(&access, &refresh)?;
        let access = canonical_text(&access)?; let refresh = canonical_text(&refresh)?;
        if access.is_empty() { return Err(CredentialStoreError::EmptyAccessToken); }
        if refresh.is_empty() && origin != CredentialOrigin::ProviderEntry { return Err(CredentialStoreError::EmptyRefreshToken); }
        Ok(Self { slot: CredentialSlot::OwnerApi, origin,
            owner: if origin == CredentialOrigin::SourceToken { RefreshOwner::External } else { RefreshOwner::Hub },
            access, refresh, legacy, client_id: None, region: None, expires_at: 0, next_refresh_at: 0 })
    }
    // TRANSITIONAL: the three V2 Owner entry paths consume these constructors.
    #[cfg_attr(not(test), expect(dead_code, reason = "TRANSITIONAL: V2 direct entry."))]
    pub(in crate::auth) fn direct_entry(access: Zeroizing<String>, refresh: Zeroizing<String>) -> Result<Self, CredentialStoreError> { Self::build(access, refresh, CredentialOrigin::DirectEntry) }
    #[cfg_attr(not(test), expect(dead_code, reason = "TRANSITIONAL: V2 source-token entry."))]
    pub(in crate::auth) fn source_token(access: Zeroizing<String>, refresh: Zeroizing<String>) -> Result<Self, CredentialStoreError> { Self::build(access, refresh, CredentialOrigin::SourceToken) }
    #[cfg_attr(not(test), expect(dead_code, reason = "TRANSITIONAL: V2 provider entry."))]
    pub(in crate::auth) fn provider_entry(access: Zeroizing<String>, refresh: Option<Zeroizing<String>>) -> Result<Self, CredentialStoreError> { Self::build(access, refresh.unwrap_or_default(), CredentialOrigin::ProviderEntry) }
    pub(in crate::auth) fn fleet_supplied(access: Zeroizing<String>, refresh: Zeroizing<String>, client_id: String, region: crate::fleet_api::FleetRegion, expires_in: u64) -> Result<Self, CredentialStoreError> {
        crate::fleet_api::FleetClientId::parse(&client_id).map_err(|_| CredentialStoreError::InvalidOrigin)?;
        let mut entry = Self::build(access, refresh, CredentialOrigin::FleetOauth)?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| CredentialStoreError::InvalidOrigin)?.as_secs();
        let expires = now.checked_add(expires_in).and_then(|v| i64::try_from(v).ok()).ok_or(CredentialStoreError::InvalidOrigin)?;
        if !(60..=365 * 24 * 3600).contains(&expires_in) { return Err(CredentialStoreError::InvalidOrigin); }
        entry.slot = CredentialSlot::FleetApi; entry.client_id = Some(client_id); entry.region = Some(region.storage_code().to_owned());
        entry.expires_at = expires; entry.next_refresh_at = expires - (expires_in / 4).min(1800) as i64; Ok(entry)
    }
    pub(super) fn row(&self, generation: Uuid) -> SealedCredentialPair {
        SealedCredentialPair { account_id: 1, slot: self.slot, origin: Some(self.origin), refresh_owner: self.owner,
            generation, access_envelope: Vec::new(), refresh_envelope: Vec::new(), refresh_present: !self.refresh.is_empty(),
            refresh_fingerprint: None, expires_at: self.expires_at, next_refresh_at: self.next_refresh_at,
            client_id: self.client_id.clone(), region: self.region.clone(), migrated: false, written_at_ms: 0 }
    }
    pub(in crate::auth) fn set_schedule(&mut self, expires_at: i64, next_refresh_at: i64) {
        self.expires_at = expires_at; self.next_refresh_at = next_refresh_at;
    }
}
pub(super) fn canonical_text(value: &str) -> Result<Zeroizing<String>, CredentialStoreError> {
    let bytes = canonical_token(value.as_bytes());
    if bytes.len() > super::super::credential_envelope::MAX_TOKEN_PLAINTEXT_BYTES { return Err(CredentialStoreError::TokenTooLarge); }
    let value = std::str::from_utf8(bytes).map_err(|_| CredentialStoreError::InvalidTokenBytes)?;
    let mut result = Zeroizing::new(String::with_capacity(value.len())); result.push_str(value); Ok(result)
}
