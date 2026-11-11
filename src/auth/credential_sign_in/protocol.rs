// SPDX-License-Identifier: AGPL-3.0-only

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::de::{self, Deserializer as _, MapAccess, Visitor};
use sha2::{Digest, Sha256};
use std::{fmt, time::{Duration, SystemTime, UNIX_EPOCH}};
use zeroize::{Zeroize, Zeroizing};
use crate::{CredentialVaultEntry, fleet_api::{FleetClientId, FleetRegion, WireSecret}};
use super::observe::{SecretBuffer, SecretText};
use super::{CredentialCommandError, super::{credential_envelope::CredentialSlot, credential_vault::{CredentialStoreError, RedeemedGrant, SignInError, SignInRefusalCode, SignInSession}}};

pub const FLEET_SIGN_IN_SCOPE: &str = "openid offline_access vehicle_device_data vehicle_location";
const ANSWER_CAP: usize = 65_536;

fn failure(error: SignInError) -> CredentialCommandError { CredentialStoreError::SignIn(error).into() }
fn wiped_copy(text: &str) -> Zeroizing<String> { Zeroizing::new(text.to_owned()) }

/// Encodes into the wiping destination without an intermediate form string.
fn parameter(body: &mut Vec<u8>, key: &str, value: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    if !body.is_empty() { body.push(b'&'); }
    body.extend_from_slice(key.as_bytes());
    body.push(b'=');
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => body.push(byte),
            b' ' => body.push(b'+'),
            _ => { body.push(b'%'); body.push(HEX[(byte >> 4) as usize]); body.push(HEX[(byte & 15) as usize]); }
        }
    }
}

fn encode_secret(secret: &[u8; 32], kind: &'static str) -> SecretText {
    let mut output = SecretText::new(kind, 43);
    URL_SAFE_NO_PAD.encode_string(secret, output.buffer_mut());
    output
}
pub fn pkce_verifier(secret: &[u8; 32]) -> SecretText { encode_secret(secret, "verifier") }

pub fn pkce_challenge(verifier: &str) -> SecretText {
    let mut digest = Sha256::digest(verifier.as_bytes());
    let mut output = SecretText::new("challenge", 43);
    URL_SAFE_NO_PAD.encode_string(&digest, output.buffer_mut());
    digest.as_mut_slice().zeroize();
    super::note_secret_drop(&super::current_secret_observer(), "challenge_digest", digest.len(), digest.len(), digest.iter().all(|byte| *byte == 0));
    output
}

/// Public configuration contains no grant secret or token.
pub struct SignInSettings {
    slot: CredentialSlot,
    authorization: url::Url,
    token: url::Url,
    client_id: String,
    redirect_uri: String,
    scope: String,
    region: Option<FleetRegion>,
    client_secret: Option<WireSecret>,
}

fn endpoint(text: &str, loopback: bool) -> Result<url::Url, CredentialCommandError> {
    let url = url::Url::parse(text).map_err(|_| failure(SignInError::SettingsInvalid))?;
    let local = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some()
        || if loopback { !local || url.scheme() != "http" } else { url.scheme() != "https" }
    { return Err(failure(SignInError::SettingsInvalid)); }
    Ok(url)
}

fn redirect_is_valid(text: &str) -> bool {
    url::Url::parse(text).is_ok_and(|url| !url.scheme().is_empty() && url.username().is_empty()
        && url.password().is_none() && url.query().is_none() && url.fragment().is_none())
}

impl SignInSettings {
    pub fn fleet(line: &[u8]) -> Result<Self, CredentialCommandError> {
        let values = secret_object(line, &["clientId", "region", "redirectUri", "clientSecret"])
            .map_err(|_| failure(SignInError::SettingsInvalid))?;
        let [client_id, region, redirect_uri, client_secret]: [WireSecret; 4] = values.try_into().map_err(|_| failure(SignInError::SettingsInvalid))?;
        FleetClientId::parse(client_id.as_str()).map_err(|_| failure(SignInError::SettingsInvalid))?;
        let region = FleetRegion::from_storage_code(region.as_str()).map_err(|_| failure(SignInError::SettingsInvalid))?;
        if client_secret.as_str().is_empty() || !redirect_is_valid(redirect_uri.as_str()) { return Err(failure(SignInError::SettingsInvalid)); }
        let authorization = if region == FleetRegion::China { "https://auth.tesla.cn/oauth2/v3/authorize" } else { "https://auth.tesla.com/oauth2/v3/authorize" };
        Ok(Self { slot: CredentialSlot::FleetApi, authorization: endpoint(authorization, false)?, token: endpoint(region.auth_token_url(), false)?,
            client_id: client_id.as_str().to_owned(), redirect_uri: redirect_uri.as_str().to_owned(), scope: FLEET_SIGN_IN_SCOPE.to_owned(),
            region: Some(region), client_secret: Some(client_secret) })
    }

    /// Explicit synthetic issuer configuration. No real endpoint is accepted.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn stub(authorization: &str, token: &str, redirect_uri: &str) -> Result<Self, CredentialCommandError> {
        if !redirect_is_valid(redirect_uri) { return Err(failure(SignInError::SettingsInvalid)); }
        Ok(Self { slot: CredentialSlot::OwnerApi, authorization: endpoint(authorization, true)?, token: endpoint(token, true)?,
            client_id: "client-0001".to_owned(), redirect_uri: redirect_uri.to_owned(), scope: "openid offline_access".to_owned(),
            region: None, client_secret: None })
    }

    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn with_stub_endpoints(mut self, authorization: &str, token: &str) -> Result<Self, CredentialCommandError> {
        self.authorization = endpoint(authorization, true)?;
        self.token = endpoint(token, true)?;
        Ok(self)
    }
}

fn secret_object(bytes: &[u8], keys: &[&str]) -> Result<Vec<WireSecret>, ()> {
    struct Object<'a>(&'a [&'a str]);
    impl<'de> Visitor<'de> for Object<'_> {
        type Value = Vec<WireSecret>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("the exact input object") }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut values: Vec<Option<WireSecret>> = (0..self.0.len()).map(|_| None).collect();
            while let Some(key) = map.next_key::<String>()? {
                let index = self.0.iter().position(|expected| *expected == key).ok_or_else(|| de::Error::custom("unknown key"))?;
                if values[index].is_some() { return Err(de::Error::custom("duplicate key")); }
                values[index] = Some(map.next_value()?);
            }
            values.into_iter().map(|value| value.ok_or_else(|| de::Error::custom("missing key"))).collect()
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let values = decoder.deserialize_map(Object(keys)).map_err(|_| ())?;
    decoder.end().map_err(|_| ())?;
    Ok(values)
}

fn decoded_component(value: &str) -> Result<SecretText, CredentialCommandError> {
    let mut bytes = SecretBuffer::new("redirect_query", value.len());
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let digit = |byte: Option<u8>| byte.and_then(|byte| (byte as char).to_digit(16)).ok_or_else(|| failure(SignInError::AnswerInvalid));
                (digit(input.next())? * 16 + digit(input.next())?) as u8
            }
            value => value,
        });
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| failure(SignInError::AnswerInvalid))?;
    Ok(SecretText::copy("redirect_component", text))
}

pub struct SignInAttempt<'a> {
    entry: &'a CredentialVaultEntry,
    settings: SignInSettings,
    session: SignInSession,
}

impl<'a> SignInAttempt<'a> {
    pub fn begin(entry: &'a CredentialVaultEntry, settings: SignInSettings) -> Result<Self, CredentialCommandError> {
        let session = entry.vault().begin_sign_in(settings.slot)?;
        Ok(Self { entry, settings, session })
    }

    pub fn authorization_url(&self) -> SecretText {
        let mut query = SecretBuffer::new("authorization_query", 1024);
        parameter(&mut query, "response_type", "code");
        parameter(&mut query, "client_id", &self.settings.client_id);
        parameter(&mut query, "redirect_uri", &self.settings.redirect_uri);
        parameter(&mut query, "scope", &self.settings.scope);
        let state = encode_secret(self.session.state(), "state_encoding");
        parameter(&mut query, "state", &state);
        if self.settings.slot == CredentialSlot::OwnerApi {
            let verifier = pkce_verifier(self.session.grant_secret());
            let challenge = pkce_challenge(&verifier);
            parameter(&mut query, "code_challenge", &challenge);
            parameter(&mut query, "code_challenge_method", "S256");
        }
        let mut address = SecretText::copy("authorization_url", self.settings.authorization.as_str());
        address.buffer_mut().push('?');
        address.buffer_mut().push_str(std::str::from_utf8(&query).expect("ASCII form"));
        address
    }

    fn redirect_code(&self, line: &[u8]) -> Result<SecretText, CredentialCommandError> {
        let mut values = secret_object(line, &["redirectUrl"]).map_err(|_| failure(SignInError::AnswerInvalid))?;
        let address = values.pop().expect("one redirect field");
        let address = address.as_str().split('#').next().unwrap_or_default();
        let (base, query) = address.split_once('?').ok_or_else(|| failure(SignInError::AnswerInvalid))?;
        let base = url::Url::parse(base).map_err(|_| failure(SignInError::AnswerInvalid))?;
        let expected = url::Url::parse(&self.settings.redirect_uri).map_err(|_| failure(SignInError::SettingsInvalid))?;
        if !base.username().is_empty() || base.password().is_some() || base.scheme() != expected.scheme()
            || base.host() != expected.host() || base.port_or_known_default() != expected.port_or_known_default() || base.path() != expected.path()
        { return Err(failure(SignInError::AnswerInvalid)); }
        let mut state = None;
        let mut code = None;
        let mut error = None;
        for field in query.split('&') {
            let (key, value) = field.split_once('=').unwrap_or((field, ""));
            let key = decoded_component(key)?;
            let value = decoded_component(value)?;
            let target = match key.as_str() { "state" => &mut state, "code" => &mut code, "error" => &mut error, _ => continue };
            if target.is_some() { return Err(failure(SignInError::AnswerInvalid)); }
            *target = Some(value);
        }
        let state = state.ok_or_else(|| failure(SignInError::AnswerInvalid))?;
        let expected_state = encode_secret(self.session.state(), "expected_state");
        // Fixed-length comparison: every byte is visited and lengths are public.
        let mut difference = state.len() ^ expected_state.len();
        for (index, expected) in expected_state.bytes().enumerate() { difference |= usize::from(state.as_bytes().get(index).copied().unwrap_or(0) ^ expected); }
        if difference != 0 { return Err(failure(SignInError::StateMismatch)); }
        match (code, error) {
            (Some(code), None) if !code.is_empty() => Ok(code),
            (None, Some(error)) if !error.is_empty() => Err(failure(SignInError::Refused(SignInRefusalCode::from_issuer_code(&error)))),
            _ => Err(failure(SignInError::AnswerInvalid)),
        }
    }

    pub async fn redeem(self, line: &[u8], mut publish: impl FnMut(&str) -> Result<(), CredentialCommandError>) -> Result<(), CredentialCommandError> {
        let code = self.redirect_code(line)?;
        let mut body = SecretBuffer::new("request_body", 1024);
        parameter(&mut body, "grant_type", "authorization_code");
        parameter(&mut body, "code", &code);
        parameter(&mut body, "redirect_uri", &self.settings.redirect_uri);
        parameter(&mut body, "client_id", &self.settings.client_id);
        if let Some(secret) = &self.settings.client_secret {
            parameter(&mut body, "client_secret", secret.as_str());
            parameter(&mut body, "audience", self.settings.region.expect("Fleet region").base_url().trim_end_matches('/'));
        } else {
            let verifier = pkce_verifier(self.session.grant_secret());
            parameter(&mut body, "code_verifier", &verifier);
        }
        crate::crypto::install_default_provider();
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(60))
            .build().map_err(|_| failure(SignInError::Unavailable))?;
        let sent_at_s = SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|value| i64::try_from(value.as_secs()).ok())
            .ok_or_else(|| failure(SignInError::Unavailable))?;
        // The transport owns the wiping body; no retry or redirect can resend it.
        let request_body = axum::body::Bytes::from_owner(body);
        let mut response = client.post(self.settings.token.clone()).header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(request_body).send().await.map_err(|error| {
                if error.is_connect() || error.is_builder() { failure(SignInError::Unavailable) } else { failure(SignInError::AnswerInvalid) }
            })?;
        let mut answer = SecretBuffer::new("answer_buffer", ANSWER_CAP);
        if !response.status().is_success() || response.content_length().is_some_and(|length| length > ANSWER_CAP as u64) { return Err(failure(SignInError::AnswerInvalid)); }
        while let Some(chunk) = response.chunk().await.map_err(|_| failure(SignInError::AnswerInvalid))? {
            if answer.len().saturating_add(chunk.len()) > ANSWER_CAP { return Err(failure(SignInError::AnswerInvalid)); }
            answer.extend_from_slice(&chunk);
            if let Ok(bytes) = chunk.try_into_mut() { let mut bytes = bytes; bytes.as_mut().zeroize(); }
        }
        let answer = token_answer(&answer)?;
        let grant = RedeemedGrant::from_validated_redemption_at(self.session, wiped_copy(answer.access.as_str()), wiped_copy(answer.refresh.as_str()), answer.expires_in,
            (self.settings.slot == CredentialSlot::FleetApi).then_some(self.settings.client_id), self.settings.region, sent_at_s)?;
        let stored = self.entry.vault().store_hub_grant(grant)?;
        tracing::info!(slot = match self.settings.slot { CredentialSlot::OwnerApi => "owner_api", CredentialSlot::FleetApi => "fleet_api" },
            origin = if self.settings.slot == CredentialSlot::OwnerApi { "hub_login" } else { "fleet_oauth" }, refresh_owner = "hub", provenance = "hub_grant", "credential write");
        publish(match self.settings.slot { CredentialSlot::OwnerApi => "owner_api", CredentialSlot::FleetApi => "fleet_api" })?;
        self.entry.vault().finish_hub_grant_aftercare(stored)?;
        Ok(())
    }
}

struct TokenAnswer { access: WireSecret, refresh: WireSecret, expires_in: u64 }
fn token_answer(bytes: &[u8]) -> Result<TokenAnswer, CredentialCommandError> {
    struct Answer;
    impl<'de> Visitor<'de> for Answer {
        type Value = TokenAnswer;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("a token answer") }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<TokenAnswer, A::Error> {
            let (mut access, mut refresh, mut expires, mut kind) = (None, None, None, None);
            let mut seen = std::collections::BTreeSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !seen.insert(key.clone()) { return Err(de::Error::custom("duplicate key")); }
                match key.as_str() {
                    "access_token" => access = Some(map.next_value::<WireSecret>()?),
                    "refresh_token" => refresh = Some(map.next_value::<WireSecret>()?),
                    "expires_in" => expires = Some(map.next_value::<u64>()?),
                    "token_type" => kind = Some(map.next_value::<WireSecret>()?),
                    "id_token" => { drop(map.next_value::<WireSecret>()?); }
                    _ => { map.next_value::<de::IgnoredAny>()?; }
                }
            }
            let access = access.ok_or_else(|| de::Error::custom("missing access"))?;
            let refresh = refresh.ok_or_else(|| de::Error::custom("missing refresh"))?;
            let expires_in = expires.ok_or_else(|| de::Error::custom("missing expiry"))?;
            if !kind.is_some_and(|kind| kind.as_str().eq_ignore_ascii_case("bearer")) { return Err(de::Error::custom("token type")); }
            for token in [&access, &refresh] {
                let text = token.as_str();
                if text.is_empty() || text.len() > 16_384 || text.bytes().any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control()) {
                    return Err(de::Error::custom("invalid token"));
                }
            }
            Ok(TokenAnswer { access, refresh, expires_in })
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let answer = decoder.deserialize_map(Answer).map_err(|_| failure(SignInError::AnswerInvalid))?;
    decoder.end().map_err(|_| failure(SignInError::AnswerInvalid))?;
    Ok(answer)
}
