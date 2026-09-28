use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngExt;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

const MAGIC: &[u8; 8] = b"TLEDGE01";
const KEY_ID_BYTES: usize = 8;
const NONCE_BYTES: usize = 24;
const HEADER_BYTES: usize = MAGIC.len() + KEY_ID_BYTES + NONCE_BYTES;

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub(crate) struct EncryptionKey([u8; 32]);

impl EncryptionKey {
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut nonce_bytes = [0_u8; NONCE_BYTES];
        rand::rng().fill(&mut nonce_bytes);
        let nonce = XNonce::try_from(&nonce_bytes[..]).expect("XChaCha nonce has fixed length");
        let cipher = self.cipher();
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: MAGIC,
                },
            )
            .map_err(|_| CryptoError::InvalidCiphertext)?;

        let mut output = Vec::with_capacity(HEADER_BYTES + ciphertext.len());
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&self.key_id());
        output.extend_from_slice(&nonce);
        output.extend_from_slice(&ciphertext);
        nonce_bytes.zeroize();
        Ok(output)
    }

    pub(crate) fn decrypt(&self, input: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if input.len() <= HEADER_BYTES || &input[..MAGIC.len()] != MAGIC {
            return Err(CryptoError::InvalidCiphertext);
        }
        let key_id_start = MAGIC.len();
        let nonce_start = key_id_start + KEY_ID_BYTES;
        let ciphertext_start = nonce_start + NONCE_BYTES;
        if input[key_id_start..nonce_start] != self.key_id() {
            return Err(CryptoError::KeyMismatch);
        }
        let nonce = XNonce::try_from(&input[nonce_start..ciphertext_start])
            .expect("validated XChaCha nonce has fixed length");
        let cipher = self.cipher();
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &input[ciphertext_start..],
                    aad: MAGIC,
                },
            )
            .map_err(|_| CryptoError::InvalidCiphertext)
    }

    /// Builds the cipher straight from the key bytes, so no separate key copy is
    /// left on the stack; the cipher wipes its own copy on drop.
    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new_from_slice(&self.0).expect("encryption key has fixed length")
    }

    fn key_id(&self) -> [u8; KEY_ID_BYTES] {
        let digest = Sha256::digest(self.0);
        let mut key_id = [0_u8; KEY_ID_BYTES];
        key_id.copy_from_slice(&digest[..KEY_ID_BYTES]);
        key_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum CryptoError {
    #[error("the spool key does not match pending records")]
    KeyMismatch,
    #[error("invalid encrypted spool record")]
    InvalidCiphertext,
}
