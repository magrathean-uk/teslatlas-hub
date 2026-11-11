// SPDX-License-Identifier: AGPL-3.0-only

//! The password-based stream-encrypted archive container (M06-T01).
//!
//! One archive file holds one backup generation. Layout:
//!
//! ```text
//! header (80 bytes, plaintext, authenticated as the AAD of every chunk)
//!   magic            8   "TLATLBK1"
//!   version          1   1
//!   kdf              1   1 = PBKDF2-HMAC-SHA-256
//!   cipher           1   1 = AES-256-GCM, STREAM chunking
//!   reserved         1   0
//!   iterations       4   big endian
//!   chunk bytes      4   big endian, plaintext bytes per full chunk
//!   reserved         4   0
//!   salt            16
//!   nonce prefix     7
//!   reserved         1   0
//!   key check       32   HMAC-SHA-256(master, label || bytes 0..48)
//! chunks: each is AES-256-GCM(plaintext chunk) followed by its 16-byte tag
//! ```
//!
//! The chunk nonce is the STREAM construction: the 7-byte prefix, a 32-bit
//! big-endian chunk counter and one flag byte that is 1 only on the final
//! chunk. A reordered, dropped, duplicated, extended or truncated chunk
//! therefore fails authentication. The header is bound as additional data, so
//! its parameters cannot be changed without detection.
//!
//! The key is derived from the user-kept recovery password with
//! PBKDF2-HMAC-SHA-256 (RFC 8018). The crate carries no memory-hard KDF, so the
//! iteration count carries the work factor; the generated recovery password has
//! 160 bits of entropy, which makes an offline search infeasible whatever the
//! KDF.

use std::{
    io::{self, Read, Write},
    path::PathBuf,
};

use aes_gcm::{
    Aes256Gcm, Nonce, Tag,
    aead::{AeadInOut, KeyInit},
};
use zeroize::{Zeroize, Zeroizing};

use super::{DataRecoveryError, invalid, io_error};
use crate::protocol::{constant_time_eq, hmac_sha256};

const MAGIC: &[u8; 8] = b"TLATLBK1";
const FORMAT_VERSION: u8 = 1;
const KDF_PBKDF2_SHA256: u8 = 1;
const CIPHER_AES256GCM_STREAM: u8 = 1;
const FIXED_HEADER_BYTES: usize = 48;
pub(super) const HEADER_BYTES: usize = FIXED_HEADER_BYTES + 32;
const SALT_BYTES: usize = 16;
const NONCE_PREFIX_BYTES: usize = 7;
const TAG_BYTES: usize = 16;

/// Plaintext bytes per full chunk in production.
pub(super) const DEFAULT_CHUNK_BYTES: usize = 1024 * 1024;
const MIN_CHUNK_BYTES: usize = if cfg!(test) { 16 } else { 64 * 1024 };
const MAX_CHUNK_BYTES: usize = 8 * 1024 * 1024;
/// OWASP's 2023 floor for PBKDF2-HMAC-SHA-256 is 600,000 iterations.
pub(super) const DEFAULT_KDF_ITERATIONS: u32 = 600_000;
const MIN_KDF_ITERATIONS: u32 = if cfg!(test) { 1 } else { 100_000 };
const MAX_KDF_ITERATIONS: u32 = 20_000_000;

pub(super) const MIN_PASSWORD_CHARS: usize = 12;

const KEY_LABEL: &[u8] = b"teslatlas-hub-backup-v1 encryption key";
const CHECK_LABEL: &[u8] = b"teslatlas-hub-backup-v1 key check";

/// RFC 8018 PBKDF2 with HMAC-SHA-256 and a 32-byte output (one block).
pub(super) fn pbkdf2_hmac_sha256_32(
    password: &[u8],
    salt: &[u8],
    iterations: u32,
) -> Zeroizing<[u8; 32]> {
    let mut first = Zeroizing::new(salt.to_vec());
    first.extend_from_slice(&1_u32.to_be_bytes());
    let mut block = hmac_sha256(password, &first);
    let mut accumulator = Zeroizing::new(block);
    for _ in 1..iterations {
        block = hmac_sha256(password, &block);
        for (target, byte) in accumulator.iter_mut().zip(block.iter()) {
            *target ^= byte;
        }
    }
    block.zeroize();
    accumulator
}

struct Keys {
    encryption: Zeroizing<[u8; 32]>,
    check: [u8; 32],
}

fn derive_keys(password: &str, fixed_header: &[u8], iterations: u32) -> Keys {
    let master = pbkdf2_hmac_sha256_32(
        password.as_bytes(),
        &fixed_header[24..24 + SALT_BYTES],
        iterations,
    );
    let encryption = Zeroizing::new(hmac_sha256(master.as_slice(), KEY_LABEL));
    let mut check_input = CHECK_LABEL.to_vec();
    check_input.extend_from_slice(fixed_header);
    let check = hmac_sha256(master.as_slice(), &check_input);
    Keys { encryption, check }
}

fn chunk_nonce(prefix: &[u8; NONCE_PREFIX_BYTES], counter: u32, last: bool) -> [u8; 12] {
    let mut nonce = [0_u8; 12];
    nonce[..NONCE_PREFIX_BYTES].copy_from_slice(prefix);
    nonce[NONCE_PREFIX_BYTES..NONCE_PREFIX_BYTES + 4].copy_from_slice(&counter.to_be_bytes());
    nonce[11] = u8::from(last);
    nonce
}

/// A random 160-bit recovery password in eight dash-separated groups of four
/// base32 characters. The Hub shows it once; the user keeps it.
pub fn generate_recovery_password() -> Result<String, DataRecoveryError> {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut entropy = Zeroizing::new([0_u8; 20]);
    getrandom::fill(entropy.as_mut_slice())
        .map_err(|_| invalid("system entropy is unavailable for the recovery password"))?;
    let mut characters = Vec::with_capacity(32);
    let mut bits = 0_u32;
    let mut width = 0_u32;
    for byte in entropy.iter() {
        bits = (bits << 8) | u32::from(*byte);
        width += 8;
        while width >= 5 {
            width -= 5;
            characters.push(ALPHABET[((bits >> width) & 31) as usize]);
        }
    }
    let text = String::from_utf8(characters).map_err(|_| invalid("recovery password encoding"))?;
    Ok(text
        .as_bytes()
        .chunks(4)
        .map(|group| String::from_utf8_lossy(group).into_owned())
        .collect::<Vec<_>>()
        .join("-"))
}

pub(super) struct ArchiveWriter<W: Write> {
    inner: W,
    path: PathBuf,
    cipher: Aes256Gcm,
    header: [u8; HEADER_BYTES],
    prefix: [u8; NONCE_PREFIX_BYTES],
    counter: u32,
    chunk_bytes: usize,
    buffer: Vec<u8>,
}

impl<W: Write> ArchiveWriter<W> {
    pub(super) fn start(
        mut inner: W,
        path: PathBuf,
        password: &str,
        iterations: u32,
        chunk_bytes: usize,
    ) -> Result<Self, DataRecoveryError> {
        if password.chars().count() < MIN_PASSWORD_CHARS {
            return Err(invalid(format!(
                "the backup password must have at least {MIN_PASSWORD_CHARS} characters"
            )));
        }
        if !(MIN_KDF_ITERATIONS..=MAX_KDF_ITERATIONS).contains(&iterations)
            || !(MIN_CHUNK_BYTES..=MAX_CHUNK_BYTES).contains(&chunk_bytes)
        {
            return Err(invalid("backup archive parameters are out of range"));
        }
        let mut fixed = [0_u8; FIXED_HEADER_BYTES];
        fixed[..8].copy_from_slice(MAGIC);
        fixed[8] = FORMAT_VERSION;
        fixed[9] = KDF_PBKDF2_SHA256;
        fixed[10] = CIPHER_AES256GCM_STREAM;
        fixed[12..16].copy_from_slice(&iterations.to_be_bytes());
        let chunk_field = u32::try_from(chunk_bytes).map_err(|_| invalid("chunk size"))?;
        fixed[16..20].copy_from_slice(&chunk_field.to_be_bytes());
        // Bytes 20..24 are reserved zero; the salt starts at 24 so that the
        // key derivation and the header layout agree.
        getrandom::fill(&mut fixed[24..24 + SALT_BYTES])
            .and_then(|()| getrandom::fill(&mut fixed[40..40 + NONCE_PREFIX_BYTES]))
            .map_err(|_| invalid("system entropy is unavailable for the backup archive"))?;
        let keys = derive_keys(password, &fixed, iterations);
        let mut header = [0_u8; HEADER_BYTES];
        header[..FIXED_HEADER_BYTES].copy_from_slice(&fixed);
        header[FIXED_HEADER_BYTES..].copy_from_slice(&keys.check);
        let mut prefix = [0_u8; NONCE_PREFIX_BYTES];
        prefix.copy_from_slice(&fixed[40..40 + NONCE_PREFIX_BYTES]);
        let cipher = Aes256Gcm::new((&*keys.encryption).into());
        inner
            .write_all(&header)
            .map_err(|source| map_write_error(&path, source))?;
        Ok(Self {
            inner,
            path,
            cipher,
            header,
            prefix,
            counter: 0,
            chunk_bytes,
            buffer: Vec::with_capacity(chunk_bytes),
        })
    }

    pub(super) fn write_plain(&mut self, mut data: &[u8]) -> Result<(), DataRecoveryError> {
        while !data.is_empty() {
            if self.buffer.len() == self.chunk_bytes {
                self.seal(false)?;
            }
            let take = (self.chunk_bytes - self.buffer.len()).min(data.len());
            self.buffer.extend_from_slice(&data[..take]);
            data = &data[take..];
        }
        Ok(())
    }

    fn seal(&mut self, last: bool) -> Result<(), DataRecoveryError> {
        let nonce = chunk_nonce(&self.prefix, self.counter, last);
        let tag = self
            .cipher
            .encrypt_inout_detached(
                &Nonce::from(nonce),
                &self.header,
                self.buffer.as_mut_slice().into(),
            )
            .map_err(|_| invalid("backup chunk encryption failed"))?;
        self.inner
            .write_all(&self.buffer)
            .and_then(|()| self.inner.write_all(&tag))
            .map_err(|source| map_write_error(&self.path, source))?;
        self.buffer.clear();
        if !last {
            self.counter = self
                .counter
                .checked_add(1)
                .filter(|counter| *counter < u32::MAX)
                .ok_or_else(|| invalid("backup archive exceeds its chunk counter"))?;
        }
        Ok(())
    }

    /// Seal the final chunk and hand the sink back for syncing.
    pub(super) fn finish(mut self) -> Result<W, DataRecoveryError> {
        self.seal(true)?;
        self.inner
            .flush()
            .map_err(|source| map_write_error(&self.path, source))?;
        Ok(self.inner)
    }
}

/// A full destination is named as such; every other write failure keeps its
/// operation and path.
pub(super) fn map_write_error(path: &std::path::Path, source: io::Error) -> DataRecoveryError {
    if source.kind() == io::ErrorKind::StorageFull || source.raw_os_error() == Some(28) {
        DataRecoveryError::DestinationFull {
            path: path.to_path_buf(),
        }
    } else {
        io_error("writing backup archive", path, source)
    }
}

pub(super) struct ArchiveReader<R: Read> {
    inner: R,
    path: PathBuf,
    cipher: Aes256Gcm,
    header: [u8; HEADER_BYTES],
    prefix: [u8; NONCE_PREFIX_BYTES],
    counter: u32,
    chunk_bytes: usize,
    peeked: Option<u8>,
    plain: Zeroizing<Vec<u8>>,
    position: usize,
    finished: bool,
}

impl<R: Read> ArchiveReader<R> {
    pub(super) fn open(
        mut inner: R,
        path: PathBuf,
        password: &str,
    ) -> Result<Self, DataRecoveryError> {
        let mut header = [0_u8; HEADER_BYTES];
        read_full(&mut inner, &mut header, &path)?
            .then_some(())
            .ok_or_else(|| {
                DataRecoveryError::CorruptArchive(
                    "the file is too short to be a backup archive".into(),
                )
            })?;
        if &header[..8] != MAGIC {
            return Err(DataRecoveryError::CorruptArchive(
                "the file is not a Teslatlas Hub backup archive".into(),
            ));
        }
        if header[8] != FORMAT_VERSION
            || header[9] != KDF_PBKDF2_SHA256
            || header[10] != CIPHER_AES256GCM_STREAM
            || header[11] != 0
            || header[20..24] != [0; 4]
            || header[FIXED_HEADER_BYTES - 1] != 0
        {
            return Err(DataRecoveryError::CorruptArchive(
                "unsupported backup archive version".into(),
            ));
        }
        let iterations = u32::from_be_bytes(header[12..16].try_into().expect("four bytes"));
        let chunk_bytes = usize::try_from(u32::from_be_bytes(
            header[16..20].try_into().expect("four bytes"),
        ))
        .map_err(|_| invalid("chunk size"))?;
        if !(MIN_KDF_ITERATIONS..=MAX_KDF_ITERATIONS).contains(&iterations)
            || !(MIN_CHUNK_BYTES..=MAX_CHUNK_BYTES).contains(&chunk_bytes)
        {
            return Err(DataRecoveryError::CorruptArchive(
                "backup archive parameters are out of range".into(),
            ));
        }
        let keys = derive_keys(password, &header[..FIXED_HEADER_BYTES], iterations);
        if !constant_time_eq(&keys.check, &header[FIXED_HEADER_BYTES..]) {
            return Err(DataRecoveryError::WrongPassword);
        }
        let mut prefix = [0_u8; NONCE_PREFIX_BYTES];
        prefix.copy_from_slice(&header[40..40 + NONCE_PREFIX_BYTES]);
        Ok(Self {
            inner,
            path,
            cipher: Aes256Gcm::new((&*keys.encryption).into()),
            header,
            prefix,
            counter: 0,
            chunk_bytes,
            peeked: None,
            plain: Zeroizing::new(Vec::new()),
            position: 0,
            finished: false,
        })
    }

    /// Decrypt the next chunk into `plain`; false when the archive is over.
    fn fill(&mut self) -> Result<bool, DataRecoveryError> {
        if self.finished {
            return Ok(false);
        }
        let wanted = self.chunk_bytes + TAG_BYTES;
        let mut chunk = Zeroizing::new(vec![0_u8; wanted]);
        let mut have = 0;
        if let Some(byte) = self.peeked.take() {
            chunk[0] = byte;
            have = 1;
        }
        have += read_some(&mut self.inner, &mut chunk[have..], &self.path)?;
        let last = if have < wanted {
            true
        } else {
            let mut probe = [0_u8; 1];
            if read_some(&mut self.inner, &mut probe, &self.path)? == 0 {
                true
            } else {
                self.peeked = Some(probe[0]);
                false
            }
        };
        if have < TAG_BYTES {
            return Err(truncated());
        }
        chunk.truncate(have);
        let tag_start = have - TAG_BYTES;
        let tag = Tag::from(<[u8; TAG_BYTES]>::try_from(&chunk[tag_start..]).expect("tag width"));
        chunk.truncate(tag_start);
        let nonce = chunk_nonce(&self.prefix, self.counter, last);
        self.cipher
            .decrypt_inout_detached(
                &Nonce::from(nonce),
                &self.header,
                chunk.as_mut_slice().into(),
                &tag,
            )
            .map_err(|_| {
                DataRecoveryError::CorruptArchive(format!(
                    "chunk {} failed authentication: the archive is damaged, truncated or extended",
                    self.counter
                ))
            })?;
        self.plain = chunk;
        self.position = 0;
        if last {
            self.finished = true;
        } else {
            self.counter = self
                .counter
                .checked_add(1)
                .ok_or_else(|| invalid("backup archive exceeds its chunk counter"))?;
        }
        Ok(true)
    }

    /// Fill `out` exactly from the authenticated plaintext stream.
    pub(super) fn read_exact_plain(&mut self, out: &mut [u8]) -> Result<(), DataRecoveryError> {
        let mut done = 0;
        while done < out.len() {
            if self.position == self.plain.len() && !self.fill()? {
                return Err(truncated());
            }
            let take = (self.plain.len() - self.position).min(out.len() - done);
            out[done..done + take]
                .copy_from_slice(&self.plain[self.position..self.position + take]);
            self.position += take;
            done += take;
        }
        Ok(())
    }

    /// After the last record: no plaintext may remain and no chunk may follow.
    pub(super) fn expect_end(&mut self) -> Result<(), DataRecoveryError> {
        if self.position < self.plain.len() || self.fill()? {
            return Err(DataRecoveryError::CorruptArchive(
                "unexpected data after the final record".into(),
            ));
        }
        Ok(())
    }
}

fn truncated() -> DataRecoveryError {
    DataRecoveryError::CorruptArchive("the archive ends early; it is truncated".into())
}

fn read_some<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
    path: &std::path::Path,
) -> Result<usize, DataRecoveryError> {
    let mut have = 0;
    while have < buffer.len() {
        match reader.read(&mut buffer[have..]) {
            Ok(0) => break,
            Ok(read) => have += read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(io_error("reading backup archive", path, source)),
        }
    }
    Ok(have)
}

fn read_full<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
    path: &std::path::Path,
) -> Result<bool, DataRecoveryError> {
    Ok(read_some(reader, buffer, path)? == buffer.len())
}
