//! TeslaMate legacy-token decryption without executing TeslaMate.
//!
//! The source database stays read-only. This module accepts only the opaque
//! `private.tokens` bytea values and the exact TeslaMate `ENCRYPTION_KEY`,
//! verifies the Cloak AES-GCM envelope, then returns a redacted typed pair for
//! immediate hand-off to a host-encrypted Hub credential.

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::credentials::OwnerTokens;

const CLOAK_TYPE: u8 = 1;
const CLOAK_TAG: &[u8] = b"AES.GCM.V1";
const NONCE_BYTES: usize = 12;
const AUTH_TAG_BYTES: usize = 16;
const ASSOCIATED_DATA: &[u8] = b"AES256GCM";

/// Decrypt the one legacy TeslaMate access/refresh pair. Both values are
/// authenticated independently; a malformed, stale, or tampered value is
/// never accepted as an Owner API credential.
pub fn decrypt_legacy_owner_tokens(
    encryption_key: &[u8],
    encrypted_access: &[u8],
    encrypted_refresh: &[u8],
) -> Result<OwnerTokens, TeslaMateTokenError> {
    if encryption_key.is_empty() {
        return Err(TeslaMateTokenError::EmptyEncryptionKey);
    }
    let key = Zeroizing::new(Sha256::digest(encryption_key).to_vec());
    let cipher = Aes256Gcm::new_from_slice(key.as_slice())
        .map_err(|_| TeslaMateTokenError::InvalidEncryptionKey)?;
    let access = decrypt_cloak_value(&cipher, encrypted_access)?;
    let refresh = decrypt_cloak_value(&cipher, encrypted_refresh)?;
    let access = String::from_utf8(access).map_err(|_| TeslaMateTokenError::InvalidPlaintext)?;
    let refresh = String::from_utf8(refresh).map_err(|_| TeslaMateTokenError::InvalidPlaintext)?;
    OwnerTokens::from_secret_parts(access, refresh)
        .map_err(|_| TeslaMateTokenError::InvalidPlaintext)
}

fn decrypt_cloak_value(
    cipher: &Aes256Gcm,
    encrypted: &[u8],
) -> Result<Vec<u8>, TeslaMateTokenError> {
    if encrypted.len() < 2 || encrypted[0] != CLOAK_TYPE {
        return Err(TeslaMateTokenError::UnsupportedEnvelope);
    }
    let tag_bytes = usize::from(encrypted[1]);
    let nonce_start = 2_usize;
    let nonce_end = nonce_start
        .checked_add(tag_bytes)
        .ok_or(TeslaMateTokenError::UnsupportedEnvelope)?;
    let auth_tag_start = nonce_end
        .checked_add(NONCE_BYTES)
        .ok_or(TeslaMateTokenError::UnsupportedEnvelope)?;
    let ciphertext_start = auth_tag_start
        .checked_add(AUTH_TAG_BYTES)
        .ok_or(TeslaMateTokenError::UnsupportedEnvelope)?;
    if tag_bytes != CLOAK_TAG.len()
        || encrypted.len() <= ciphertext_start
        || encrypted.get(nonce_start..nonce_end) != Some(CLOAK_TAG)
    {
        return Err(TeslaMateTokenError::UnsupportedEnvelope);
    }

    let mut ciphertext = Zeroizing::new(Vec::with_capacity(
        encrypted.len() - ciphertext_start + AUTH_TAG_BYTES,
    ));
    ciphertext.extend_from_slice(&encrypted[ciphertext_start..]);
    ciphertext.extend_from_slice(&encrypted[auth_tag_start..ciphertext_start]);
    cipher
        .decrypt(
            Nonce::from_slice(&encrypted[nonce_end..auth_tag_start]),
            Payload {
                msg: ciphertext.as_slice(),
                aad: ASSOCIATED_DATA,
            },
        )
        .map_err(|_| TeslaMateTokenError::AuthenticationFailed)
}

#[derive(Debug, Error)]
pub enum TeslaMateTokenError {
    #[error("TeslaMate encryption key is empty")]
    EmptyEncryptionKey,
    #[error("TeslaMate encryption key is invalid")]
    InvalidEncryptionKey,
    #[error("TeslaMate token envelope is unsupported")]
    UnsupportedEnvelope,
    #[error("TeslaMate token authentication failed")]
    AuthenticationFailed,
    #[error("TeslaMate token plaintext is invalid")]
    InvalidPlaintext,
}

#[cfg(test)]
mod tests {
    use aes_gcm::{
        Aes256Gcm, Nonce,
        aead::{Aead, KeyInit, Payload},
    };
    use sha2::{Digest, Sha256};

    use super::{
        ASSOCIATED_DATA, AUTH_TAG_BYTES, CLOAK_TAG, CLOAK_TYPE, NONCE_BYTES, TeslaMateTokenError,
        decrypt_legacy_owner_tokens,
    };

    const ENCRYPTION_KEY: &[u8] = b"teslamate-test-encryption-key-v1";
    const WRONG_ENCRYPTION_KEY: &[u8] = b"teslamate-test-encryption-key-v2";
    const ACCESS_TOKEN: &[u8] = b"access-token-deterministic-01";
    const REFRESH_TOKEN: &[u8] = b"refresh-token-deterministic-01";

    fn envelope(plaintext: &[u8], nonce_bytes: [u8; NONCE_BYTES]) -> Vec<u8> {
        let key = Sha256::digest(ENCRYPTION_KEY);
        let cipher = Aes256Gcm::new_from_slice(&key).expect("fixed key is valid");
        let encrypted = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad: ASSOCIATED_DATA,
                },
            )
            .expect("fixed plaintext is encryptable");
        let split = encrypted.len() - AUTH_TAG_BYTES;
        let mut result = Vec::with_capacity(2 + CLOAK_TAG.len() + NONCE_BYTES + encrypted.len());
        result.extend_from_slice(&[CLOAK_TYPE, CLOAK_TAG.len() as u8]);
        result.extend_from_slice(CLOAK_TAG);
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&encrypted[split..]);
        result.extend_from_slice(&encrypted[..split]);
        result
    }

    #[test]
    fn decrypts_deterministic_cloak_access_refresh_pair_with_exact_key_bytes() {
        let access = envelope(ACCESS_TOKEN, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        let refresh = envelope(REFRESH_TOKEN, [12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);

        let tokens = decrypt_legacy_owner_tokens(ENCRYPTION_KEY, &access, &refresh)
            .expect("fixed Cloak pair decrypts");
        let json = tokens.credential_json().expect("pair serializes");

        assert!(
            json.windows(ACCESS_TOKEN.len())
                .any(|window| window == ACCESS_TOKEN)
        );
        assert!(
            json.windows(REFRESH_TOKEN.len())
                .any(|window| window == REFRESH_TOKEN)
        );
    }

    #[test]
    fn rejects_deterministic_cloak_pair_with_wrong_key() {
        let access = envelope(ACCESS_TOKEN, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        let refresh = envelope(REFRESH_TOKEN, [12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);

        let result = decrypt_legacy_owner_tokens(WRONG_ENCRYPTION_KEY, &access, &refresh);

        assert!(matches!(
            result,
            Err(TeslaMateTokenError::AuthenticationFailed)
        ));
    }

    #[test]
    fn rejects_deterministic_cloak_pair_with_altered_authenticated_byte() {
        let mut access = envelope(ACCESS_TOKEN, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        let ciphertext_byte = 2 + CLOAK_TAG.len() + NONCE_BYTES + AUTH_TAG_BYTES;
        access[ciphertext_byte] ^= 1;
        let refresh = envelope(REFRESH_TOKEN, [12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);

        let result = decrypt_legacy_owner_tokens(ENCRYPTION_KEY, &access, &refresh);

        assert!(matches!(
            result,
            Err(TeslaMateTokenError::AuthenticationFailed)
        ));
    }
}
