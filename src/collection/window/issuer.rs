// SPDX-License-Identifier: AGPL-3.0-only

//! The single grant issuer on G's Mac (design 2.2, rulings R1 and R2).
//!
//! Issuance holds one exclusive lock from the overlap check through the
//! reservation, signing and publication. A reservation is appended and synced
//! before signing and keeps blocking overlaps. It can be voided only after an
//! `unpublished` entry, which issuance appends when it has established that
//! the signed grant never reached the output path and no staged copy of it
//! remains. Once the grant is renamed into place it is published, whatever
//! happens next (a failed directory sync, a crash before `issued` is
//! appended): such a reservation is never voidable and blocks its interval
//! until that interval has passed. After every append the ledger head
//! is pinned in the login keychain (through `AnchorStore`); a ledger whose
//! head neither matches nor extends the pin refuses issuance, which detects a
//! copied older ledger. The private key stays in 1Password and is read from a
//! 0600 file path at issuance; the key is never stored here.

use std::{
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};

use super::{
    GRANT_GAP_SECONDS,
    anchor::AnchorStore,
    claim::{FileLock, LockMode},
    clock::WindowClock,
    grant::{APPROVER, GRANT_FORMAT, GrantBody, SignedGrant, TrustDocument, key_id_of},
    ledger::{self, LedgerError, Line},
    refusal::Refusal,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IssuerEntry {
    Header {
        issuer_host_id: String,
        key_id: String,
        trust_epoch: u64,
        created_at: i64,
    },
    TrustEpoch {
        epoch: u64,
        at: i64,
    },
    Reserved {
        seq: u64,
        host_id: String,
        starts_at: i64,
        expires_at: i64,
        approval_ref: String,
        reason: String,
        body_digest: String,
        at: i64,
    },
    Issued {
        seq: u64,
        at: i64,
    },
    /// The grant for `seq` was never published and no copy of it remains.
    Unpublished {
        seq: u64,
        at: i64,
    },
    Void {
        seq: u64,
        why: String,
        at: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRequest {
    pub host_id: String,
    pub starts_at: i64,
    pub expires_at: i64,
    pub approver: String,
    pub reason: String,
    pub approval_ref: String,
}

pub struct Issuer {
    dir: PathBuf,
    host_id: String,
    anchor: Arc<dyn AnchorStore>,
    clock: Arc<dyn WindowClock>,
    #[cfg(test)]
    fault: std::sync::Mutex<Option<PublishFault>>,
}

/// Failures the tests inject into publication.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublishFault {
    /// The process dies after the rename, before `issued` is appended.
    CrashAfterRename,
    /// The directory sync after the rename fails.
    DirSyncFails,
}

/// What publication established.
enum Publication {
    /// The grant is at the output path; `durable` is false when the
    /// directory sync failed.
    Published { durable: bool },
    /// The grant never reached the output path and no staged copy remains.
    NotPublished,
    /// A staged copy may remain: treat it as published.
    Unknown,
}

fn remove_staged(path: &Path) -> bool {
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

struct Opened {
    key: SigningKey,
    key_id: String,
    lines: Vec<Line<IssuerEntry>>,
}

impl Issuer {
    /// `dir` is the canonical ledger directory (`Issuer::canonical_dir`);
    /// `host_id` is the issuing Mac's host id.
    pub fn new(
        dir: &Path,
        host_id: &str,
        anchor: Arc<dyn AnchorStore>,
        clock: Arc<dyn WindowClock>,
    ) -> Self {
        Self {
            dir: dir.to_owned(),
            host_id: host_id.to_owned(),
            anchor,
            clock,
            #[cfg(test)]
            fault: std::sync::Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn inject(&self, fault: PublishFault) {
        *self.fault.lock().expect("fault lock") = Some(fault);
    }

    #[cfg(test)]
    fn take_fault(&self, fault: PublishFault) -> bool {
        let mut slot = self.fault.lock().expect("fault lock");
        if *slot == Some(fault) {
            *slot = None;
            true
        } else {
            false
        }
    }

    /// Stages, renames and syncs the grant file, and says what is known.
    fn publish(&self, out_path: &Path, bytes: &[u8]) -> Publication {
        let staged = ledger::temp_path(out_path);
        if ledger::write_temp(out_path, bytes).is_err() || fs::rename(&staged, out_path).is_err() {
            // A failed rename leaves the output path untouched.
            return if remove_staged(&staged) {
                Publication::NotPublished
            } else {
                Publication::Unknown
            };
        }
        let synced = ledger::sync_parent(out_path).is_ok();
        #[cfg(test)]
        let synced = synced && !self.take_fault(PublishFault::DirSyncFails);
        Publication::Published { durable: synced }
    }

    /// The one canonical ledger directory (ruling R2); the operator verb takes
    /// no path argument.
    pub fn canonical_dir(home: &Path) -> PathBuf {
        home.join("dev/machine/records/teslatlas/hub-evidence/window-issuer")
    }

    pub fn ledger_path(&self) -> PathBuf {
        self.dir.join("ledger.jsonl")
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join("issuer.lock")
    }

    fn pin_name(key_id: &str) -> String {
        format!("window-issuer-head-{key_id}")
    }

    fn pin(&self, key_id: &str, line: &Line<IssuerEntry>) -> Result<(), Refusal> {
        self.anchor.write(
            &Self::pin_name(key_id),
            &format!("{}:{}", line.n, line.digest),
        )
    }

    /// Creates the key pair and the ledger once. The private key (hex) is
    /// written to `key_out` (0600, must not exist) for G to store in
    /// 1Password; the trust document for `window trust` is returned.
    pub fn init(&self, key_out: &Path) -> Result<TrustDocument, Refusal> {
        ledger::create_private_dir(&self.dir).map_err(|_| Refusal::Io)?;
        let _lock = FileLock::acquire(&self.lock_path(), LockMode::Exclusive, true)?;
        if self.ledger_path().exists() {
            return Err(Refusal::IssuerExists);
        }
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|_| Refusal::Io)?;
        let key = SigningKey::from_bytes(&seed);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(key_out)
            .map_err(|_| Refusal::Io)?;
        {
            use std::io::Write;
            file.write_all(hex::encode(seed).as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.sync_all())
                .map_err(|_| Refusal::Io)?;
        }
        seed.fill(0);
        let key_id = key_id_of(&key.verifying_key());
        let line = ledger::create(
            &self.ledger_path(),
            IssuerEntry::Header {
                issuer_host_id: self.host_id.clone(),
                key_id: key_id.clone(),
                trust_epoch: 1,
                created_at: self.clock.wall(),
            },
        )
        .map_err(|_| Refusal::Io)?;
        self.pin(&key_id, &line)?;
        Ok(TrustDocument::new(&key.verifying_key(), 1))
    }

    fn load_key(path: &Path) -> Result<SigningKey, Refusal> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Refusal::IssuerKeyMissing);
            }
            Err(_) => return Err(Refusal::IssuerKeyUnreadable),
        };
        if !metadata.is_file() || metadata.mode() & 0o177 != 0 {
            return Err(Refusal::IssuerKeyUnreadable);
        }
        let text = fs::read_to_string(path).map_err(|_| Refusal::IssuerKeyUnreadable)?;
        let seed: [u8; 32] = hex::decode(text.trim())
            .map_err(|_| Refusal::IssuerKeyLength)?
            .try_into()
            .map_err(|_| Refusal::IssuerKeyLength)?;
        Ok(SigningKey::from_bytes(&seed))
    }

    /// Key, ledger, key id, host and pin checks shared by every write.
    fn open_ledger(&self, key_path: &Path) -> Result<Opened, Refusal> {
        let key = Self::load_key(key_path)?;
        let key_id = key_id_of(&key.verifying_key());
        let lines =
            ledger::read::<IssuerEntry>(&self.ledger_path()).map_err(|error| match error {
                LedgerError::Missing => Refusal::IssuerLedgerMissing,
                _ => Refusal::IssuerLedgerBroken,
            })?;
        let Some(IssuerEntry::Header {
            issuer_host_id,
            key_id: header_key_id,
            ..
        }) = lines.first().map(|line| &line.entry)
        else {
            return Err(Refusal::IssuerLedgerBroken);
        };
        if *header_key_id != key_id {
            return Err(Refusal::IssuerKeyIdMismatch);
        }
        if *issuer_host_id != self.host_id {
            return Err(Refusal::IssuerWrongHost);
        }
        let pin = self
            .anchor
            .read(&Self::pin_name(&key_id))?
            .ok_or(Refusal::IssuerPinMissing)?;
        let (pinned_n, pinned_digest) = pin
            .split_once(':')
            .and_then(|(n, digest)| Some((n.parse::<u64>().ok()?, digest.to_owned())))
            .ok_or(Refusal::IssuerLedgerRolledBack)?;
        let matches_pin = lines
            .get(pinned_n.saturating_sub(1) as usize)
            .is_some_and(|line| {
                pinned_n >= 1 && line.n == pinned_n && line.digest == pinned_digest
            });
        if !matches_pin {
            return Err(Refusal::IssuerLedgerRolledBack);
        }
        Ok(Opened { key, key_id, lines })
    }

    fn current_epoch(lines: &[Line<IssuerEntry>]) -> u64 {
        lines
            .iter()
            .filter_map(|line| match &line.entry {
                IssuerEntry::Header { trust_epoch, .. } => Some(*trust_epoch),
                IssuerEntry::TrustEpoch { epoch, .. } => Some(*epoch),
                _ => None,
            })
            .max()
            .unwrap_or(1)
    }

    /// The trust document for the current epoch (for `window trust`).
    pub fn trust_document(&self, key_path: &Path) -> Result<TrustDocument, Refusal> {
        let opened = self.open_ledger(key_path)?;
        Ok(TrustDocument::new(
            &opened.key.verifying_key(),
            Self::current_epoch(&opened.lines),
        ))
    }

    /// Raises the trust epoch. Hubs that trust the new document refuse every
    /// grant of an older epoch.
    pub fn bump_trust_epoch(&self, key_path: &Path) -> Result<TrustDocument, Refusal> {
        let _lock = FileLock::acquire(&self.lock_path(), LockMode::Exclusive, true)?;
        let opened = self.open_ledger(key_path)?;
        let epoch = Self::current_epoch(&opened.lines) + 1;
        let line = ledger::append(
            &self.ledger_path(),
            IssuerEntry::TrustEpoch {
                epoch,
                at: self.clock.wall(),
            },
        )
        .map_err(|_| Refusal::Io)?;
        self.pin(&opened.key_id, &line)?;
        Ok(TrustDocument::new(&opened.key.verifying_key(), epoch))
    }

    /// Issues one grant, or refuses with a code and writes no grant.
    pub fn issue(
        &self,
        key_path: &Path,
        request: &IssueRequest,
        out_path: &Path,
    ) -> Result<SignedGrant, Refusal> {
        let _lock = FileLock::acquire(&self.lock_path(), LockMode::Exclusive, true)?;
        let opened = self.open_ledger(key_path)?;
        if request.approver != APPROVER {
            return Err(Refusal::ApproverNotG);
        }
        if request.approval_ref.trim().is_empty() {
            return Err(Refusal::ApprovalRefMissing);
        }
        let now = self.clock.wall();
        let seq = opened
            .lines
            .iter()
            .filter_map(|line| match &line.entry {
                IssuerEntry::Reserved { seq, .. } => Some(*seq),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            + 1;
        let body = GrantBody {
            format: GRANT_FORMAT.to_owned(),
            issuer_key_id: opened.key_id.clone(),
            seq,
            host_id: request.host_id.clone(),
            starts_at: request.starts_at,
            expires_at: request.expires_at,
            approver: request.approver.clone(),
            reason: request.reason.clone(),
            trust_epoch: Self::current_epoch(&opened.lines),
        };
        body.check_shape()?;
        if request.expires_at <= now {
            return Err(Refusal::GrantExpired);
        }
        let voided: Vec<u64> = opened
            .lines
            .iter()
            .filter_map(|line| match &line.entry {
                IssuerEntry::Void { seq, .. } => Some(*seq),
                _ => None,
            })
            .collect();
        for line in &opened.lines {
            if let IssuerEntry::Reserved {
                seq,
                starts_at,
                expires_at,
                ..
            } = &line.entry
                && !voided.contains(seq)
                && request.starts_at < expires_at + GRANT_GAP_SECONDS
                && *starts_at < request.expires_at + GRANT_GAP_SECONDS
            {
                return Err(Refusal::Overlap);
            }
        }
        let digest = body.digest()?;
        let reserved = ledger::append(
            &self.ledger_path(),
            IssuerEntry::Reserved {
                seq,
                host_id: request.host_id.clone(),
                starts_at: request.starts_at,
                expires_at: request.expires_at,
                approval_ref: request.approval_ref.clone(),
                reason: request.reason.clone(),
                body_digest: digest,
                at: now,
            },
        )
        .map_err(|_| Refusal::Io)?;
        self.pin(&opened.key_id, &reserved)?;
        let signed = SignedGrant::sign(body, &opened.key)
            .and_then(|grant| grant.to_bytes().map(|bytes| (grant, bytes)));
        let (grant, publication) = match signed {
            Ok((grant, bytes)) => {
                let publication = self.publish(out_path, &bytes);
                (Some(grant), publication)
            }
            Err(_) => (None, Publication::NotPublished),
        };
        match publication {
            Publication::NotPublished => {
                // Only now may the reservation be voided. If this append
                // fails it stays non-voidable, which is safe.
                if let Ok(line) = ledger::append(
                    &self.ledger_path(),
                    IssuerEntry::Unpublished { seq, at: now },
                ) {
                    let _ = self.pin(&opened.key_id, &line);
                }
                Err(Refusal::PublicationFailed)
            }
            Publication::Unknown => Err(Refusal::PublicationFailed),
            Publication::Published { durable } => {
                #[cfg(test)]
                if self.take_fault(PublishFault::CrashAfterRename) {
                    return Err(Refusal::PublicationFailed);
                }
                let issued =
                    ledger::append(&self.ledger_path(), IssuerEntry::Issued { seq, at: now })
                        .map_err(|_| Refusal::PublicationFailed)?;
                self.pin(&opened.key_id, &issued)?;
                match (durable, grant) {
                    (true, Some(grant)) => Ok(grant),
                    _ => Err(Refusal::PublicationFailed),
                }
            }
        }
    }

    /// Voids a reservation whose publication is known to have failed,
    /// releasing its interval. A grant that was or may have been published
    /// (no `unpublished` entry) cannot be voided.
    pub fn void(&self, key_path: &Path, seq: u64, why: &str) -> Result<(), Refusal> {
        let _lock = FileLock::acquire(&self.lock_path(), LockMode::Exclusive, true)?;
        let opened = self.open_ledger(key_path)?;
        let mut reserved = false;
        let mut unpublished = false;
        for line in &opened.lines {
            match &line.entry {
                IssuerEntry::Reserved { seq: s, .. } if *s == seq => reserved = true,
                IssuerEntry::Unpublished { seq: s, .. } if *s == seq => unpublished = true,
                IssuerEntry::Issued { seq: s, .. } | IssuerEntry::Void { seq: s, .. }
                    if *s == seq =>
                {
                    return Err(Refusal::VoidTarget);
                }
                _ => {}
            }
        }
        if !reserved {
            return Err(Refusal::VoidTarget);
        }
        if !unpublished {
            return Err(Refusal::PossiblyPublished);
        }
        let line = ledger::append(
            &self.ledger_path(),
            IssuerEntry::Void {
                seq,
                why: why.chars().take(200).collect(),
                at: self.clock.wall(),
            },
        )
        .map_err(|_| Refusal::Io)?;
        self.pin(&opened.key_id, &line)
    }
}
