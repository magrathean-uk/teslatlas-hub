// SPDX-License-Identifier: AGPL-3.0-only

//! The password-based stream-encrypted archive container (M06-T01).
//!
//! One archive file holds one backup generation. The Backups decision names
//! the password-based format: Argon2id (RFC 9106) derives the key from the
//! user-kept recovery password, and the payload is the STREAM construction
//! (Hoang, Reyhanitabar, Rogaway and Vizár, 2015) over AES-256-GCM, the same
//! online-AEAD shape that age uses for its payload. Layout:
//!
//! ```text
//! header (88 bytes, plaintext, authenticated as the AAD of every chunk)
//!   magic            8   "TLATLBK1"
//!   version          1   2
//!   kdf              1   2 = Argon2id, version 0x13 (RFC 9106)
//!   cipher           1   1 = AES-256-GCM, STREAM chunking
//!   reserved         1   0
//!   memory KiB       4   big endian, Argon2id m
//!   passes           4   big endian, Argon2id t
//!   lanes            4   big endian, Argon2id p
//!   chunk bytes      4   big endian, plaintext bytes per full chunk
//!   reserved         4   0
//!   salt            16
//!   nonce prefix     7
//!   reserved         1   0
//!   key check       32   HMAC-SHA-256(master, label || bytes 0..56)
//! chunks: each is AES-256-GCM(plaintext chunk) followed by its 16-byte tag
//! ```
//!
//! The chunk nonce is the STREAM nonce: the 7-byte prefix, a 32-bit big-endian
//! chunk counter and one flag byte that is 1 only on the final chunk. A
//! reordered, dropped, duplicated, extended or truncated chunk therefore fails
//! authentication. The header is bound as additional data, so its parameters
//! cannot be changed without detection.
//!
//! Memory. The default Argon2id cost is the second recommended option of
//! RFC 9106 section 4 (m = 64 MiB, t = 3, p = 4). A reader refuses any header
//! asking for more than [`MAX_KDF_MEMORY_KIB`], so a crafted archive cannot
//! push the Hub past its 1 GiB peak; [`super::encrypted_backup`] counts that
//! maximum against the peak beside the in-memory catalogue copy.

use std::{
    io::{self, Read, Write},
    path::PathBuf,
};

use aes_gcm::{
    Aes256Gcm, Nonce, Tag,
    aead::{AeadInOut, KeyInit},
};
use argon2::{Algorithm, Argon2, AssociatedData, Block, ParamsBuilder, Version};
use zeroize::{Zeroize, Zeroizing};

use super::{DataRecoveryError, invalid, io_error};
use crate::protocol::{constant_time_eq, hmac_sha256};

const MAGIC: &[u8; 8] = b"TLATLBK1";
const FORMAT_VERSION: u8 = 2;
const KDF_ARGON2ID_V13: u8 = 2;
const CIPHER_AES256GCM_STREAM: u8 = 1;
const FIXED_HEADER_BYTES: usize = 56;
pub(super) const HEADER_BYTES: usize = FIXED_HEADER_BYTES + 32;
pub(super) const SALT_OFFSET: usize = 32;
const SALT_BYTES: usize = 16;
const PREFIX_OFFSET: usize = SALT_OFFSET + SALT_BYTES;
const NONCE_PREFIX_BYTES: usize = 7;
pub(super) const TAG_BYTES: usize = 16;

/// Plaintext bytes per full chunk in production.
pub(super) const DEFAULT_CHUNK_BYTES: usize = 1024 * 1024;
const MIN_CHUNK_BYTES: usize = if cfg!(test) { 16 } else { 64 * 1024 };
pub(super) const MAX_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// Argon2id cost parameters as the header stores them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KdfParams {
    pub memory_kib: u32,
    pub passes: u32,
    pub lanes: u32,
}

/// RFC 9106 section 4, second recommended option.
pub(crate) const DEFAULT_KDF: KdfParams = KdfParams {
    memory_kib: 64 * 1024,
    passes: 3,
    lanes: 4,
};
/// The most memory a header may ask for (256 MiB).
pub(super) const MAX_KDF_MEMORY_KIB: u32 = 256 * 1024;
/// OWASP's Argon2id floor is 19 MiB; tests run far below it for speed.
const MIN_KDF_MEMORY_KIB: u32 = if cfg!(test) { 8 } else { 19 * 1024 };
const MAX_KDF_PASSES: u32 = 10;
const MAX_KDF_LANES: u32 = 8;

pub(super) const MIN_PASSWORD_CHARS: usize = 12;

const KEY_LABEL: &[u8] = b"teslatlas-hub-backup-v2 encryption key";
const CHECK_LABEL: &[u8] = b"teslatlas-hub-backup-v2 key check";

fn kdf_params_in_range(params: KdfParams) -> bool {
    (MIN_KDF_MEMORY_KIB..=MAX_KDF_MEMORY_KIB).contains(&params.memory_kib)
        && (1..=MAX_KDF_PASSES).contains(&params.passes)
        && (1..=MAX_KDF_LANES).contains(&params.lanes)
        && params.memory_kib >= 8 * params.lanes
}

/// Argon2id version 0x13 into `out`. `secret` and `data` are RFC 9106's
/// optional K and X inputs; the archive passes both empty, and the published
/// test vector exercises them. The working memory is zeroed before it is
/// freed.
pub(super) fn argon2id(
    password: &[u8],
    salt: &[u8],
    secret: &[u8],
    data: &[u8],
    params: KdfParams,
    out: &mut [u8],
) -> Result<(), DataRecoveryError> {
    let refused = |_| invalid("the backup key derivation parameters are not usable");
    let mut builder = ParamsBuilder::new();
    builder
        .m_cost(params.memory_kib)
        .t_cost(params.passes)
        .p_cost(params.lanes)
        .output_len(out.len());
    if !data.is_empty() {
        builder.data(AssociatedData::new(data).map_err(refused)?);
    }
    let argon2_params = builder.build().map_err(refused)?;
    let context = if secret.is_empty() {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params.clone())
    } else {
        Argon2::new_with_secret(
            secret,
            Algorithm::Argon2id,
            Version::V0x13,
            argon2_params.clone(),
        )
        .map_err(refused)?
    };
    let mut blocks = vec![Block::new(); argon2_params.block_count()];
    let result = context
        .hash_password_into_with_memory(password, salt, out, blocks.as_mut_slice())
        .map_err(refused);
    blocks.zeroize();
    result
}

struct Keys {
    encryption: Zeroizing<[u8; 32]>,
    check: [u8; 32],
}

fn derive_keys(
    password: &str,
    fixed_header: &[u8],
    params: KdfParams,
) -> Result<Keys, DataRecoveryError> {
    let mut master = Zeroizing::new([0_u8; 32]);
    argon2id(
        password.as_bytes(),
        &fixed_header[SALT_OFFSET..SALT_OFFSET + SALT_BYTES],
        &[],
        &[],
        params,
        master.as_mut_slice(),
    )?;
    let encryption = Zeroizing::new(hmac_sha256(master.as_slice(), KEY_LABEL));
    let mut check_input = CHECK_LABEL.to_vec();
    check_input.extend_from_slice(fixed_header);
    let check = hmac_sha256(master.as_slice(), &check_input);
    Ok(Keys { encryption, check })
}

#[cfg(test)]
pub(super) fn derive_keys_for_test(
    password: &str,
    fixed_header: &[u8],
    params: KdfParams,
) -> ([u8; 32], [u8; 32]) {
    let keys = derive_keys(password, fixed_header, params).expect("test keys");
    (*keys.encryption, keys.check)
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

/// The derived key and header of one new archive. Deriving it is the costly,
/// memory-hard step, so a run does it before it takes any store lock; the
/// Argon2id memory is freed when this returns.
pub(super) struct SealingKey {
    cipher: Aes256Gcm,
    header: [u8; HEADER_BYTES],
    prefix: [u8; NONCE_PREFIX_BYTES],
    chunk_bytes: usize,
}

impl SealingKey {
    pub(super) fn derive(
        password: &str,
        params: KdfParams,
        chunk_bytes: usize,
    ) -> Result<Self, DataRecoveryError> {
        if password.chars().count() < MIN_PASSWORD_CHARS {
            return Err(invalid(format!(
                "the backup password must have at least {MIN_PASSWORD_CHARS} characters"
            )));
        }
        if !kdf_params_in_range(params)
            || !(MIN_CHUNK_BYTES..=MAX_CHUNK_BYTES).contains(&chunk_bytes)
        {
            return Err(invalid("backup archive parameters are out of range"));
        }
        let mut fixed = [0_u8; FIXED_HEADER_BYTES];
        fixed[..8].copy_from_slice(MAGIC);
        fixed[8] = FORMAT_VERSION;
        fixed[9] = KDF_ARGON2ID_V13;
        fixed[10] = CIPHER_AES256GCM_STREAM;
        fixed[12..16].copy_from_slice(&params.memory_kib.to_be_bytes());
        fixed[16..20].copy_from_slice(&params.passes.to_be_bytes());
        fixed[20..24].copy_from_slice(&params.lanes.to_be_bytes());
        let chunk_field = u32::try_from(chunk_bytes).map_err(|_| invalid("chunk size"))?;
        fixed[24..28].copy_from_slice(&chunk_field.to_be_bytes());
        getrandom::fill(&mut fixed[SALT_OFFSET..SALT_OFFSET + SALT_BYTES])
            .and_then(|()| {
                getrandom::fill(&mut fixed[PREFIX_OFFSET..PREFIX_OFFSET + NONCE_PREFIX_BYTES])
            })
            .map_err(|_| invalid("system entropy is unavailable for the backup archive"))?;
        let keys = derive_keys(password, &fixed, params)?;
        let mut header = [0_u8; HEADER_BYTES];
        header[..FIXED_HEADER_BYTES].copy_from_slice(&fixed);
        header[FIXED_HEADER_BYTES..].copy_from_slice(&keys.check);
        let mut prefix = [0_u8; NONCE_PREFIX_BYTES];
        prefix.copy_from_slice(&fixed[PREFIX_OFFSET..PREFIX_OFFSET + NONCE_PREFIX_BYTES]);
        Ok(Self {
            cipher: Aes256Gcm::new((&*keys.encryption).into()),
            header,
            prefix,
            chunk_bytes,
        })
    }
}

pub(super) struct ArchiveWriter<W: Write> {
    inner: W,
    path: PathBuf,
    cipher: Aes256Gcm,
    header: [u8; HEADER_BYTES],
    prefix: [u8; NONCE_PREFIX_BYTES],
    counter: u32,
    chunk_bytes: usize,
    buffer: Zeroizing<Vec<u8>>,
}

impl<W: Write> ArchiveWriter<W> {
    /// Write the header of a new archive. A key seals one archive only: the
    /// nonce prefix and salt are fresh per key.
    pub(super) fn start(
        mut inner: W,
        path: PathBuf,
        key: SealingKey,
    ) -> Result<Self, DataRecoveryError> {
        inner
            .write_all(&key.header)
            .map_err(|source| map_write_error(&path, source))?;
        Ok(Self {
            inner,
            path,
            cipher: key.cipher,
            header: key.header,
            prefix: key.prefix,
            counter: 0,
            chunk_bytes: key.chunk_bytes,
            buffer: Zeroizing::new(Vec::with_capacity(key.chunk_bytes)),
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
            || header[9] != KDF_ARGON2ID_V13
            || header[10] != CIPHER_AES256GCM_STREAM
            || header[11] != 0
            || header[28..32] != [0; 4]
            || header[FIXED_HEADER_BYTES - 1] != 0
        {
            return Err(DataRecoveryError::CorruptArchive(
                "unsupported backup archive version".into(),
            ));
        }
        let field = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().expect("four"));
        let params = KdfParams {
            memory_kib: field(12),
            passes: field(16),
            lanes: field(20),
        };
        let chunk_bytes = usize::try_from(field(24)).map_err(|_| invalid("chunk size"))?;
        if !kdf_params_in_range(params)
            || !(MIN_CHUNK_BYTES..=MAX_CHUNK_BYTES).contains(&chunk_bytes)
        {
            return Err(DataRecoveryError::CorruptArchive(
                "backup archive parameters are out of range".into(),
            ));
        }
        let keys = derive_keys(password, &header[..FIXED_HEADER_BYTES], params)?;
        if !constant_time_eq(&keys.check, &header[FIXED_HEADER_BYTES..]) {
            return Err(DataRecoveryError::WrongPassword);
        }
        let mut prefix = [0_u8; NONCE_PREFIX_BYTES];
        prefix.copy_from_slice(&header[PREFIX_OFFSET..PREFIX_OFFSET + NONCE_PREFIX_BYTES]);
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

    /// Every remaining authenticated plaintext byte (tests inspect what an
    /// archive carries).
    #[cfg(test)]
    pub(super) fn read_to_end_plain(&mut self) -> Result<Vec<u8>, DataRecoveryError> {
        let mut all = Vec::new();
        loop {
            all.extend_from_slice(&self.plain[self.position..]);
            self.position = self.plain.len();
            if !self.fill()? {
                return Ok(all);
            }
        }
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
