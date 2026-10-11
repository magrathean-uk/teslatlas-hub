// SPDX-License-Identifier: AGPL-3.0-only

// What this process has already proved about stored lineage packs.
//
// Serving a lineage manifest means proving that every base and delta pack on
// disk still hashes to the digest the catalogue expects. Packs are immutable
// and content-addressed, so a stable pack never needs hashing twice: this
// module remembers each pack that verified, keyed by the file's identity and
// the digest it verified against. A pack whose identity changes in any way
// the filesystem reports is hashed again, and a pack that fails is never
// remembered.

/// Remembered verdicts, evicted least recently used first. A lineage holds at
/// most 512 packs (the default `ProtocolLimits::max_chunks`), so the bound
/// sits well above one lineage and a cyclic manifest scan keeps hitting.
const LINEAGE_DIGEST_CACHE_CAPACITY: usize = 2048;

/// One stored pack file as the filesystem reports it (device, inode, length,
/// modification time and status-change time) together with the digest the
/// catalogue expects it to have. A rewrite in place moves the status-change
/// time even when the modification time is put back, so an unchanged key is
/// evidence that the bytes that were hashed are still there. The expected
/// digest is part of the key because the file name is only a claim: the same
/// inode reached through a different digest name must be hashed against that
/// name's digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct LineageDigestKey {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    expected: Sha256Digest,
}

impl LineageDigestKey {
    fn new(metadata: &fs::Metadata, expected: Sha256Digest) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            expected,
        }
    }
}

/// A bounded set of verified keys with least-recently-used eviction.
struct VerifiedLineageDigests {
    capacity: usize,
    clock: u64,
    recency: HashMap<LineageDigestKey, u64>,
    by_recency: std::collections::BTreeMap<u64, LineageDigestKey>,
}

impl VerifiedLineageDigests {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            clock: 0,
            recency: HashMap::new(),
            by_recency: std::collections::BTreeMap::new(),
        }
    }

    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.capacity
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.recency.len()
    }

    /// Whether `key` was verified; a hit becomes the most recently used.
    fn contains(&mut self, key: &LineageDigestKey) -> bool {
        let Some(&stamp) = self.recency.get(key) else {
            return false;
        };
        self.by_recency.remove(&stamp);
        let stamp = self.next_stamp();
        self.recency.insert(*key, stamp);
        self.by_recency.insert(stamp, *key);
        true
    }

    /// Remember `key` as the most recently used, evicting the least recently
    /// used keys beyond the capacity.
    fn insert(&mut self, key: LineageDigestKey) {
        if let Some(stamp) = self.recency.remove(&key) {
            self.by_recency.remove(&stamp);
        }
        let stamp = self.next_stamp();
        self.recency.insert(key, stamp);
        self.by_recency.insert(stamp, key);
        while self.recency.len() > self.capacity {
            let Some((_, oldest)) = self.by_recency.pop_first() else {
                break;
            };
            self.recency.remove(&oldest);
        }
    }

    fn next_stamp(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
}

fn verified_lineage_digests() -> &'static std::sync::Mutex<VerifiedLineageDigests> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<VerifiedLineageDigests>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::Mutex::new(VerifiedLineageDigests::new(LINEAGE_DIGEST_CACHE_CAPACITY))
    })
}

/// Whether this process already verified exactly this file against exactly
/// this digest. An unusable lock is a miss, so the pack is hashed.
fn lineage_digest_verified(key: &LineageDigestKey) -> bool {
    verified_lineage_digests()
        .lock()
        .map(|mut cache| cache.contains(key))
        .unwrap_or(false)
}

/// Remember a pack that was just hashed to its expected digest. The lock is
/// never held while hashing, and an unusable lock skips the insert.
fn remember_verified_lineage_digest(key: LineageDigestKey) {
    if let Ok(mut cache) = verified_lineage_digests().lock() {
        cache.insert(key);
    }
}

#[cfg(test)]
thread_local! {
    /// Test seam: runs once, on the thread that hashes a pack, after the bytes
    /// were hashed and before the file's identity is read again, so a test can
    /// rewrite the file at exactly the moment a concurrent writer would.
    static AFTER_LINEAGE_HASH: std::cell::RefCell<Option<Box<dyn FnOnce(&Path)>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn run_after_lineage_hash_seam(path: &Path) {
    if let Some(seam) = AFTER_LINEAGE_HASH.with(|slot| slot.borrow_mut().take()) {
        seam(path);
    }
}

impl HubStore {
    /// Hash one stored pack against its expected digest and remember the
    /// verdict. The file is opened without following links and the descriptor
    /// that is hashed is also the one whose identity keys the verdict, read
    /// before and after hashing, so a file swapped in after the caller looked
    /// at it, or rewritten while it is read, is never remembered under the
    /// identity of another file.
    fn hash_lineage_pack(&self, pack: &TransportPack, path: &Path) -> Result<(), StoreError> {
        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|errno| StoreError::OpenBackupPack(errno.into()))?;
        let mut file = File::from(descriptor);
        let opened = file.metadata().map_err(StoreError::OpenBackupPack)?;
        if !opened.file_type().is_file() || opened.len() != pack.compressed_bytes {
            return Err(StoreError::LineagePackNotReady);
        }
        let key = LineageDigestKey::new(&opened, pack.sha256);
        #[cfg(test)]
        self.record_lineage_pack_hash(pack.sha256);
        let digest = sha256_open_file_hex(&mut file)?;
        #[cfg(test)]
        run_after_lineage_hash_seam(path);
        let closed = file.metadata().map_err(StoreError::OpenBackupPack)?;
        if LineageDigestKey::new(&closed, pack.sha256) != key {
            return Err(StoreError::LineagePackNotReady);
        }
        if digest != pack.sha256.to_string() {
            return Err(StoreError::LineagePackDigestMismatch);
        }
        remember_verified_lineage_digest(key);
        Ok(())
    }

    /// Digests of the lineage packs this store hashed, in order; a pack that
    /// is served from the verified set is not listed again.
    #[cfg(test)]
    pub(crate) fn lineage_pack_hash_log(&self) -> Vec<Sha256Digest> {
        self.lineage_pack_hashes
            .lock()
            .map(|log| log.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn record_lineage_pack_hash(&self, digest: Sha256Digest) {
        if let Ok(mut log) = self.lineage_pack_hashes.lock() {
            log.push(digest);
        }
    }
}
