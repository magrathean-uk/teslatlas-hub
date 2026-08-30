use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::RngCore;
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
        let mut nonce = [0_u8; NONCE_BYTES];
        rand::rng().fill_bytes(&mut nonce);
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&self.0));
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
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
        nonce.zeroize();
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
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&self.0));
        cipher
            .decrypt(
                XNonce::from_slice(&input[nonce_start..ciphertext_start]),
                Payload {
                    msg: &input[ciphertext_start..],
                    aad: MAGIC,
                },
            )
            .map_err(|_| CryptoError::InvalidCiphertext)
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
