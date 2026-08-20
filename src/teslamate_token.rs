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

/// Shared with the source reader and matched by the Hub token store. No
/// plaintext whose Cloak envelope would exceed this limit is admitted.
pub const MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES: usize = 16 * 1024;
pub const CLOAK_ENVELOPE_OVERHEAD_BYTES: usize = 2 + CLOAK_TAG.len() + NONCE_BYTES + AUTH_TAG_BYTES;
pub const MAX_LEGACY_TOKEN_PLAINTEXT_BYTES: usize =
    MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES - CLOAK_ENVELOPE_OVERHEAD_BYTES;

/// Decrypt the one legacy TeslaMate access/refresh pair. Both values are
/// authenticated independently; a malformed, stale, or tampered value is
/// never accepted as an Owner API credential.
pub fn decrypt_legacy_owner_tokens(
    encryption_key: &[u8],
    encrypted_access: &[u8],
    encrypted_refresh: &[u8],
) -> Result<OwnerTokens, TeslaMateTokenError> {
    let cipher = cloak_cipher(encryption_key)?;
    let access = decrypt_cloak_value(&cipher, encrypted_access)?;
    let refresh = decrypt_cloak_value(&cipher, encrypted_refresh)?;
    let access = String::from_utf8(access).map_err(|_| TeslaMateTokenError::InvalidPlaintext)?;
    let refresh = String::from_utf8(refresh).map_err(|_| TeslaMateTokenError::InvalidPlaintext)?;
    OwnerTokens::from_secret_parts(access, refresh)
        .map_err(|_| TeslaMateTokenError::InvalidPlaintext)
}

/// Encrypt one legacy access/refresh pair in the exact TeslaMate Cloak
/// envelope used by `private.tokens`.
pub fn encrypt_legacy_owner_tokens(
    encryption_key: &[u8],
    tokens: &OwnerTokens,
) -> Result<(Vec<u8>, Vec<u8>), TeslaMateTokenError> {
    let cipher = cloak_cipher(encryption_key)?;
    Ok((
        encrypt_cloak_value(&cipher, tokens.access_token().as_bytes())?,
        encrypt_cloak_value(&cipher, tokens.refresh_token().as_bytes())?,
    ))
}

/// Encrypt a user-supplied legacy pair without exposing the credential type
/// through the CLI crate. Files may end in one conventional line ending.
pub fn encrypt_legacy_owner_token_files(
    encryption_key: &[u8],
    mut access_token: Zeroizing<Vec<u8>>,
    mut refresh_token: Zeroizing<Vec<u8>>,
) -> Result<(Vec<u8>, Vec<u8>), TeslaMateTokenError> {
    strip_line_ending(&mut access_token);
    strip_line_ending(&mut refresh_token);
    validate_file_token_plaintext(access_token.as_slice())?;
    validate_file_token_plaintext(refresh_token.as_slice())?;
    let cipher = cloak_cipher(encryption_key)?;
    Ok((
        encrypt_cloak_value(&cipher, access_token.as_slice())?,
        encrypt_cloak_value(&cipher, refresh_token.as_slice())?,
    ))
}

fn strip_line_ending(bytes: &mut Zeroizing<Vec<u8>>) {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
}

fn validate_file_token_plaintext(value: &[u8]) -> Result<(), TeslaMateTokenError> {
    if value.is_empty()
        || value.len() > MAX_LEGACY_TOKEN_PLAINTEXT_BYTES
        || std::str::from_utf8(value).is_err()
        || value
            .iter()
            .any(|byte| *byte == 0 || *byte == b'\r' || *byte == b'\n' || byte.is_ascii_control())
    {
        return Err(TeslaMateTokenError::InvalidPlaintext);
    }
    Ok(())
}

fn cloak_cipher(encryption_key: &[u8]) -> Result<Aes256Gcm, TeslaMateTokenError> {
    if encryption_key.is_empty() {
        return Err(TeslaMateTokenError::EmptyEncryptionKey);
    }
    let key = Zeroizing::new(Sha256::digest(encryption_key).to_vec());
    Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| TeslaMateTokenError::InvalidEncryptionKey)
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
    let nonce = Nonce::try_from(&encrypted[nonce_end..auth_tag_start])
        .map_err(|_| TeslaMateTokenError::UnsupportedEnvelope)?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext.as_slice(),
                aad: ASSOCIATED_DATA,
            },
        )
        .map_err(|_| TeslaMateTokenError::AuthenticationFailed)
}

fn encrypt_cloak_value(
    cipher: &Aes256Gcm,
    plaintext: &[u8],
) -> Result<Vec<u8>, TeslaMateTokenError> {
    let mut nonce_bytes = [0_u8; NONCE_BYTES];
    getrandom::getrandom(&mut nonce_bytes).expect("system entropy");
    let nonce = Nonce::from(nonce_bytes);
    let mut ciphertext_and_tag = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: ASSOCIATED_DATA,
            },
        )
        .map_err(|_| TeslaMateTokenError::EncryptionFailed)?;
    let tag_start = ciphertext_and_tag
        .len()
        .checked_sub(AUTH_TAG_BYTES)
        .ok_or(TeslaMateTokenError::EncryptionFailed)?;
    let auth_tag = ciphertext_and_tag.split_off(tag_start);

    let mut result = Vec::with_capacity(
        2 + CLOAK_TAG.len() + NONCE_BYTES + auth_tag.len() + ciphertext_and_tag.len(),
    );
    result.extend_from_slice(&[CLOAK_TYPE, CLOAK_TAG.len() as u8]);
    result.extend_from_slice(CLOAK_TAG);
    result.extend_from_slice(nonce.as_slice());
    result.extend_from_slice(&auth_tag);
    result.extend_from_slice(&ciphertext_and_tag);
    Ok(result)
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
    #[error("TeslaMate token encryption failed")]
    EncryptionFailed,
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
    use zeroize::Zeroizing;

    use super::{
        ASSOCIATED_DATA, AUTH_TAG_BYTES, CLOAK_TAG, CLOAK_TYPE, MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES,
        MAX_LEGACY_TOKEN_PLAINTEXT_BYTES, NONCE_BYTES, TeslaMateTokenError,
        decrypt_legacy_owner_tokens, encrypt_legacy_owner_token_files, encrypt_legacy_owner_tokens,
    };
    use crate::credentials::OwnerTokens;

    const ENCRYPTION_KEY: &[u8] = b"teslamate-test-encryption-key-v1";
    const WRONG_ENCRYPTION_KEY: &[u8] = b"teslamate-test-encryption-key-v2";
    const ACCESS_TOKEN: &[u8] = b"access-token-deterministic-01";
    const REFRESH_TOKEN: &[u8] = b"refresh-token-deterministic-01";

    // Generated independently with Python cryptography AESGCM using the raw
    // key SHA-256, this nonce, and AAD `AES256GCM`. It is a fixed TeslaMate
    // Cloak V1 envelope: type, tag length, tag, IV, auth tag, ciphertext.
    const EXTERNAL_CLOAK_ACCESS: [u8; 69] = [
        0x01, 0x0a, 0x41, 0x45, 0x53, 0x2e, 0x47, 0x43, 0x4d, 0x2e, 0x56, 0x31, 0x01, 0x02, 0x03,
        0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x62, 0x16, 0xd5, 0xcd, 0x52, 0xe3,
        0x7a, 0x91, 0xf7, 0xc9, 0x9b, 0x0b, 0x99, 0xbe, 0xeb, 0x1b, 0x16, 0xef, 0xb1, 0xee, 0xab,
        0xbf, 0xbc, 0xce, 0x4f, 0x5f, 0x56, 0xdb, 0x3d, 0x6c, 0x3e, 0x8a, 0xb5, 0x84, 0x75, 0xed,
        0x8b, 0x81, 0xd6, 0xb1, 0xd0, 0x5a, 0x46, 0xc9, 0x24,
    ];

    fn envelope(plaintext: &[u8], nonce_bytes: [u8; NONCE_BYTES]) -> Vec<u8> {
        let key = Sha256::digest(ENCRYPTION_KEY);
        let cipher = Aes256Gcm::new_from_slice(&key).expect("fixed key is valid");
        let encrypted = cipher
            .encrypt(
                &Nonce::from(nonce_bytes),
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
        let access = EXTERNAL_CLOAK_ACCESS;
        let refresh = envelope(REFRESH_TOKEN, [12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);

        let tokens = decrypt_legacy_owner_tokens(ENCRYPTION_KEY, &access, &refresh)
            .expect("fixed Cloak pair decrypts");
        assert_eq!(tokens.access_token().as_bytes(), ACCESS_TOKEN);
        assert_eq!(tokens.refresh_token().as_bytes(), REFRESH_TOKEN);
    }

    #[test]
    fn encrypts_legacy_pair_read_from_newline_terminated_files() {
        let (access, refresh) = encrypt_legacy_owner_token_files(
            ENCRYPTION_KEY,
            Zeroizing::new(b"access-token-deterministic-01\n".to_vec()),
            Zeroizing::new(b"refresh-token-deterministic-01\r\n".to_vec()),
        )
        .expect("file pair encrypts");
        let tokens = decrypt_legacy_owner_tokens(ENCRYPTION_KEY, &access, &refresh)
            .expect("encrypted file pair decrypts");
        assert_eq!(tokens.access_token().as_bytes(), ACCESS_TOKEN);
        assert_eq!(tokens.refresh_token().as_bytes(), REFRESH_TOKEN);
    }

    #[test]
    fn envelope_exact_plaintext_cap_persists_and_next_byte_is_rejected() {
        let access = Zeroizing::new(vec![b'a'; MAX_LEGACY_TOKEN_PLAINTEXT_BYTES]);
        let refresh = Zeroizing::new(vec![b'b'; MAX_LEGACY_TOKEN_PLAINTEXT_BYTES]);
        let (access_ciphertext, refresh_ciphertext) =
            encrypt_legacy_owner_token_files(ENCRYPTION_KEY, access, refresh)
                .expect("exact plaintext cap encrypts");
        assert_eq!(access_ciphertext.len(), MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES);
        assert_eq!(refresh_ciphertext.len(), MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES);
        let stored =
            crate::db::TeslaMateLegacyTokenStore::imported(access_ciphertext, refresh_ciphertext)
                .expect("exact envelope cap persists");
        assert_eq!(
            decrypt_legacy_owner_tokens(ENCRYPTION_KEY, stored.access(), stored.refresh())
                .expect("stored cap decrypts")
                .access_token()
                .len(),
            MAX_LEGACY_TOKEN_PLAINTEXT_BYTES
        );

        assert!(matches!(
            encrypt_legacy_owner_token_files(
                ENCRYPTION_KEY,
                Zeroizing::new(vec![b'a'; MAX_LEGACY_TOKEN_PLAINTEXT_BYTES + 1]),
                Zeroizing::new(vec![b'b'; MAX_LEGACY_TOKEN_PLAINTEXT_BYTES]),
            ),
            Err(TeslaMateTokenError::InvalidPlaintext)
        ));
    }

    #[test]
    fn rejects_deterministic_cloak_pair_with_wrong_key() {
        let access = EXTERNAL_CLOAK_ACCESS;
        let refresh = envelope(REFRESH_TOKEN, [12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);

        let result = decrypt_legacy_owner_tokens(WRONG_ENCRYPTION_KEY, &access, &refresh);

        assert!(matches!(
            result,
            Err(TeslaMateTokenError::AuthenticationFailed)
        ));
    }

    #[test]
    fn rejects_deterministic_cloak_pair_with_altered_authenticated_byte() {
        let mut access = EXTERNAL_CLOAK_ACCESS;
        let ciphertext_byte = 2 + CLOAK_TAG.len() + NONCE_BYTES + AUTH_TAG_BYTES;
        access[ciphertext_byte] ^= 1;
        let refresh = envelope(REFRESH_TOKEN, [12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);

        let result = decrypt_legacy_owner_tokens(ENCRYPTION_KEY, &access, &refresh);

        assert!(matches!(
            result,
            Err(TeslaMateTokenError::AuthenticationFailed)
        ));
    }

    #[test]
    fn encrypts_teslamate_cloak_pair_that_round_trips() {
        let original = OwnerTokens::from_secret_parts(
            String::from_utf8(ACCESS_TOKEN.to_vec()).expect("test token is utf8"),
            String::from_utf8(REFRESH_TOKEN.to_vec()).expect("test token is utf8"),
        )
        .expect("test pair is valid");

        let (access, refresh) =
            encrypt_legacy_owner_tokens(ENCRYPTION_KEY, &original).expect("pair encrypts");
        let recovered = decrypt_legacy_owner_tokens(ENCRYPTION_KEY, &access, &refresh)
            .expect("encrypted pair decrypts");

        assert_eq!(recovered, original);
        for (encrypted, plaintext) in [(&access, ACCESS_TOKEN), (&refresh, REFRESH_TOKEN)] {
            assert_eq!(&encrypted[..2], &[CLOAK_TYPE, CLOAK_TAG.len() as u8]);
            assert_eq!(&encrypted[2..2 + CLOAK_TAG.len()], CLOAK_TAG);
            assert_eq!(
                encrypted.len(),
                2 + CLOAK_TAG.len() + NONCE_BYTES + AUTH_TAG_BYTES + plaintext.len()
            );
        }
    }

    #[test]
    fn encrypts_with_distinct_cryptographic_nonces() {
        let tokens = OwnerTokens::from_secret_parts(
            String::from_utf8(ACCESS_TOKEN.to_vec()).expect("test token is utf8"),
            String::from_utf8(REFRESH_TOKEN.to_vec()).expect("test token is utf8"),
        )
        .expect("test pair is valid");

        let (first_access, _) =
            encrypt_legacy_owner_tokens(ENCRYPTION_KEY, &tokens).expect("first pair encrypts");
        let (second_access, _) =
            encrypt_legacy_owner_tokens(ENCRYPTION_KEY, &tokens).expect("second pair encrypts");
        let nonce_start = 2 + CLOAK_TAG.len();
        let nonce_end = nonce_start + NONCE_BYTES;

        assert_ne!(
            &first_access[nonce_start..nonce_end],
            &second_access[nonce_start..nonce_end]
        );
    }
}
