// SPDX-License-Identifier: AGPL-3.0-only

//! C10's credential value codec. Storage and key custody belong to later parts.

use std::fmt;

use aes_gcm::aead::{AeadInOut, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::protocol::hmac_sha256;

pub(crate) const ENVELOPE_MAGIC: [u8; 4] = *b"HCEV";
pub(crate) const ENVELOPE_VERSION: u8 = 1;
pub(crate) const ALG_AES_256_GCM: u8 = 1;
pub(crate) const ENVELOPE_HEADER_BYTES: usize = 39;
pub(crate) const ENVELOPE_NONCE_BYTES: usize = 12;
pub(crate) const ENVELOPE_TAG_BYTES: usize = 16;
pub(crate) const ENVELOPE_OVERHEAD_BYTES: usize =
    ENVELOPE_HEADER_BYTES + ENVELOPE_NONCE_BYTES + ENVELOPE_TAG_BYTES;
pub(crate) const MAX_TOKEN_PLAINTEXT_BYTES: usize = 16_384;
pub(crate) const MAX_TOKEN_ENVELOPE_BYTES: usize =
    MAX_TOKEN_PLAINTEXT_BYTES + ENVELOPE_OVERHEAD_BYTES;
pub(crate) const ENVELOPE_KEY_BYTES: usize = 32;
pub(crate) const KEY_ID_BYTES: usize = 8;
pub(crate) const FINGERPRINT_BYTES: usize = 32;
const ENVELOPE_KEY_INFO: &[u8; 48] = b"teslatlas-hub/credential-envelope/v1/aes-256-gcm";
const FINGERPRINT_LABEL: &[u8; 36] = b"teslatlas-hub/refresh-fingerprint/v1";

// Section 3.1's fixed wire layout; ends are the next field's offset.
const HEADER_MAGIC_OFFSET: usize = 0;
const HEADER_VERSION_OFFSET: usize = 4;
const HEADER_ALGORITHM_OFFSET: usize = 5;
const HEADER_KEY_ID_OFFSET: usize = 6;
const HEADER_TABLE_OFFSET: usize = 14;
const HEADER_SLOT_OFFSET: usize = 15;
const HEADER_COLUMN_OFFSET: usize = 16;
const HEADER_ORIGIN_OFFSET: usize = 17;
const HEADER_OWNER_OFFSET: usize = 18;
const HEADER_ACCOUNT_OFFSET: usize = 19;
const HEADER_GENERATION_OFFSET: usize = 23;
const ENVELOPE_NONCE_OFFSET: usize = ENVELOPE_HEADER_BYTES;
const ENVELOPE_CIPHERTEXT_OFFSET: usize = ENVELOPE_NONCE_OFFSET + ENVELOPE_NONCE_BYTES;
const ENVELOPE_BINDING_BYTES: usize = ENVELOPE_HEADER_BYTES - HEADER_TABLE_OFFSET;
const CREDENTIAL_PAIRS_TABLE: u8 = 1;

const fn binding_offset(header_offset: usize) -> usize {
    header_offset - HEADER_TABLE_OFFSET
}

macro_rules! codes {
    ($name:ident { $($variant:ident = $value:literal => $text:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(u8)]
        #[cfg_attr(not(test), allow(dead_code, reason = "Part E handoff API; consumed by Parts K, S and V1."))]
        pub(crate) enum $name { $($variant = $value),+ }
        impl $name {
            #[cfg_attr(not(test), allow(dead_code, reason = "Part E handoff API; consumed by Parts K, S and V1."))]
            pub(crate) const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }
    };
}

codes!(CredentialSlot { OwnerApi = 1 => "owner_api", FleetApi = 2 => "fleet_api" });
codes!(CredentialColumn { Access = 1 => "access", Refresh = 2 => "refresh" });
codes!(CredentialOrigin {
    HubLogin = 1 => "hub_login", DirectEntry = 2 => "direct_entry",
    ProviderEntry = 3 => "provider_entry", FleetOauth = 4 => "fleet_oauth",
    SourceToken = 5 => "source_token",
});
codes!(RefreshOwner { Hub = 1 => "hub", External = 2 => "external" });

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) struct KeyId(pub [u8; KEY_ID_BYTES]);

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyId([redacted])")
    }
}

#[derive(Clone, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) struct EnvelopePlace {
    pub table: u8,
    pub slot: CredentialSlot,
    pub column: CredentialColumn,
    pub origin: Option<CredentialOrigin>,
    pub owner: RefreshOwner,
    pub account_id: u32,
    pub generation: Uuid,
}

impl fmt::Debug for EnvelopePlace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnvelopePlace([redacted])")
    }
}

/// M2P-25: parsing exposes no typed place. Only `check_place` interprets bindings.
#[derive(Clone)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) struct EnvelopeHeader {
    pub version: u8,
    pub algorithm: u8,
    pub key_id: KeyId,
    binding: [u8; ENVELOPE_BINDING_BYTES],
}

impl fmt::Debug for EnvelopeHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnvelopeHeader([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) enum BindingField {
    Table,
    Slot,
    Column,
    Origin,
    Owner,
    Account,
    Generation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) enum EnvelopeError {
    Truncated,
    OverCap,
    BadMagic,
    UnknownVersion(u8),
    UnknownAlgorithm(u8),
    BindingMismatch(BindingField),
    UnknownKey,
    Corrupt,
    TokenTooLarge,
    EntropyUnavailable,
}

mod nonce_source_sealed {
    pub trait Sealed {}
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) trait NonceSource: nonce_source_sealed::Sealed {
    fn next(&mut self) -> Result<[u8; ENVELOPE_NONCE_BYTES], EnvelopeError>;
}

#[derive(Debug, Default)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) struct OsNonces;

impl nonce_source_sealed::Sealed for OsNonces {}

impl NonceSource for OsNonces {
    fn next(&mut self) -> Result<[u8; ENVELOPE_NONCE_BYTES], EnvelopeError> {
        let mut nonce = [0; ENVELOPE_NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| EnvelopeError::EntropyUnavailable)?;
        Ok(nonce)
    }
}

/// The sole supplied-nonce source (R3), shared by every clean part's tests.
/// One source belongs to one key id. Its queue is FIFO and reuse within its
/// lifetime panics, including a repeated nonce later in the supplied list.
#[cfg(test)]
pub(crate) struct FixedNonces {
    key_id: KeyId,
    pending: std::collections::VecDeque<Result<[u8; ENVELOPE_NONCE_BYTES], EnvelopeError>>,
    used: std::collections::HashSet<(KeyId, [u8; ENVELOPE_NONCE_BYTES])>,
    calls: usize,
}

#[cfg(test)]
impl fmt::Debug for FixedNonces {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FixedNonces([redacted])")
    }
}

#[cfg(test)]
impl FixedNonces {
    pub(crate) fn new(
        key_id: KeyId,
        nonces: impl IntoIterator<Item = [u8; ENVELOPE_NONCE_BYTES]>,
    ) -> Self {
        Self {
            key_id,
            pending: nonces.into_iter().map(Ok).collect(),
            used: std::collections::HashSet::new(),
            calls: 0,
        }
    }

    /// Inject an entropy-source failure without defining another nonce source.
    pub(crate) fn with_error(key_id: KeyId, error: EnvelopeError) -> Self {
        let mut source = Self::new(key_id, []);
        source.pending.push_back(Err(error));
        source
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls
    }
}

#[cfg(test)]
impl nonce_source_sealed::Sealed for FixedNonces {}

#[cfg(test)]
impl NonceSource for FixedNonces {
    fn next(&mut self) -> Result<[u8; ENVELOPE_NONCE_BYTES], EnvelopeError> {
        self.calls += 1;
        let nonce = self.pending.pop_front().expect("fixed nonce exhausted")?;
        assert!(
            self.used.insert((self.key_id, nonce)),
            "nonce reused for key id"
        );
        Ok(nonce)
    }
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) trait EnvelopeKeys {
    fn envelope_key(&self, id: KeyId) -> Option<&Zeroizing<[u8; ENVELOPE_KEY_BYTES]>>;
}

fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> Zeroizing<[u8; 32]> {
    // H4 returns by value (R8); immediate Zeroizing ownership cannot promise
    // removal of compiler-created stack/ABI copies of that returned array.
    Zeroizing::new(hmac_sha256(salt, ikm))
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) fn hkdf_sha256_32(salt: &[u8], ikm: &[u8], info: &[u8]) -> Zeroizing<[u8; 32]> {
    let prk = hkdf_extract(salt, ikm);
    let mut expansion = Zeroizing::new(Vec::with_capacity(info.len() + 1));
    expansion.extend_from_slice(info);
    expansion.push(1);
    Zeroizing::new(hmac_sha256(prk.as_ref(), &expansion))
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) fn derive_envelope_key(root: &[u8; ENVELOPE_KEY_BYTES]) -> Zeroizing<[u8; 32]> {
    hkdf_sha256_32(&[0; 32], root, ENVELOPE_KEY_INFO)
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) fn canonical_token(token: &[u8]) -> &[u8] {
    let is_edge = |b: &u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n');
    let start = token
        .iter()
        .position(|b| !is_edge(b))
        .unwrap_or(token.len());
    let end = token
        .iter()
        .rposition(|b| !is_edge(b))
        .map_or(start, |i| i + 1);
    &token[start..end]
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) fn refresh_fingerprint(token: &[u8]) -> [u8; FINGERPRINT_BYTES] {
    // R9's existing SHA-256 interface does not promise to wipe its internal
    // partial-block buffer. No stronger erasure claim is made here.
    let mut hash = Sha256::new();
    hash.update(FINGERPRINT_LABEL);
    hash.update([0]);
    hash.update(canonical_token(token));
    hash.finalize().into()
}

fn encode_header(key_id: KeyId, place: &EnvelopePlace) -> [u8; ENVELOPE_HEADER_BYTES] {
    let mut header = [0; ENVELOPE_HEADER_BYTES];
    header[HEADER_MAGIC_OFFSET..HEADER_VERSION_OFFSET].copy_from_slice(&ENVELOPE_MAGIC);
    header[HEADER_VERSION_OFFSET] = ENVELOPE_VERSION;
    header[HEADER_ALGORITHM_OFFSET] = ALG_AES_256_GCM;
    header[HEADER_KEY_ID_OFFSET..HEADER_TABLE_OFFSET].copy_from_slice(&key_id.0);
    // Valid callers supply table 1 (§3.1). The spec gives no sealing-time
    // misuse response for another u8; preserve it and let the reader refuse it.
    header[HEADER_TABLE_OFFSET] = place.table;
    header[HEADER_SLOT_OFFSET] = place.slot as u8;
    header[HEADER_COLUMN_OFFSET] = place.column as u8;
    header[HEADER_ORIGIN_OFFSET] = place.origin.map_or(0, |origin| origin as u8);
    header[HEADER_OWNER_OFFSET] = place.owner as u8;
    header[HEADER_ACCOUNT_OFFSET..HEADER_GENERATION_OFFSET]
        .copy_from_slice(&place.account_id.to_be_bytes());
    header[HEADER_GENERATION_OFFSET..ENVELOPE_HEADER_BYTES]
        .copy_from_slice(place.generation.as_bytes());
    header
}

// One in-place AEAD seam also serves CT5's independent published GCM vectors.
fn aead_seal(
    key: &[u8; 32],
    nonce: &[u8; ENVELOPE_NONCE_BYTES],
    aad: &[u8],
    working: &mut [u8],
) -> Result<[u8; ENVELOPE_TAG_BYTES], EnvelopeError> {
    let cipher = Aes256Gcm::new(key.into());
    // Encryption failure is unreachable for the fixed nonce/tag sizes and
    // R7's 16,384-byte cap; retain the existing error mapping as a fallback.
    let tag = cipher
        .encrypt_inout_detached(&Nonce::from(*nonce), aad, working.into())
        .map_err(|_| EnvelopeError::Corrupt)?;
    Ok(tag.into())
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) fn seal_value(
    k_enc: &[u8; ENVELOPE_KEY_BYTES],
    key_id: KeyId,
    place: &EnvelopePlace,
    plaintext: &[u8],
    nonces: &mut dyn NonceSource,
) -> Result<Vec<u8>, EnvelopeError> {
    if plaintext.len() > MAX_TOKEN_PLAINTEXT_BYTES {
        return Err(EnvelopeError::TokenTooLarge);
    }
    let nonce = nonces
        .next()
        .map_err(|_| EnvelopeError::EntropyUnavailable)?;
    #[cfg(test)] crate::auth::credential_vault::fixtures::power_seal(key_id);
    let header = encode_header(key_id, place);
    // Allocate once, at the final envelope size. The only plaintext copy goes
    // directly into its working region and becomes ciphertext in place.
    let mut envelope = Zeroizing::new(vec![0; plaintext.len() + ENVELOPE_OVERHEAD_BYTES]);
    envelope[..ENVELOPE_HEADER_BYTES].copy_from_slice(&header);
    envelope[ENVELOPE_NONCE_OFFSET..ENVELOPE_CIPHERTEXT_OFFSET].copy_from_slice(&nonce);
    let tag_start = ENVELOPE_CIPHERTEXT_OFFSET + plaintext.len();
    envelope[ENVELOPE_CIPHERTEXT_OFFSET..tag_start].copy_from_slice(plaintext);
    let tag = aead_seal(
        k_enc,
        &nonce,
        &header,
        &mut envelope[ENVELOPE_CIPHERTEXT_OFFSET..tag_start],
    )?;
    envelope[tag_start..].copy_from_slice(&tag);
    // Transfer the now-ciphertext buffer without making another copy.
    Ok(std::mem::take(&mut *envelope))
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) fn check_envelope_length(len: usize) -> Result<(), EnvelopeError> {
    if len < ENVELOPE_OVERHEAD_BYTES {
        Err(EnvelopeError::Truncated)
    } else if len > MAX_TOKEN_ENVELOPE_BYTES {
        Err(EnvelopeError::OverCap)
    } else {
        Ok(())
    }
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) fn parse_header(
    header: &[u8; ENVELOPE_HEADER_BYTES],
) -> Result<EnvelopeHeader, EnvelopeError> {
    if header[HEADER_MAGIC_OFFSET..HEADER_VERSION_OFFSET] != ENVELOPE_MAGIC {
        return Err(EnvelopeError::BadMagic);
    }
    if header[HEADER_VERSION_OFFSET] != ENVELOPE_VERSION {
        return Err(EnvelopeError::UnknownVersion(header[HEADER_VERSION_OFFSET]));
    }
    if header[HEADER_ALGORITHM_OFFSET] != ALG_AES_256_GCM {
        return Err(EnvelopeError::UnknownAlgorithm(
            header[HEADER_ALGORITHM_OFFSET],
        ));
    }
    Ok(EnvelopeHeader {
        version: header[HEADER_VERSION_OFFSET],
        algorithm: header[HEADER_ALGORITHM_OFFSET],
        key_id: KeyId(
            header[HEADER_KEY_ID_OFFSET..HEADER_TABLE_OFFSET]
                .try_into()
                .expect("fixed key id width"),
        ),
        binding: header[HEADER_TABLE_OFFSET..ENVELOPE_HEADER_BYTES]
            .try_into()
            .expect("fixed binding width"),
    })
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) fn check_place(
    header: &EnvelopeHeader,
    expected: &EnvelopePlace,
) -> Result<(), EnvelopeError> {
    let raw = &header.binding;
    let origin = expected.origin.map_or(0, |origin| origin as u8);
    // Each guard returns before the next binding is compared (R6, M2P-25).
    macro_rules! require {
        ($matches:expr, $field:ident) => {
            if !$matches {
                return Err(EnvelopeError::BindingMismatch(BindingField::$field));
            }
        };
    }
    require!(
        raw[binding_offset(HEADER_TABLE_OFFSET)] == CREDENTIAL_PAIRS_TABLE
            && raw[binding_offset(HEADER_TABLE_OFFSET)] == expected.table,
        Table
    );
    require!(
        raw[binding_offset(HEADER_SLOT_OFFSET)] == expected.slot as u8,
        Slot
    );
    require!(
        raw[binding_offset(HEADER_COLUMN_OFFSET)] == expected.column as u8,
        Column
    );
    require!(raw[binding_offset(HEADER_ORIGIN_OFFSET)] == origin, Origin);
    require!(
        raw[binding_offset(HEADER_OWNER_OFFSET)] == expected.owner as u8,
        Owner
    );
    require!(
        raw[binding_offset(HEADER_ACCOUNT_OFFSET)..binding_offset(HEADER_GENERATION_OFFSET)]
            == expected.account_id.to_be_bytes(),
        Account
    );
    require!(
        raw[binding_offset(HEADER_GENERATION_OFFSET)..] == *expected.generation.as_bytes(),
        Generation
    );
    Ok(())
}

#[cfg(test)]
thread_local! {
    static DECRYPTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "Part E handoff API; consumed by Parts K, S and V1."
    )
)]
pub(crate) fn open_value(
    keys: &dyn EnvelopeKeys,
    expected: &EnvelopePlace,
    envelope: &[u8],
) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    check_envelope_length(envelope.len())?;
    let aad: &[u8; ENVELOPE_HEADER_BYTES] = envelope[..ENVELOPE_HEADER_BYTES]
        .try_into()
        .expect("checked length");
    let header = parse_header(aad)?;
    check_place(&header, expected)?;
    let key = keys
        .envelope_key(header.key_id)
        .ok_or(EnvelopeError::UnknownKey)?;
    let cipher = Aes256Gcm::new((&**key).into());
    let nonce = Nonce::from(
        <[u8; ENVELOPE_NONCE_BYTES]>::try_from(
            &envelope[ENVELOPE_NONCE_OFFSET..ENVELOPE_CIPHERTEXT_OFFSET],
        )
        .expect("fixed nonce width"),
    );
    let tag_start = envelope.len() - ENVELOPE_TAG_BYTES;
    let tag = Tag::from(
        <[u8; ENVELOPE_TAG_BYTES]>::try_from(&envelope[tag_start..]).expect("fixed tag width"),
    );
    let mut working = Zeroizing::new(envelope[ENVELOPE_CIPHERTEXT_OFFSET..tag_start].to_vec());
    #[cfg(test)]
    DECRYPTIONS.with(|count| count.set(count.get() + 1));
    // GCM verifies its detached tag before decrypting in place. On error the
    // buffer is discarded and zeroised; no unauthenticated bytes escape.
    cipher
        .decrypt_inout_detached(&nonce, aad, working.as_mut_slice().into(), &tag)
        .map_err(|_| EnvelopeError::Corrupt)?;
    Ok(working)
}

#[cfg(test)]
#[path = "credential_envelope/tests.rs"]
mod g24_m2_e;
