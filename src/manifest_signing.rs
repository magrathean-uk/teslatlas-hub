//! Ed25519 signing state for client-verifiable manifest response bytes.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};

use crate::protocol::CursorKey;

pub(crate) struct ManifestSigning {
    signing_key: SigningKey,
}

impl ManifestSigning {
    pub(crate) fn from_cursor_key(cursor_key: &CursorKey) -> Self {
        let mut seed = cursor_key.manifest_signing_seed();
        let signing_key = SigningKey::from_bytes(&seed);
        seed.fill(0);
        Self { signing_key }
    }

    pub(crate) fn verifying_key_hex(&self) -> String {
        hex::encode(self.signing_key.verifying_key().to_bytes())
    }

    pub(crate) fn sign_base64(&self, raw_manifest_json: &[u8]) -> String {
        let signature = self.signing_key.sign(raw_manifest_json);
        STANDARD.encode(signature.to_bytes())
    }
}

impl std::fmt::Debug for ManifestSigning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ManifestSigning([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signature, VerifyingKey};

    use super::*;

    #[test]
    fn derived_signature_is_strictly_bound_to_exact_manifest_bytes() {
        let signing = ManifestSigning::from_cursor_key(&CursorKey::from_bytes([7; 32]));
        let raw_manifest = br#"{"protocol":{"major":1,"minor":0}}"#;
        let signature_bytes = STANDARD
            .decode(signing.sign_base64(raw_manifest))
            .expect("base64 signature");
        let signature = Signature::from_slice(&signature_bytes).expect("64-byte signature");
        let verifying_key_bytes: [u8; 32] = hex::decode(signing.verifying_key_hex())
            .expect("hex verifying key")
            .try_into()
            .expect("32-byte verifying key");
        let verifying_key =
            VerifyingKey::from_bytes(&verifying_key_bytes).expect("valid verifying key");

        verifying_key
            .verify_strict(raw_manifest, &signature)
            .expect("exact response bytes verify");

        let mut mutated = raw_manifest.to_vec();
        mutated[0] ^= 1;
        assert!(verifying_key.verify_strict(&mutated, &signature).is_err());
    }

    #[test]
    fn manifest_key_derivation_is_stable_separate_and_redacted() {
        let cursor_key = CursorKey::from_bytes([19; 32]);
        let first = ManifestSigning::from_cursor_key(&cursor_key);
        let second = ManifestSigning::from_cursor_key(&cursor_key);
        let different = ManifestSigning::from_cursor_key(&CursorKey::from_bytes([20; 32]));

        assert_eq!(first.verifying_key_hex(), second.verifying_key_hex());
        assert_ne!(first.verifying_key_hex(), different.verifying_key_hex());
        assert_eq!(first.verifying_key_hex().len(), 64);
        assert!(
            first
                .verifying_key_hex()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_eq!(format!("{first:?}"), "ManifestSigning([redacted])");
    }
}
