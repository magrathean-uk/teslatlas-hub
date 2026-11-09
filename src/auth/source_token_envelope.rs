// SPDX-License-Identifier: AGPL-3.0-only

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

pub const CLOAK_ENVELOPE_OVERHEAD_BYTES: usize = 2 + CLOAK_TAG.len() + NONCE_BYTES + AUTH_TAG_BYTES;
/// The established semantic limit for one plaintext owner token.
// Public cap bridge for the separate CLI crate; the sole value is defined by R7.
pub const MAX_TOKEN_PLAINTEXT_BYTES: usize =
    crate::auth::credential_envelope::MAX_TOKEN_PLAINTEXT_BYTES;
/// Shared with the source reader and matched by the Hub token store. This cap
/// includes the fixed Cloak V1 envelope around a maximum-size plaintext.
pub const MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES: usize =
    MAX_TOKEN_PLAINTEXT_BYTES + CLOAK_ENVELOPE_OVERHEAD_BYTES;

/// Decrypt the one legacy TeslaMate access/refresh pair. Both values are
/// authenticated independently; a malformed, stale, or tampered value is
/// never accepted as an Owner API credential.
pub fn decrypt_legacy_owner_tokens(
    encryption_key: &[u8],
    encrypted_access: &[u8],
    encrypted_refresh: &[u8],
) -> Result<OwnerTokens, SourceDbTokenError> {
    let cipher = cloak_cipher(encryption_key)?;
    let access = Zeroizing::new(decrypt_cloak_value(&cipher, encrypted_access)?);
    let refresh = Zeroizing::new(decrypt_cloak_value(&cipher, encrypted_refresh)?);
    let access = secret_utf8_copy(&access)?;
    let refresh = secret_utf8_copy(&refresh)?;
    OwnerTokens::from_secret_parts(access, refresh)
        .map_err(|_| SourceDbTokenError::InvalidPlaintext)
}



/// Stable, non-reversible identity of the single-use refresh authority.
/// Tesla refresh tokens are high-entropy secrets; the domain-separated digest
/// lets the durable fence recognize the same plaintext across random Cloak
/// nonces without storing or logging the token.
pub(crate) fn legacy_refresh_credential_generation(tokens: &OwnerTokens) -> uuid::Uuid {
    let refresh = tokens.refresh_token().as_bytes();
    let mut digest = Sha256::new();
    digest.update(b"teslatlas-hub:legacy-refresh-generation:v1\0");
    digest.update((refresh.len() as u64).to_le_bytes());
    digest.update(refresh);
    let bytes: [u8; 32] = digest.finalize().into();
    uuid::Uuid::from_bytes(bytes[..16].try_into().expect("fixed digest prefix"))
}

/// Non-reversible identity of one legacy access token (hub fix pass 3): the digest the
/// rejection fuse keeps of the access token it counted as rejected, so a restart can tell
/// whether the stored pair still holds that token. The same family as the refresh
/// generation above (a domain-separated SHA-256, 128 bits of it), under its own domain, so
/// it is never equal to a generation; it is never logged and `Debug` shows presence only.
///
/// The digest is of the canonical token, the one the load works with: the surrounding
/// whitespace the load trims (R46) is trimmed here too, so the construction, the fill of a
/// missing digest and the count of a rejection hash the same bytes whichever text they
/// hold (hub fix pass 4, F4-1). The digest stays unkeyed, like the generation: a key-derived
/// one would change when the key is replaced with the same plaintext, and a restart would
/// conclude that the rejected token was replaced.
pub(crate) fn legacy_access_token_digest(access_token: &str) -> uuid::Uuid {
    let access = crate::legacy_auth::trim_token_whitespace(access_token).as_bytes();
    let mut digest = Sha256::new();
    digest.update(b"teslatlas-hub:legacy-access-rejected:v1\0");
    digest.update((access.len() as u64).to_le_bytes());
    digest.update(access);
    let bytes: [u8; 32] = digest.finalize().into();
    uuid::Uuid::from_bytes(bytes[..16].try_into().expect("fixed digest prefix"))
}





fn secret_utf8_copy(bytes: &[u8]) -> Result<String, SourceDbTokenError> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| SourceDbTokenError::InvalidPlaintext)
}



fn cloak_cipher(encryption_key: &[u8]) -> Result<Aes256Gcm, SourceDbTokenError> {
    if encryption_key.is_empty() {
        return Err(SourceDbTokenError::EmptyEncryptionKey);
    }
    let key = Zeroizing::new(Sha256::digest(encryption_key).to_vec());
    Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| SourceDbTokenError::InvalidEncryptionKey)
}

fn decrypt_cloak_value(
    cipher: &Aes256Gcm,
    encrypted: &[u8],
) -> Result<Vec<u8>, SourceDbTokenError> {
    if encrypted.len() < 2 || encrypted[0] != CLOAK_TYPE {
        return Err(SourceDbTokenError::UnsupportedEnvelope);
    }
    let tag_bytes = usize::from(encrypted[1]);
    let nonce_start = 2_usize;
    let nonce_end = nonce_start
        .checked_add(tag_bytes)
        .ok_or(SourceDbTokenError::UnsupportedEnvelope)?;
    let auth_tag_start = nonce_end
        .checked_add(NONCE_BYTES)
        .ok_or(SourceDbTokenError::UnsupportedEnvelope)?;
    let ciphertext_start = auth_tag_start
        .checked_add(AUTH_TAG_BYTES)
        .ok_or(SourceDbTokenError::UnsupportedEnvelope)?;
    if tag_bytes != CLOAK_TAG.len()
        || encrypted.len() <= ciphertext_start
        || encrypted.get(nonce_start..nonce_end) != Some(CLOAK_TAG)
    {
        return Err(SourceDbTokenError::UnsupportedEnvelope);
    }

    let mut ciphertext = Zeroizing::new(Vec::with_capacity(
        encrypted.len() - ciphertext_start + AUTH_TAG_BYTES,
    ));
    ciphertext.extend_from_slice(&encrypted[ciphertext_start..]);
    ciphertext.extend_from_slice(&encrypted[auth_tag_start..ciphertext_start]);
    let nonce = Nonce::try_from(&encrypted[nonce_end..auth_tag_start])
        .map_err(|_| SourceDbTokenError::UnsupportedEnvelope)?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext.as_slice(),
                aad: ASSOCIATED_DATA,
            },
        )
        .map_err(|_| SourceDbTokenError::AuthenticationFailed)
}



#[derive(Debug, Error)]
pub enum SourceDbTokenError {
    #[error("operating-system entropy is unavailable for legacy owner token encryption")]
    EntropyUnavailable,
    #[error("source token key is empty")]
    EmptyEncryptionKey,
    #[error("source token key is invalid")]
    InvalidEncryptionKey,
    #[error("Legacy owner token envelope is unsupported")]
    UnsupportedEnvelope,
    #[error("Legacy owner token authentication failed")]
    AuthenticationFailed,
    #[error("Legacy owner token encryption failed")]
    EncryptionFailed,
    #[error("Legacy owner token plaintext is invalid")]
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
        ASSOCIATED_DATA, AUTH_TAG_BYTES, CLOAK_TAG, CLOAK_TYPE, MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES,
        MAX_TOKEN_PLAINTEXT_BYTES, NONCE_BYTES, SourceDbTokenError,
        decrypt_legacy_owner_tokens,  
        legacy_access_token_digest, legacy_refresh_credential_generation,
    };
    use crate::credentials::OwnerTokens;

    const ENCRYPTION_KEY: &[u8] = b"source-test-token-key-v1";
    const WRONG_ENCRYPTION_KEY: &[u8] = b"source-test-token-key-v2";
    const ACCESS_TOKEN: &[u8] = b"access-token-deterministic-01";
    const REFRESH_TOKEN: &[u8] = b"refresh-token-deterministic-01";

    // Generated independently with Python cryptography AESGCM using the raw
    // key SHA-256, this nonce, and AAD `AES256GCM`. It is a fixed TeslaMate
    // Cloak V1 envelope: type, tag length, tag, IV, auth tag, ciphertext.
    const EXTERNAL_CLOAK_ACCESS: [u8; 69] = [
        0x01, 0x0a, 0x41, 0x45, 0x53, 0x2e, 0x47, 0x43, 0x4d, 0x2e, 0x56, 0x31, 0x01, 0x02, 0x03,
        0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x71, 0x65, 0x02, 0x62, 0x6a, 0x2f,
        0xa7, 0xb8, 0xa5, 0x36, 0x2d, 0xd0, 0x91, 0x49, 0x17, 0x89, 0xbc, 0x8b, 0x66, 0x36, 0x0b,
        0xe3, 0xe8, 0x66, 0x59, 0xc4, 0x78, 0xd4, 0x88, 0xe1, 0x57, 0x78, 0x32, 0x5c, 0x85, 0xa2,
        0x7a, 0x5f, 0x31, 0xef, 0x8a, 0xcc, 0x98, 0x8d, 0xda,
    ];



    #[test]
    fn decrypts_deterministic_cloak_access_refresh_pair_with_exact_key_bytes() {
        panic!("C10_UNAVAILABLE_E: unaccepted candidate; removed credential table dependency");
    }

    #[test]
    fn encrypts_legacy_pair_read_from_newline_terminated_files() {
        panic!("C10_UNAVAILABLE_E: unaccepted candidate; removed credential table dependency");
    }

    #[test]
    fn envelope_exact_plaintext_cap_persists_and_next_byte_is_rejected() {
        assert_eq!(MAX_TOKEN_PLAINTEXT_BYTES, 16 * 1024);
        assert_eq!(
            MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES,
            MAX_TOKEN_PLAINTEXT_BYTES + super::CLOAK_ENVELOPE_OVERHEAD_BYTES
        );
        let access = Zeroizing::new(vec![b'a'; MAX_TOKEN_PLAINTEXT_BYTES]);
        let refresh = Zeroizing::new(vec![b'b'; MAX_TOKEN_PLAINTEXT_BYTES]);
        let (access_ciphertext, refresh_ciphertext) =
            ({ let encrypt_legacy_owner_token_files: fn(&[u8], Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>) -> Result<(Vec<u8>, Vec<u8>), SourceDbTokenError> = panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency"); encrypt_legacy_owner_token_files(ENCRYPTION_KEY, access, refresh) })
                .expect("exact plaintext cap encrypts");
        assert_eq!(access_ciphertext.len(), MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES);
        assert_eq!(refresh_ciphertext.len(), MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES);
        let stored =
            ({ let unavailable_value: Result<(Vec<u8>, Vec<u8>, i64, i64, Option<uuid::Uuid>, String, String), crate::db::StoreError> = panic!("C10_UNAVAILABLE_E: unaccepted candidate; removed credential table dependency"); unavailable_value })
                .expect("exact envelope cap persists");
        assert_eq!(
            decrypt_legacy_owner_tokens(ENCRYPTION_KEY, stored.0.as_slice(), stored.1.as_slice())
                .expect("stored cap decrypts")
                .access_token()
                .len(),
            MAX_TOKEN_PLAINTEXT_BYTES
        );

        assert!(matches!(
            ({ let encrypt_legacy_owner_token_files: fn(&[u8], Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>) -> Result<(Vec<u8>, Vec<u8>), SourceDbTokenError> = panic!("C10_UNAVAILABLE_V2: unaccepted candidate; removed credential dependency"); encrypt_legacy_owner_token_files(
                ENCRYPTION_KEY,
                Zeroizing::new(vec![b'a'; MAX_TOKEN_PLAINTEXT_BYTES + 1]),
                Zeroizing::new(vec![b'b'; MAX_TOKEN_PLAINTEXT_BYTES]),
            ) }),
            Err(SourceDbTokenError::InvalidPlaintext)
        ));
    }

    #[test]
    fn rejects_deterministic_cloak_pair_with_wrong_key() {
        panic!("C10_UNAVAILABLE_E: unaccepted candidate; removed credential table dependency");
    }

    #[test]
    fn rejects_deterministic_cloak_pair_with_altered_authenticated_byte() {
        panic!("C10_UNAVAILABLE_E: unaccepted candidate; removed credential table dependency");
    }

    #[test]
    fn encrypts_source_db_cloak_pair_that_round_trips() {
        panic!("C10_UNAVAILABLE_E: unaccepted candidate; removed credential table dependency");
    }

    #[test]
    fn encrypts_with_distinct_cryptographic_nonces() {
        panic!("C10_UNAVAILABLE_E: unaccepted candidate; removed credential table dependency");
    }

    #[test]
    fn legacy_refresh_credential_generation_has_stable_v1_vector_and_ignores_access_token() {
        let first = OwnerTokens::from_secret_parts(
            "access-one".to_owned(),
            "stable-refresh-token-v1".to_owned(),
        )
        .expect("first pair");
        let same_refresh = OwnerTokens::from_secret_parts(
            "different-access".to_owned(),
            "stable-refresh-token-v1".to_owned(),
        )
        .expect("same refresh pair");
        let different_refresh = OwnerTokens::from_secret_parts(
            "access-one".to_owned(),
            "different-refresh-token".to_owned(),
        )
        .expect("different refresh pair");

        let generation = legacy_refresh_credential_generation(&first);
        assert_eq!(
            generation,
            uuid::Uuid::parse_str("5dc0cafc-ff0e-c09b-efb5-3ce6534ad2c9")
                .expect("golden generation")
        );
        assert_eq!(
            generation,
            legacy_refresh_credential_generation(&same_refresh)
        );
        assert_ne!(
            generation,
            legacy_refresh_credential_generation(&different_refresh)
        );
    }
    #[test]
    fn legacy_access_token_digest_is_stable_domain_separated_and_tells_tokens_apart() {
        let digest = legacy_access_token_digest("access-one");
        assert_eq!(digest, legacy_access_token_digest("access-one"));
        assert_ne!(digest, legacy_access_token_digest("access-two"));
        // Never a refresh generation of the same text.
        let as_refresh =
            OwnerTokens::from_secret_parts("a".to_owned(), "access-one".to_owned()).expect("pair");
        assert_ne!(digest, legacy_refresh_credential_generation(&as_refresh));
        // The text itself is not in the digest's form.
        assert!(!digest.to_string().contains("access"));
    }

    #[test]
    fn legacy_access_token_digest_is_of_the_trimmed_token() {
        // F4-1: the surrounding whitespace the load trims (R46) is not part of the identity.
        let digest = legacy_access_token_digest("access-one");
        assert_eq!(digest, legacy_access_token_digest("  access-one \t\r\n"));
        // Whitespace inside the token is part of it.
        assert_ne!(digest, legacy_access_token_digest("access- one"));
    }
}

#[cfg(test)]
mod n1_src_auth_source_token_envelope_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free};

    impl ErrorExample for SourceDbTokenError {
        fn error_example() -> Self {
            SourceDbTokenError::EntropyUnavailable
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_source_db_token_error(error: SourceDbTokenError) {
            match &error {
                SourceDbTokenError::EntropyUnavailable => (),
                SourceDbTokenError::EmptyEncryptionKey => (),
                SourceDbTokenError::InvalidEncryptionKey => (),
                SourceDbTokenError::UnsupportedEnvelope => (),
                SourceDbTokenError::AuthenticationFailed => (),
                SourceDbTokenError::EncryptionFailed => (),
                SourceDbTokenError::InvalidPlaintext => (),
            }
            assert_name_free(&error);
        }
        check_source_db_token_error(SourceDbTokenError::EntropyUnavailable);
        check_source_db_token_error(SourceDbTokenError::EmptyEncryptionKey);
        check_source_db_token_error(SourceDbTokenError::InvalidEncryptionKey);
        check_source_db_token_error(SourceDbTokenError::UnsupportedEnvelope);
        check_source_db_token_error(SourceDbTokenError::AuthenticationFailed);
        check_source_db_token_error(SourceDbTokenError::EncryptionFailed);
        check_source_db_token_error(SourceDbTokenError::InvalidPlaintext);
    }
}
