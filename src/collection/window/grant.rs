// SPDX-License-Identifier: AGPL-3.0-only

//! The signed window grant and the issuer's trust document.
//!
//! The canonical body (ruling R1) is the issuer key id, a sequence number, the
//! host id, the start, the hard expiry, the approver `G`, the reason and the
//! trust epoch. The issuer signs its canonical JSON (RFC 8785). The body
//! carries no ledger digest; the ledger holds the body digest and the previous
//! entry's digest instead.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::{
    GRANT_MAX_BYTES, REASON_MAX_CHARS, WINDOW_CEILING_SECONDS, ledger::sha256_hex, refusal::Refusal,
};

pub const GRANT_FORMAT: &str = "window-grant/1";
pub const TRUST_FORMAT: &str = "window-trust/1";
pub const APPROVER: &str = "G";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantBody {
    pub format: String,
    pub issuer_key_id: String,
    pub seq: u64,
    pub host_id: String,
    pub starts_at: i64,
    pub expires_at: i64,
    pub approver: String,
    pub reason: String,
    pub trust_epoch: u64,
}

impl GrantBody {
    pub fn canonical(&self) -> Result<Vec<u8>, Refusal> {
        serde_jcs::to_vec(self).map_err(|_| Refusal::GrantMalformed)
    }

    /// SHA-256 over the canonical body, in hex: the grant's identity.
    pub fn digest(&self) -> Result<String, Refusal> {
        Ok(sha256_hex(&self.canonical()?))
    }

    /// Shape checks both the issuer and the Hub apply; none needs the clock.
    pub fn check_shape(&self) -> Result<(), Refusal> {
        if self.format != GRANT_FORMAT {
            return Err(Refusal::FormatUnknown);
        }
        if self.approver != APPROVER {
            return Err(Refusal::ApproverNotG);
        }
        if self.reason.chars().count() > REASON_MAX_CHARS {
            return Err(Refusal::ReasonTooLong);
        }
        if self.host_id.is_empty() || self.issuer_key_id.is_empty() {
            return Err(Refusal::GrantMalformed);
        }
        if self.expires_at <= self.starts_at {
            return Err(Refusal::ExpiryInvalid);
        }
        if self.expires_at - self.starts_at > WINDOW_CEILING_SECONDS {
            return Err(Refusal::LengthExceedsCeiling);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedGrant {
    pub body: GrantBody,
    /// Ed25519 signature over the canonical body, hex.
    pub signature: String,
}

impl SignedGrant {
    pub fn sign(body: GrantBody, key: &SigningKey) -> Result<Self, Refusal> {
        let signature = key.sign(&body.canonical()?);
        Ok(Self {
            body,
            signature: hex::encode(signature.to_bytes()),
        })
    }

    /// Strict parse of a grant file: size limit, unknown fields refused.
    pub fn parse(bytes: &[u8]) -> Result<Self, Refusal> {
        if bytes.len() > GRANT_MAX_BYTES {
            return Err(Refusal::GrantTooLarge);
        }
        serde_json::from_slice(bytes).map_err(|_| Refusal::GrantMalformed)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Refusal> {
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|_| Refusal::GrantMalformed)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn verify(&self, key: &VerifyingKey) -> Result<(), Refusal> {
        let raw = hex::decode(&self.signature).map_err(|_| Refusal::SignatureInvalid)?;
        let signature = Signature::from_slice(&raw).map_err(|_| Refusal::SignatureInvalid)?;
        key.verify_strict(&self.body.canonical()?, &signature)
            .map_err(|_| Refusal::SignatureInvalid)
    }
}

/// What `window trust` stores: the issuer's public key, its id and the trust
/// epoch grants must carry. The issuer exports it; it holds no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustDocument {
    pub format: String,
    pub issuer_key_id: String,
    pub public_key: String,
    pub trust_epoch: u64,
}

pub fn key_id_of(key: &VerifyingKey) -> String {
    format!("ed25519-sha256-{}", sha256_hex(&key.to_bytes()))
}

impl TrustDocument {
    pub fn new(key: &VerifyingKey, trust_epoch: u64) -> Self {
        Self {
            format: TRUST_FORMAT.to_owned(),
            issuer_key_id: key_id_of(key),
            public_key: hex::encode(key.to_bytes()),
            trust_epoch,
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, Refusal> {
        if bytes.len() > GRANT_MAX_BYTES {
            return Err(Refusal::TrustInvalid);
        }
        let document: Self = serde_json::from_slice(bytes).map_err(|_| Refusal::TrustInvalid)?;
        document.verifying_key()?;
        Ok(document)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Refusal> {
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|_| Refusal::TrustInvalid)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// The public key, checked against the document's format and key id.
    pub fn verifying_key(&self) -> Result<VerifyingKey, Refusal> {
        if self.format != TRUST_FORMAT || self.trust_epoch == 0 {
            return Err(Refusal::TrustInvalid);
        }
        let raw: [u8; 32] = hex::decode(&self.public_key)
            .map_err(|_| Refusal::TrustInvalid)?
            .try_into()
            .map_err(|_| Refusal::TrustInvalid)?;
        let key = VerifyingKey::from_bytes(&raw).map_err(|_| Refusal::TrustInvalid)?;
        if key_id_of(&key) != self.issuer_key_id {
            return Err(Refusal::TrustInvalid);
        }
        Ok(key)
    }
}
