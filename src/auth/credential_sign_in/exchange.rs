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
use super::{CredentialCommandError, SecretBuffer, SecretText, owner_client::{OWNER_CLIENT_ID, OWNER_REDIRECT_URI, OWNER_TOKEN_ENDPOINT}, protocol::{SignInSettings, parameter, token_answer}};
use super::super::{credential_envelope::CredentialSlot, credential_vault::{CredentialStoreError, RedeemedGrant, SignInError, SignInRefusalCode, SignInSession, WriteAuthority}};

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
const EXCHANGE_USER_AGENT: &str = concat!("Teslatlas-Hub/", env!("CARGO_PKG_VERSION")); // rename:keep

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

/// Whose code a used-code record is (M09-T19 ruling 5): the issuer's token
/// endpoint, the application and the credential slot. The same opaque text
/// for another application, region or slot is another code.
struct CodeNamespace<'a> { slot: CredentialSlot, issuer: &'a str, client_id: &'a str }

fn code_digest(namespace: &CodeNamespace<'_>, code: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"teslatlas-hub/sign-in-code/v2\0"); // rename:keep
    hash.update(match namespace.slot { CredentialSlot::OwnerApi => b"owner_api\0".as_slice(), CredentialSlot::FleetApi => b"fleet_api\0".as_slice() });
    hash.update(namespace.issuer.as_bytes());
    hash.update(b"\0");
    hash.update(namespace.client_id.as_bytes());
    hash.update(b"\0");
    hash.update(code.as_bytes());
    hash.finalize().into()
}

/// Records `code` as used, or refuses it when it already was.
fn spend_code(namespace: &CodeNamespace<'_>, code: &str) -> Result<(), CredentialCommandError> {
    let digest = code_digest(namespace, code);
    let mut used = USED_CODES.lock().map_err(|_| CredentialCommandError::new(SIGN_IN_CODE_REUSED))?;
    if used.contains(&digest) { return Err(CredentialCommandError::new(SIGN_IN_CODE_REUSED)); }
    if used.len() >= USED_CODES_KEPT { used.pop_front(); }
    used.push_back(digest);
    Ok(())
}

/// Forgets a code [`spend_code`] recorded whose request is proven never to
/// have been sent (M09-T19 ruling 5), so the same code may be tried again.
fn release_code(namespace: &CodeNamespace<'_>, code: &str) {
    let digest = code_digest(namespace, code);
    if let Ok(mut used) = USED_CODES.lock() && let Some(index) = used.iter().rposition(|candidate| *candidate == digest) {
        used.remove(index);
    }
}

/// How long a stored sign-in may be resumed by a repeat `setup` with the same
/// material: as long as a sign-in may take (the vault's 10 minutes).
const RESUME_WINDOW_S: i64 = 600;

/// The sign-ins this process stored: the digest of their code and verifier,
/// the generation the exchange wrote and when it was sent. Only digests.
static STORED_SIGN_INS: Mutex<VecDeque<([u8; 32], uuid::Uuid, i64)>> = Mutex::new(VecDeque::new());

fn material_digest(code: &str, code_verifier: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"teslatlas-hub/owner-sign-in/v1\0"); // rename:keep
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
    // Sized in full before anything is written (M09-T19 ruling 8): a JSON
    // escape at most doubles a checked code or verifier, so the buffer never
    // grows and no freed block keeps a copy.
    let mut body = Zeroizing::new(Vec::with_capacity(256 + 2 * (code.len() + code_verifier.len())));
    serde_json::to_writer(&mut *body, &request).map_err(|_| CredentialCommandError::new(SIGN_IN_MATERIAL_INVALID))?;
    Ok(body)
}

/// What came back, before validation.
enum Answer {
    Complete(reqwest::StatusCode, Zeroizing<Vec<u8>>),
    TooLarge,
}

async fn send(client: &Client, endpoint: Url, body: reqwest::Body, content_type: &'static str) -> Result<Answer, reqwest::Error> {
    let request = client.post(endpoint)
        .header(USER_AGENT, EXCHANGE_USER_AGENT)
        .header(CONTENT_TYPE, content_type)
        .header(ACCEPT, "application/json")
        .body(body);
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
    spend_code(&CodeNamespace { slot: CredentialSlot::OwnerApi, issuer: endpoint.as_str(), client_id: OWNER_CLIENT_ID }, code)?;
    let mut body = request_body(code, code_verifier)?;
    crate::crypto::install_default_provider();
    let client = crate::legacy_auth::build_auth_client(loopback).map_err(|_| failure(SignInError::Unavailable))?;
    // The gate is the last check before the send: a refusal opens no
    // connection and is never retried.
    let permit = egress::ask(endpoint.as_str(), Provider::Legacy, PermitKind::TokenExchange)
        .map_err(|_| CredentialCommandError::new(WINDOW_CLOSED))?;
    let sent_at_s = now_s()?;
    let answer = match permit.bound(send(&client, endpoint, reqwest::Body::from(std::mem::take(&mut *body)), "application/json")).await {
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

/// The refusal of a Fleet redeem that does not finish the sign-in its begin
/// started (another application, region or redirect, or no begin at all).
pub const SIGN_IN_BINDING_INVALID: &str = "sign_in_binding_invalid";

/// The refusal of a Fleet redeem with no begun sign-in to finish.
pub fn fleet_sign_in_not_begun() -> CredentialCommandError { CredentialCommandError::new(SIGN_IN_BINDING_INVALID) }

/// The first step of a Fleet sign-in with the owner's own Tesla developer
/// application (M09-T09; design T-B2): the binding the vault recorded and
/// the application's public values. The service holds it until the redeem;
/// it holds no client secret, and its state and grant secret are wiped when
/// it is dropped. It expires with the vault's binding (10 minutes).
pub struct FleetSignInBinding {
    settings: SignInSettings,
    session: SignInSession,
}

impl FleetSignInBinding {
    /// The binding's identifier (not a secret): it names this attempt.
    pub fn id(&self) -> uuid::Uuid { self.session.id() }

    /// When the binding expires, in Unix seconds.
    pub fn expires_at_s(&self) -> i64 { self.session.expires_at_ms().div_euclid(1000) }

    /// The regional authorise address, with the Fleet scope and this
    /// binding's state, that the owner opens to sign in with Tesla.
    pub fn authorization_url(&self) -> SecretText { self.settings.authorization_url(&self.session) }
}

impl std::fmt::Debug for FleetSignInBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FleetSignInBinding").field("id", &self.session.id()).finish_non_exhaustive()
    }
}

/// `setup-fleet begin` (M09-T09): reads exactly `clientId`, `region` and
/// `redirectUri`, then records a sign-in binding for the Fleet slot. The
/// binding is written only while the window's egress gate grants Fleet a
/// permit (`window_closed` otherwise); nothing is sent to Tesla.
pub fn begin_fleet_sign_in(entry: &CredentialVaultEntry, line: &[u8]) -> Result<FleetSignInBinding, CredentialCommandError> {
    fleet_begin_at(entry, line, None)
}

/// [`begin_fleet_sign_in`] asking the gate for the loopback stub token
/// endpoint the redeem will use, for the binary's tests.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn begin_fleet_sign_in_with_stub_for_test(entry: &CredentialVaultEntry, line: &[u8], stub: &str) -> Result<FleetSignInBinding, CredentialCommandError> {
    let endpoint = stub_endpoint(stub)?;
    fleet_begin_at(entry, line, Some(&endpoint))
}

fn fleet_begin_at(entry: &CredentialVaultEntry, line: &[u8], stub: Option<&Url>) -> Result<FleetSignInBinding, CredentialCommandError> {
    let settings = SignInSettings::fleet_application(line)?;
    let endpoint = stub.unwrap_or(settings.token_endpoint()).as_str();
    let permit = egress::ask(endpoint, Provider::Fleet, PermitKind::Request)
        .map_err(|_| CredentialCommandError::new(WINDOW_CLOSED))?;
    let live = || permit.check().is_ok();
    let session = entry.vault().begin_sign_in_under(CredentialSlot::FleetApi, WriteAuthority::of(&live))?;
    Ok(FleetSignInBinding { settings, session })
}

/// Removes the binding of a Fleet sign-in that will not be redeemed (a new
/// begin replaces it); best effort; an unremoved binding expires unused.
pub fn abandon_fleet_sign_in(entry: &CredentialVaultEntry, binding: FleetSignInBinding) {
    entry.vault().abandon_sign_in_under(binding.session.id(), WriteAuthority::UNBOUNDED);
}

/// The Fleet form body: exactly these six members, encoded into a wiping
/// buffer that the transport owns and wipes once sent. The client secret is
/// copied only here.
fn fleet_request_body(settings: &SignInSettings, code: &str) -> Result<reqwest::Body, CredentialCommandError> {
    let (body, _bounded) = fleet_request_form(settings, code)?;
    Ok(reqwest::Body::from(axum::body::Bytes::from_owner(body)))
}

/// The Fleet form in its wiping buffer, and the full size it was allocated
/// at before anything was copied in.
fn fleet_request_form(settings: &SignInSettings, code: &str) -> Result<(SecretBuffer, usize), CredentialCommandError> {
    let (Some(secret), Some(region)) = (settings.client_secret(), settings.region()) else {
        return Err(failure(SignInError::SettingsInvalid));
    };
    // At most three bytes per encoded byte: allocated in full before the
    // secret is copied in, so the buffer never grows (M09-T19 ruling 8).
    let bounded = 128 + 3 * (settings.client_id().len() + secret.len() + code.len() + settings.redirect_uri().len() + region.base_url().len());
    let mut body = SecretBuffer::new("request_body", bounded);
    parameter(&mut body, "grant_type", "authorization_code");
    parameter(&mut body, "client_id", settings.client_id());
    parameter(&mut body, "client_secret", secret);
    parameter(&mut body, "code", code);
    parameter(&mut body, "audience", region.base_url().trim_end_matches('/'));
    parameter(&mut body, "redirect_uri", settings.redirect_uri());
    Ok((body, bounded))
}

/// `setup-fleet` (M09-T09, M09-T19): redeems the code of the sign-in held
/// in `held`, the newest one begun. Reads exactly `code`, `redirectUri`,
/// `clientId`, `clientSecret`, `region` and `binding`; the binding must be the
/// held one's and the application, region and redirect its begin's. Order, so
/// that a refusal opens no connection and writes nothing:
/// 1. the input and the binding are checked locally; a refusal here keeps the
///    held sign-in and leaves the code unused;
/// 2. the code is recorded as used in this process, for this issuer,
///    application and slot;
/// 3. the request is built and the window's egress gate is asked for a Fleet
///    token-exchange permit (`window_closed` unless a window is Open),
///    immediately before the send; a failure up to and including a refused
///    connection is proven to have sent nothing, so the code is forgotten
///    again and the held sign-in kept;
/// 4. one form POST to the regional Fleet token endpoint, bounded by the
///    permit, with no redirect followed and the answer read to at most
///    [`ANSWER_CAP`] bytes; from here the sign-in is used up whatever the
///    outcome, and the client secret is wiped once the body is built;
/// 5. the answer's tokens must have the grammar and the claims and scopes
///    the Fleet collector needs (`vehicle_device_data` and
///    `vehicle_location`); a refusal here keeps the stored login as it was;
/// 6. only then is it stored, through the vault's one grant path
///    (`store_hub_grant`): origin `fleet_oauth`, refresh owner the Hub, the
///    client id and region, under the permit's write fence.
pub async fn exchange_fleet_code(entry: &CredentialVaultEntry, held: &mut Option<FleetSignInBinding>, line: &[u8]) -> Result<OwnerSignIn, CredentialCommandError> {
    fleet_exchange_at(entry, held, line, None).await
}

/// [`exchange_fleet_code`] against a loopback stub token endpoint, for the
/// binary's tests. It still asks the process-wide egress gate.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub async fn exchange_fleet_code_with_stub_for_test(entry: &CredentialVaultEntry, held: &mut Option<FleetSignInBinding>, line: &[u8], stub: &str) -> Result<OwnerSignIn, CredentialCommandError> {
    let endpoint = stub_endpoint(stub)?;
    fleet_exchange_at(entry, held, line, Some(endpoint)).await
}

/// The refusal of a Fleet grant whose access token's claims cannot be read.
pub const FLEET_TOKEN_CLAIMS_INVALID: &str = "fleet_token_claims_invalid";
/// The refusals of a Fleet grant without a scope the collector needs.
pub const FLEET_SCOPE_MISSING_VEHICLE_DEVICE_DATA: &str = "fleet_scope_missing_vehicle_device_data";
pub const FLEET_SCOPE_MISSING_VEHICLE_LOCATION: &str = "fleet_scope_missing_vehicle_location";
pub const FLEET_SCOPES_MISSING: &str = "fleet_scopes_missing";

/// The scope check of a new Fleet grant (M09-T19 ruling 6), before it may
/// replace a login: the same claims the collector's start reads.
fn fleet_grant_scopes(access: &str) -> Result<(), CredentialCommandError> {
    use crate::auth::fleet_credentials::{FleetGrantScopeRefusal, check_fleet_grant_scopes};
    check_fleet_grant_scopes(access).map_err(|refusal| CredentialCommandError::new(match refusal {
        FleetGrantScopeRefusal::ClaimsInvalid => FLEET_TOKEN_CLAIMS_INVALID,
        FleetGrantScopeRefusal::Missing { vehicle_device_data: true, vehicle_location: true } => FLEET_SCOPES_MISSING,
        FleetGrantScopeRefusal::Missing { vehicle_device_data: true, .. } => FLEET_SCOPE_MISSING_VEHICLE_DEVICE_DATA,
        FleetGrantScopeRefusal::Missing { .. } => FLEET_SCOPE_MISSING_VEHICLE_LOCATION,
    }))
}

async fn fleet_exchange_at(entry: &CredentialVaultEntry, held: &mut Option<FleetSignInBinding>, line: &[u8], stub: Option<Url>) -> Result<OwnerSignIn, CredentialCommandError> {
    let (settings, code, binding_id) = SignInSettings::fleet_redeem(line)?;
    let Some(begun) = held.as_ref() else { return Err(fleet_sign_in_not_begun()) };
    // Only the newest unused binding, for the same application, is redeemed.
    if begun.id() != binding_id || !begun.settings.same_application(&settings) {
        return Err(CredentialCommandError::new(SIGN_IN_BINDING_INVALID));
    }
    if !code_is_valid(code.as_str()) { return Err(CredentialCommandError::new(SIGN_IN_MATERIAL_INVALID)); }
    // A binding past its expiry could not store the answer: the code is not
    // sent, so it is not spent at Tesla either; the binding is gone.
    if begun.session.expires_at_ms() <= now_s()?.saturating_mul(1000) {
        *held = None;
        return Err(failure(SignInError::Expired));
    }
    let loopback = stub.is_some();
    let endpoint = stub.unwrap_or_else(|| settings.token_endpoint().clone());
    let issuer = endpoint.as_str().to_owned();
    let (client_id, region) = (settings.client_id().to_owned(), settings.region());
    let namespace = CodeNamespace { slot: CredentialSlot::FleetApi, issuer: &issuer, client_id: &client_id };
    spend_code(&namespace, code.as_str())?;
    // Until the send, a failure is proven to have sent nothing: the code is
    // forgotten again and the held sign-in stays for another try.
    let unsent = |error: CredentialCommandError| { release_code(&namespace, code.as_str()); error };
    let body = fleet_request_body(&settings, code.as_str()).map_err(unsent)?;
    // Exchange only: the client secret is not needed again and is wiped now.
    drop(settings);
    crate::crypto::install_default_provider();
    let client = crate::legacy_auth::build_auth_client(loopback).map_err(|_| unsent(failure(SignInError::Unavailable)))?;
    let permit = egress::ask(endpoint.as_str(), Provider::Fleet, PermitKind::TokenExchange)
        .map_err(|_| unsent(CredentialCommandError::new(WINDOW_CLOSED)))?;
    let sent_at_s = now_s().map_err(unsent)?;
    let answer = permit.bound(send(&client, endpoint, body, "application/x-www-form-urlencoded")).await;
    // A request that never left (no connection, or never built) sent nothing.
    if let Ok(Err(error)) = &answer && (error.is_connect() || error.is_builder()) {
        return Err(unsent(failure(SignInError::Unavailable)));
    }
    // Sent, or possibly sent: the sign-in is used up and the code stays spent.
    let FleetSignInBinding { settings: _, session } = held.take().expect("the held sign-in was checked above");
    drop(code);
    let answer = match answer {
        Ok(Ok(answer)) => answer,
        Ok(Err(_)) | Err(_) => return Err(failure(SignInError::AnswerInvalid)),
    };
    let bytes = match answer {
        Answer::Complete(status, bytes) if status.is_success() => bytes,
        Answer::Complete(_, bytes) => return Err(refused(&bytes)),
        Answer::TooLarge => return Err(failure(SignInError::AnswerInvalid)),
    };
    let tokens = token_answer(&bytes)?;
    drop(bytes);
    // The grammar the Fleet loader reads a stored pair with: a pair it
    // would refuse is never stored.
    if !(60..=31_536_000).contains(&tokens.expires_in)
        || crate::fleet_api::FleetAccessToken::new(tokens.access.as_str()).is_err()
        || crate::fleet_api::FleetRefreshToken::new(tokens.refresh.as_str()).is_err()
    {
        return Err(failure(SignInError::AnswerInvalid));
    }
    // The claims and scopes the collector needs, before anything replaces
    // the stored login (M09-T19 ruling 6).
    fleet_grant_scopes(tokens.access.as_str())?;
    permit.check().map_err(|_| CredentialCommandError::new(WINDOW_CLOSED))?;
    let live = || permit.check().is_ok();
    let authority = WriteAuthority::of(&live);
    let sign_in_id = session.id();
    let grant = RedeemedGrant::from_validated_redemption_at(session, Zeroizing::new(tokens.access.as_str().to_owned()),
        Zeroizing::new(tokens.refresh.as_str().to_owned()), tokens.expires_in, Some(client_id), region, sent_at_s);
    drop(tokens);
    let grant = grant.inspect_err(|_| entry.vault().abandon_sign_in_under(sign_in_id, authority))?;
    let stored = entry.vault().store_hub_grant_under(grant, authority)?;
    tracing::info!(slot = "fleet_api", origin = "fleet_oauth", refresh_owner = "hub", provenance = "hub_grant", "credential write");
    let aftercare = entry.vault().finish_hub_grant_aftercare_under(stored, authority);
    drop(permit);
    match aftercare {
        Ok(()) => Ok(OwnerSignIn::Complete),
        Err(error) => {
            tracing::warn!(code = error.code(), "credential aftercare deferred after a stored Fleet sign-in");
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
