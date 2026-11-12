// SPDX-License-Identifier: AGPL-3.0-only

//! The Hub's own owner code exchange (DEC-088; T01 design sections 1.1 and 4).
//!
//! The Mac app's sign-in window obtains an authorisation code in the owner's
//! browser session and hands the code, its PKCE verifier and the redirect URI
//! to `setup`; the running service calls [`exchange_owner_code`]. The service
//! posts the exchange to the owner token endpoint with the standard public
//! owner client ([`super::owner_client`]) and persists only that in-memory
//! answer, with origin `hub_login` (the design's `OwnLegacyExchange`). No input
//! here accepts a token.
//!
//! Order, so that a refusal changes nothing and opens no connection:
//! 1. the material is checked locally (exact redirect, verifier grammar,
//!    bounded code);
//! 2. the code is recorded as used in this process, so a second exchange with
//!    the same code is refused locally, whatever happened to the first;
//! 3. the window's egress gate is asked for a token-exchange permit
//!    (`window_closed` unless a window is Open), immediately before the send;
//! 4. one POST, bounded by the permit, with no redirect followed and the answer
//!    read to at most [`ANSWER_CAP`] bytes;
//! 5. only a complete, validated answer is stored, through the vault's one
//!    grant path, which keeps its admission rules and the refresh fence. The
//!    permit is held until that write and its aftercare are done, and is
//!    checked through the window's write fence before every mutation, so
//!    once the window has lapsed it nothing more is written; the window is
//!    handed back within its grace whatever the write is doing (design 2.7,
//!    M02-T13 round 2).
//!
//! Every error is a fixed code: no code, verifier, token or issuer text appears
//! in an error, a log line or a report.

use std::{collections::VecDeque, sync::Mutex, time::{SystemTime, UNIX_EPOCH}};

use reqwest::{Client, header::{ACCEPT, CONTENT_TYPE, USER_AGENT}};
use serde::Serialize;
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::{Zeroize, Zeroizing};

use crate::CredentialVaultEntry;
use crate::fleet_api::egress::{self, PermitKind, Provider};
use super::{CredentialCommandError, owner_client::{OWNER_CLIENT_ID, OWNER_REDIRECT_URI, OWNER_TOKEN_ENDPOINT}, protocol::token_answer};
use super::super::{credential_envelope::CredentialSlot, credential_vault::{CredentialStoreError, RedeemedGrant, SignInError, SignInRefusalCode, WriteAuthority}};

/// The longest authorisation code accepted. Tesla's codes are far shorter; the
/// bound keeps the request small and refuses pasted garbage locally.
pub const MAX_OWNER_CODE_BYTES: usize = 2048;

/// The most answer bytes read (as the refresh path's R34 cap). A longer answer
/// is refused and nothing is stored.
pub const ANSWER_CAP: usize = 64 * 1024;

/// How many used codes this process remembers. A code is one-use and expires
/// within minutes, so the most recent few hundred cover every honest retry.
const USED_CODES_KEPT: usize = 256;

/// The refusal of a code this process has already exchanged or tried.
pub const SIGN_IN_CODE_REUSED: &str = "sign_in_code_reused";
/// The refusal of a code, verifier or redirect that fails its local check.
pub const SIGN_IN_MATERIAL_INVALID: &str = "sign_in_material_invalid";
/// The egress gate refused: no window is Open.
pub const WINDOW_CLOSED: &str = "window_closed";

/// A stored owner sign-in. The pair is durable either way; its aftercare
/// (the migration cleanup behind the write) either completed or is left to
/// the vault's next open, which runs it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerSignIn {
    Complete,
    AftercarePending,
    /// A repeat `setup` with the material of a sign-in this process stored
    /// (M02-T13): nothing was sent to Tesla and the stored pair is the one
    /// that exchange wrote, so `setup` only runs its car discovery again.
    Resumed,
}

impl OwnerSignIn {
    /// The fixed report value: `complete`, `pending` or, for a resumed
    /// sign-in whose write happened earlier, `unchanged`.
    pub fn aftercare(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::AftercarePending => "pending",
            Self::Resumed => "unchanged",
        }
    }

    /// The fixed report value: `exchanged` or `resumed`.
    pub fn sign_in(self) -> &'static str {
        match self {
            Self::Complete | Self::AftercarePending => "exchanged",
            Self::Resumed => "resumed",
        }
    }
}

/// The same product token as the refresh request (R11).
const EXCHANGE_USER_AGENT: &str = concat!("Teslatlas-Hub/", env!("CARGO_PKG_VERSION"));

fn failure(error: SignInError) -> CredentialCommandError { CredentialStoreError::SignIn(error).into() }

/// The request body: exactly these five members (public client, PKCE).
#[derive(Serialize)]
struct ExchangeRequest<'a> {
    grant_type: &'static str,
    client_id: &'static str,
    code: &'a str,
    code_verifier: &'a str,
    redirect_uri: &'static str,
}

/// RFC 7636 section 4.1: 43 to 128 unreserved characters.
fn verifier_is_valid(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
}

/// RFC 6749 appendix A.11 VSCHAR without the space, so the code cannot carry
/// a separator or a control character.
fn code_is_valid(code: &str) -> bool {
    (1..=MAX_OWNER_CODE_BYTES).contains(&code.len()) && code.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn check_material(code: &str, code_verifier: &str, redirect_uri: &str) -> Result<(), CredentialCommandError> {
    if redirect_uri != OWNER_REDIRECT_URI || !verifier_is_valid(code_verifier) || !code_is_valid(code) {
        return Err(CredentialCommandError::new(SIGN_IN_MATERIAL_INVALID));
    }
    Ok(())
}

/// The digests of the codes this process has used. Only a digest is kept.
static USED_CODES: Mutex<VecDeque<[u8; 32]>> = Mutex::new(VecDeque::new());

fn code_digest(code: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"teslatlas-hub/owner-code/v1\0");
    hash.update(code.as_bytes());
    hash.finalize().into()
}

/// Records `code` as used, or refuses it when it already was.
fn spend_code(code: &str) -> Result<(), CredentialCommandError> {
    let digest = code_digest(code);
    let mut used = USED_CODES.lock().map_err(|_| CredentialCommandError::new(SIGN_IN_CODE_REUSED))?;
    if used.contains(&digest) { return Err(CredentialCommandError::new(SIGN_IN_CODE_REUSED)); }
    if used.len() >= USED_CODES_KEPT { used.pop_front(); }
    used.push_back(digest);
    Ok(())
}

/// How long a stored sign-in may be resumed by a repeat `setup` with the same
/// material: as long as a sign-in may take (the vault's 10 minutes).
const RESUME_WINDOW_S: i64 = 600;

/// The sign-ins this process stored: the digest of their code and verifier,
/// the generation the exchange wrote and when it was sent. Only digests.
static STORED_SIGN_INS: Mutex<VecDeque<([u8; 32], uuid::Uuid, i64)>> = Mutex::new(VecDeque::new());

fn material_digest(code: &str, code_verifier: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"teslatlas-hub/owner-sign-in/v1\0");
    hash.update(code.as_bytes());
    hash.update(b"\0");
    hash.update(code_verifier.as_bytes());
    hash.finalize().into()
}

fn record_stored_sign_in(code: &str, code_verifier: &str, generation: uuid::Uuid, sent_at_s: i64) {
    let Ok(mut stored) = STORED_SIGN_INS.lock() else { return };
    if stored.len() >= USED_CODES_KEPT { stored.pop_front(); }
    stored.push_back((material_digest(code, code_verifier), generation, sent_at_s));
}

/// Whether this material belongs to a sign-in this process stored less than
/// [`RESUME_WINDOW_S`] ago whose pair is still the stored owner pair.
fn resumable(entry: &CredentialVaultEntry, code: &str, code_verifier: &str) -> Result<bool, CredentialCommandError> {
    let digest = material_digest(code, code_verifier);
    let now = now_s()?;
    let recorded = STORED_SIGN_INS.lock().ok().and_then(|stored| stored.iter().rev()
        .find(|(candidate, _, at)| *candidate == digest && (0..=RESUME_WINDOW_S).contains(&(now - at)))
        .map(|(_, generation, _)| *generation));
    let Some(generation) = recorded else { return Ok(false) };
    Ok(entry.vault().stored_generation(CredentialSlot::OwnerApi).ok().flatten() == Some(generation))
}

/// `setup`'s sign-in (M02-T13): the material of a sign-in this process
/// stored, with that sign-in's pair still in place, resumes it with no
/// request to Tesla, so a failed car discovery can be finished by repeating
/// `setup` without a second Tesla login. Any other material is exchanged by
/// [`exchange_owner_code`], which refuses a used code as before.
pub async fn sign_in_or_resume_owner_code(entry: &CredentialVaultEntry, code: &str, code_verifier: &str, redirect_uri: &str) -> Result<OwnerSignIn, CredentialCommandError> {
    check_material(code, code_verifier, redirect_uri)?;
    if resumable(entry, code, code_verifier)? { return Ok(OwnerSignIn::Resumed); }
    exchange_owner_code(entry, code, code_verifier, redirect_uri).await
}

/// [`sign_in_or_resume_owner_code`] against a loopback stub token endpoint,
/// for the binary's tests.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub async fn sign_in_or_resume_owner_code_with_stub_for_test(entry: &CredentialVaultEntry, code: &str, code_verifier: &str, redirect_uri: &str, stub: &str) -> Result<OwnerSignIn, CredentialCommandError> {
    check_material(code, code_verifier, redirect_uri)?;
    if resumable(entry, code, code_verifier)? { return Ok(OwnerSignIn::Resumed); }
    exchange_owner_code_with_stub_for_test(entry, code, code_verifier, redirect_uri, stub).await
}

/// The request body in one wiping buffer of its exact length.
fn request_body(code: &str, code_verifier: &str) -> Result<Zeroizing<Vec<u8>>, CredentialCommandError> {
    let request = ExchangeRequest {
        grant_type: "authorization_code", client_id: OWNER_CLIENT_ID, code, code_verifier, redirect_uri: OWNER_REDIRECT_URI,
    };
    let mut body = Zeroizing::new(Vec::with_capacity(512));
    serde_json::to_writer(&mut *body, &request).map_err(|_| CredentialCommandError::new(SIGN_IN_MATERIAL_INVALID))?;
    Ok(body)
}

/// What came back, before validation.
enum Answer {
    Complete(reqwest::StatusCode, Zeroizing<Vec<u8>>),
    TooLarge,
}

async fn send(client: &Client, endpoint: Url, mut body: Zeroizing<Vec<u8>>) -> Result<Answer, reqwest::Error> {
    let request = client.post(endpoint)
        .header(USER_AGENT, EXCHANGE_USER_AGENT)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .body(std::mem::take(&mut *body));
    #[cfg(test)]
    crate::api::test_egress_guard::refuse_production_request(&request);
    let mut response = request.send().await?;
    let status = response.status();
    if response.content_length().is_some_and(|length| length > ANSWER_CAP as u64) { return Ok(Answer::TooLarge); }
    let mut bytes = Zeroizing::new(Vec::with_capacity(ANSWER_CAP));
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > ANSWER_CAP - bytes.len() { return Ok(Answer::TooLarge); }
        bytes.extend_from_slice(&chunk);
        if let Ok(mut chunk) = chunk.try_into_mut() { chunk.as_mut().zeroize(); }
    }
    Ok(Answer::Complete(status, bytes))
}

/// The fixed refusal of a non-2xx answer: the issuer's `error` member when it
/// is one of the allowlisted RFC 6749 codes, else `other`. No text is kept.
fn refused(bytes: &[u8]) -> CredentialCommandError {
    #[derive(serde::Deserialize)]
    struct ErrorAnswer { error: Option<String> }
    let code = serde_json::from_slice::<ErrorAnswer>(bytes).ok().and_then(|answer| answer.error);
    match code {
        Some(code) => failure(SignInError::Refused(SignInRefusalCode::from_issuer_code(&code))),
        None => failure(SignInError::AnswerInvalid),
    }
}

/// The grammar the refresh path loads a stored owner pair with (R46): an
/// access token of base64 and JWT characters, and a refresh token of visible
/// ASCII. A pair the loader would refuse is never stored.
fn owner_pair_grammar(access: &str, refresh: &str) -> bool {
    access.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte)) && access.bytes().any(|byte| byte != b'=')
        && !refresh.is_empty() && refresh.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

fn now_s() -> Result<i64, CredentialCommandError> {
    SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|value| i64::try_from(value.as_secs()).ok())
        .ok_or_else(|| failure(SignInError::Unavailable))
}

/// Exchanges the owner's authorisation code at Tesla's owner token endpoint
/// and stores the answer as the Hub's own pair. See the module notes for the
/// order of checks; a refusal at any step leaves the store unchanged.
pub async fn exchange_owner_code(entry: &CredentialVaultEntry, code: &str, code_verifier: &str, redirect_uri: &str) -> Result<OwnerSignIn, CredentialCommandError> {
    let endpoint = Url::parse(OWNER_TOKEN_ENDPOINT).map_err(|_| failure(SignInError::SettingsInvalid))?;
    exchange_at(entry, code, code_verifier, redirect_uri, endpoint, false).await
}

/// The exchange against a loopback stub token endpoint, for the binary's
/// tests. It still asks the process-wide egress gate, so it sends nothing
/// unless a window is Open. No real endpoint is accepted.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub async fn exchange_owner_code_with_stub_for_test(entry: &CredentialVaultEntry, code: &str, code_verifier: &str, redirect_uri: &str, stub: &str) -> Result<OwnerSignIn, CredentialCommandError> {
    let endpoint = stub_endpoint(stub)?;
    exchange_at(entry, code, code_verifier, redirect_uri, endpoint, true).await
}

#[cfg(any(test, debug_assertions))]
fn stub_endpoint(stub: &str) -> Result<Url, CredentialCommandError> {
    let url = Url::parse(stub).map_err(|_| failure(SignInError::SettingsInvalid))?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if !loopback || url.scheme() != "http" { return Err(failure(SignInError::SettingsInvalid)); }
    Ok(url)
}

async fn exchange_at(entry: &CredentialVaultEntry, code: &str, code_verifier: &str, redirect_uri: &str, endpoint: Url, loopback: bool) -> Result<OwnerSignIn, CredentialCommandError> {
    check_material(code, code_verifier, redirect_uri)?;
    spend_code(code)?;
    let body = request_body(code, code_verifier)?;
    crate::crypto::install_default_provider();
    let client = crate::legacy_auth::build_auth_client(loopback).map_err(|_| failure(SignInError::Unavailable))?;
    // The gate is the last check before the send: a refusal opens no
    // connection and is never retried.
    let permit = egress::ask(endpoint.as_str(), Provider::Legacy, PermitKind::TokenExchange)
        .map_err(|_| CredentialCommandError::new(WINDOW_CLOSED))?;
    let sent_at_s = now_s()?;
    let answer = match permit.bound(send(&client, endpoint, body)).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(error)) if error.is_connect() || error.is_builder() => return Err(failure(SignInError::Unavailable)),
        // Sent and not answered in full, or cut at the window's deadline: the
        // code may be spent at Tesla and nothing is stored.
        Ok(Err(_)) | Err(_) => return Err(failure(SignInError::AnswerInvalid)),
    };
    let bytes = match answer {
        Answer::Complete(status, bytes) if status.is_success() => bytes,
        Answer::Complete(_, bytes) => return Err(refused(&bytes)),
        Answer::TooLarge => return Err(failure(SignInError::AnswerInvalid)),
    };
    let tokens = token_answer(&bytes)?;
    drop(bytes);
    if !(60..=31_536_000).contains(&tokens.expires_in)
        || !owner_pair_grammar(tokens.access.as_str(), tokens.refresh.as_str())
    {
        return Err(failure(SignInError::AnswerInvalid));
    }
    // The permit still holds the window. It is checked once more here, and
    // then again, through the window's write fence, before every mutation
    // of the store below (the binding, a key's publication, the pair's
    // commit, the aftercare and any clean-up): once the window has lapsed
    // the permit nothing more is written, and the window is handed back
    // within its grace whatever this write is doing (M02-T13 round 2).
    permit.check().map_err(|_| CredentialCommandError::new(WINDOW_CLOSED))?;
    #[cfg(test)]
    tests::persistence_pause::reached();
    let live = || permit.check().is_ok();
    let authority = WriteAuthority::of(&live);
    // The local binding that proves this grant came from the Hub's own
    // exchange; begun only now, so a failed exchange leaves none behind.
    // Each step below that refuses while the authority holds leaves no
    // binding or attempt-created key behind; the vault removes them (or
    // reports a committed but unproven write as `sign_in_stored_unconfirmed`).
    // Once the authority has ended a refused attempt's binding expires
    // unused.
    let session = entry.vault().begin_sign_in_under(CredentialSlot::OwnerApi, authority)?;
    let sign_in_id = session.id();
    let grant = RedeemedGrant::from_validated_redemption_at(session, Zeroizing::new(tokens.access.as_str().to_owned()),
        Zeroizing::new(tokens.refresh.as_str().to_owned()), tokens.expires_in, None, None, sent_at_s);
    drop(tokens);
    let grant = grant.inspect_err(|_| entry.vault().abandon_sign_in_under(sign_in_id, authority))?;
    let stored = entry.vault().store_hub_grant_under(grant, authority)?;
    // The generation this exchange wrote, so the same material may later
    // resume this sign-in while that pair is still the stored one.
    if let Ok(Some(generation)) = entry.vault().stored_generation(CredentialSlot::OwnerApi) {
        record_stored_sign_in(code, code_verifier, generation, sent_at_s);
    }
    tracing::info!(slot = "owner_api", origin = "hub_login", refresh_owner = "hub", provenance = "own_legacy_exchange", "credential write");
    // The pair is stored: an aftercare failure, or a window that has ended,
    // no longer makes the sign-in a refusal. The vault's next open runs the
    // aftercare again.
    let aftercare = entry.vault().finish_hub_grant_aftercare_under(stored, authority);
    // Nothing more is written: the window may now finish closing.
    drop(permit);
    match aftercare {
        Ok(()) => Ok(OwnerSignIn::Complete),
        Err(error) => {
            tracing::warn!(code = error.code(), "credential aftercare deferred after a stored sign-in");
            Ok(OwnerSignIn::AftercarePending)
        }
    }
}

#[cfg(test)]
async fn exchange_at_stub_for_test(entry: &CredentialVaultEntry, code: &str, code_verifier: &str, redirect_uri: &str, stub: &str) -> Result<OwnerSignIn, CredentialCommandError> {
    exchange_at(entry, code, code_verifier, redirect_uri, stub_endpoint(stub)?, true).await
}

#[cfg(test)]
#[path = "exchange_tests.rs"]
mod tests;
