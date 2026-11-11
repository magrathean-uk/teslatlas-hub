// SPDX-License-Identifier: AGPL-3.0-only

use std::{cell::RefCell, fmt};
use serde::de::{self, Deserializer as _, MapAccess, Visitor};
use zeroize::Zeroizing;
use crate::fleet_api::WireSecret;
use super::{CredentialCommandError, OwnerEntryKind, PreparedOwnerEntry, prepare_owner_entry};

#[derive(Debug)]
pub enum OwnerInputError { Credential(CredentialCommandError), UnknownKey(String), DuplicateKey(String), InvalidDocument }
impl fmt::Display for OwnerInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Credential(error) => error.fmt(f),
            Self::UnknownKey(key) => write!(f, "unknown input key: {key}"),
            Self::DuplicateKey(key) => write!(f, "duplicate input key: {key}"),
            Self::InvalidDocument => f.write_str("invalid token input document"),
        }
    }
}
impl std::error::Error for OwnerInputError {}

pub fn decode_owner_files(access: Zeroizing<Vec<u8>>, refresh: Zeroizing<Vec<u8>>, kind: OwnerEntryKind) -> Result<PreparedOwnerEntry, OwnerInputError> {
    fn token(bytes: &[u8]) -> Result<Zeroizing<String>, OwnerInputError> {
        let text = std::str::from_utf8(bytes).map_err(|_| OwnerInputError::InvalidDocument)?;
        let trimmed = text.trim_matches(|ch| matches!(ch, ' ' | '\t' | '\r' | '\n'));
        if trimmed.starts_with('"') {
            let decoded: WireSecret = serde_json::from_str(trimmed).map_err(|_| OwnerInputError::InvalidDocument)?;
            Ok(Zeroizing::new(decoded.as_str().to_owned()))
        } else { Ok(Zeroizing::new(text.to_owned())) }
    }
    prepare_owner_entry(kind, token(&access)?, Some(token(&refresh)?), false).map_err(OwnerInputError::Credential)
}

pub fn decode_owner_entry(bytes: &[u8], kind: OwnerEntryKind, replace_source: bool) -> Result<PreparedOwnerEntry, OwnerInputError> {
    struct Input<'a>(&'a RefCell<Option<OwnerInputError>>);
    impl<'de> Visitor<'de> for Input<'_> {
        type Value = (WireSecret, Option<WireSecret>);
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("a token object") }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let (mut access, mut refresh, mut refresh_seen) = (None, None, false);
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "origin" => { *self.0.borrow_mut() = Some(OwnerInputError::Credential(CredentialCommandError::new("invalid_origin"))); return Err(de::Error::custom("origin")); }
                    "accessToken" if access.is_none() => access = Some(map.next_value()?),
                    "refreshToken" if !refresh_seen => { refresh_seen = true; refresh = Some(map.next_value()?); }
                    "accessToken" | "refreshToken" => { *self.0.borrow_mut() = Some(OwnerInputError::DuplicateKey(key)); return Err(de::Error::custom("duplicate")); }
                    _ => { *self.0.borrow_mut() = Some(OwnerInputError::UnknownKey(key)); return Err(de::Error::custom("unknown")); }
                }
            }
            Ok((access.ok_or_else(|| de::Error::custom("missing access"))?, refresh))
        }
    }
    let reason = RefCell::new(None);
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let (access, refresh) = decoder.deserialize_map(Input(&reason)).map_err(|_| reason.borrow_mut().take().unwrap_or(OwnerInputError::InvalidDocument))?;
    decoder.end().map_err(|_| OwnerInputError::InvalidDocument)?;
    prepare_owner_entry(kind, Zeroizing::new(access.as_str().to_owned()), refresh.map(|value| Zeroizing::new(value.as_str().to_owned())), replace_source)
        .map_err(OwnerInputError::Credential)
}

pub struct PreparedFleetSetup {
    entry: super::PreparedFleetEntry,
    credentials: crate::fleet_credentials::FleetSetupCredentials,
}
impl fmt::Debug for PreparedFleetSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.debug_struct("PreparedFleetSetup").field("entry", &self.entry).finish() }
}
impl PreparedFleetSetup {
    pub fn into_credentials(self) -> crate::fleet_credentials::FleetSetupCredentials { self.credentials }
}

pub fn decode_fleet_entry(bytes: &[u8]) -> Result<PreparedFleetSetup, Box<dyn std::error::Error>> {
    struct Input<'a>(&'a RefCell<Option<OwnerInputError>>);
    impl<'de> Visitor<'de> for Input<'_> {
        type Value = (WireSecret, WireSecret, String, crate::fleet_api::FleetRegion, u64);
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("a Fleet token object") }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let (mut access, mut refresh, mut client, mut region, mut expiry) = (None, None, None, None, None);
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "origin" => { *self.0.borrow_mut() = Some(OwnerInputError::Credential(CredentialCommandError::new("invalid_origin"))); return Err(de::Error::custom("origin")); }
                    "accessToken" if access.is_none() => access = Some(map.next_value()?),
                    "refreshToken" if refresh.is_none() => refresh = Some(map.next_value()?),
                    "clientId" if client.is_none() => client = Some(map.next_value()?),
                    "region" if region.is_none() => region = Some(map.next_value()?),
                    "expiresInSeconds" if expiry.is_none() => expiry = Some(map.next_value()?),
                    "accessToken" | "refreshToken" | "clientId" | "region" | "expiresInSeconds" => { *self.0.borrow_mut() = Some(OwnerInputError::DuplicateKey(key)); return Err(de::Error::custom("duplicate")); }
                    _ => { *self.0.borrow_mut() = Some(OwnerInputError::UnknownKey(key)); return Err(de::Error::custom("unknown")); }
                }
            }
            let missing = || de::Error::custom("missing input field");
            Ok((access.ok_or_else(missing)?, refresh.ok_or_else(missing)?, client.ok_or_else(missing)?, region.ok_or_else(missing)?, expiry.ok_or_else(missing)?))
        }
    }
    let reason = RefCell::new(None);
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let (access, refresh, client, region, expiry) = decoder.deserialize_map(Input(&reason))
        .map_err(|_| reason.borrow_mut().take().unwrap_or(OwnerInputError::InvalidDocument))?;
    decoder.end().map_err(|_| OwnerInputError::InvalidDocument)?;
    let entry = super::prepare_fleet_entry(Zeroizing::new(access.as_str().to_owned()), Zeroizing::new(refresh.as_str().to_owned()), client.clone(), region, expiry)?;
    let canonical = |text: &str| text.trim_matches(|ch| matches!(ch, ' ' | '\t' | '\r' | '\n')).to_owned();
    let credentials = crate::fleet_credentials::FleetSetupCredentials::new(canonical(access.as_str()), canonical(refresh.as_str()), client, region, expiry)?;
    credentials.require_collection_scopes()?;
    Ok(PreparedFleetSetup { entry, credentials })
}

pub fn persist_fleet_setup(entry: &crate::CredentialVaultEntry, prepared: PreparedFleetSetup, clear_owner: bool) -> Result<crate::fleet_credentials::FleetSetupCredentials, CredentialCommandError> {
    super::persist_fleet_entry(entry, prepared.entry, clear_owner)?;
    Ok(prepared.credentials)
}
