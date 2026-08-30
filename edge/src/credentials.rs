use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

const STORE_VERSION: u16 = 1;
const MAX_TTL_MS: i64 = 366 * 24 * 60 * 60 * 1_000;
const MAX_ROTATION_OVERLAP_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Clone)]
pub struct CredentialStore {
    path: Arc<PathBuf>,
    mutation_lock: Arc<Mutex<()>>,
}

impl CredentialStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CredentialError> {
        let path = path.as_ref().to_path_buf();
        if !path.is_absolute() {
            return Err(CredentialError::InvalidPath);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| CredentialError::Io)?;
        }
        let store = Self {
            path: Arc::new(path),
            mutation_lock: Arc::new(Mutex::new(())),
        };
        if store.path.exists() {
            store.load()?;
        } else {
            store.save(&CredentialFile {
                version: STORE_VERSION,
                credentials: Vec::new(),
            })?;
        }
        Ok(store)
    }

    pub fn enrol(
        &self,
        label: &str,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<IssuedCredential, CredentialError> {
        validate_label(label)?;
        validate_ttl(ttl_ms)?;
        let _guard = self.mutation_lock.lock().map_err(|_| CredentialError::Io)?;
        let mut file = self.load()?;
        let issued = issue(label, None, now_ms, ttl_ms);
        file.credentials.push(issued.stored.clone());
        self.save(&file)?;
        Ok(issued.public)
    }

    pub fn rotate(
        &self,
        credential_id: &str,
        now_ms: i64,
        overlap_ms: i64,
        ttl_ms: i64,
    ) -> Result<IssuedCredential, CredentialError> {
        validate_ttl(ttl_ms)?;
        if !(0..=MAX_ROTATION_OVERLAP_MS).contains(&overlap_ms) {
            return Err(CredentialError::InvalidOverlap);
        }
        let credential_id =
            Uuid::parse_str(credential_id).map_err(|_| CredentialError::UnknownCredential)?;
        let _guard = self.mutation_lock.lock().map_err(|_| CredentialError::Io)?;
        let mut file = self.load()?;
        let old = file
            .credentials
            .iter_mut()
            .find(|credential| credential.id == credential_id)
            .ok_or(CredentialError::UnknownCredential)?;
        if !old.active_at(now_ms) {
            return Err(CredentialError::InactiveCredential);
        }
        old.valid_until_ms = Some(now_ms.saturating_add(overlap_ms).min(old.expires_at_ms));
        let issued = issue(&old.label, Some(old.id), now_ms, ttl_ms);
        file.credentials.push(issued.stored.clone());
        self.save(&file)?;
        Ok(issued.public)
    }

    pub fn revoke(&self, credential_id: &str, now_ms: i64) -> Result<(), CredentialError> {
        let credential_id =
            Uuid::parse_str(credential_id).map_err(|_| CredentialError::UnknownCredential)?;
        let _guard = self.mutation_lock.lock().map_err(|_| CredentialError::Io)?;
        let mut file = self.load()?;
        let credential = file
            .credentials
            .iter_mut()
            .find(|credential| credential.id == credential_id)
            .ok_or(CredentialError::UnknownCredential)?;
        credential.revoked_at_ms = Some(
            credential
                .revoked_at_ms
                .map_or(now_ms, |existing| existing.min(now_ms)),
        );
        self.save(&file)
    }

    pub fn verify(&self, token: &str, now_ms: i64) -> Result<bool, CredentialError> {
        let Some(parsed) = ParsedToken::parse(token) else {
            return Ok(false);
        };
        let file = self.load()?;
        let Some(credential) = file
            .credentials
            .iter()
            .find(|credential| credential.id == parsed.id)
        else {
            return Ok(false);
        };
        if !credential.active_at(now_ms) {
            return Ok(false);
        }
        let expected = digest_secret(credential.id, &parsed.secret);
        let valid = expected.len() == credential.secret_digest.len()
            && bool::from(
                expected
                    .as_bytes()
                    .ct_eq(credential.secret_digest.as_bytes()),
            );
        Ok(valid)
    }

    pub fn list(&self) -> Result<Vec<CredentialSummary>, CredentialError> {
        Ok(self
            .load()?
            .credentials
            .into_iter()
            .map(|credential| CredentialSummary {
                credential_id: credential.id.to_string(),
                label: credential.label,
                created_at_ms: credential.created_at_ms,
                expires_at_ms: credential.expires_at_ms,
                valid_until_ms: credential.valid_until_ms,
                revoked_at_ms: credential.revoked_at_ms,
            })
            .collect())
    }

    fn load(&self) -> Result<CredentialFile, CredentialError> {
        let bytes = fs::read(self.path.as_ref()).map_err(|_| CredentialError::Io)?;
        if bytes.len() > 1_048_576 {
            return Err(CredentialError::InvalidStore);
        }
        let file: CredentialFile =
            serde_json::from_slice(&bytes).map_err(|_| CredentialError::InvalidStore)?;
        if file.version != STORE_VERSION {
            return Err(CredentialError::InvalidStore);
        }
        let mut ids = std::collections::HashSet::new();
        if file.credentials.iter().any(|credential| {
            !ids.insert(credential.id)
                || validate_label(&credential.label).is_err()
                || credential.created_at_ms >= credential.expires_at_ms
                || credential.secret_digest.len() != 64
                || !credential
                    .secret_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(CredentialError::InvalidStore);
        }
        Ok(file)
    }

    fn save(&self, file: &CredentialFile) -> Result<(), CredentialError> {
        let bytes = serde_json::to_vec_pretty(file).map_err(|_| CredentialError::InvalidStore)?;
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(CredentialError::InvalidPath)?;
        let temporary = self
            .path
            .with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut output = options.open(&temporary).map_err(|_| CredentialError::Io)?;
        output.write_all(&bytes).map_err(|_| CredentialError::Io)?;
        output.sync_all().map_err(|_| CredentialError::Io)?;
        drop(output);
        fs::rename(&temporary, self.path.as_ref()).map_err(|_| CredentialError::Io)?;
        #[cfg(unix)]
        fs::set_permissions(self.path.as_ref(), fs::Permissions::from_mode(0o600))
            .map_err(|_| CredentialError::Io)?;
        if let Some(parent) = self.path.parent() {
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| CredentialError::Io)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct IssuedCredential {
    credential_id: String,
    token: String,
    secret_component: String,
}

impl IssuedCredential {
    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn secret_component(&self) -> &str {
        &self.secret_component
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSummary {
    pub credential_id: String,
    pub label: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub valid_until_ms: Option<i64>,
    pub revoked_at_ms: Option<i64>,
}

impl CredentialSummary {
    pub fn token(&self) -> Option<&str> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CredentialError {
    #[error("credential path must be absolute")]
    InvalidPath,
    #[error("credential label is invalid")]
    InvalidLabel,
    #[error("credential lifetime is invalid")]
    InvalidLifetime,
    #[error("credential rotation overlap is invalid")]
    InvalidOverlap,
    #[error("credential does not exist")]
    UnknownCredential,
    #[error("credential is inactive")]
    InactiveCredential,
    #[error("credential store is invalid")]
    InvalidStore,
    #[error("credential input or output failed")]
    Io,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    version: u16,
    credentials: Vec<StoredCredential>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredential {
    id: Uuid,
    label: String,
    secret_digest: String,
    created_at_ms: i64,
    expires_at_ms: i64,
    valid_until_ms: Option<i64>,
    revoked_at_ms: Option<i64>,
    rotation_of: Option<Uuid>,
}

impl StoredCredential {
    fn active_at(&self, now_ms: i64) -> bool {
        now_ms >= self.created_at_ms
            && now_ms < self.expires_at_ms
            && self.valid_until_ms.is_none_or(|until| now_ms < until)
            && self.revoked_at_ms.is_none_or(|revoked| now_ms < revoked)
    }
}

struct IssuedPair {
    stored: StoredCredential,
    public: IssuedCredential,
}

fn issue(label: &str, rotation_of: Option<Uuid>, now_ms: i64, ttl_ms: i64) -> IssuedPair {
    let id = Uuid::new_v4();
    let mut secret = [0_u8; 32];
    rand::rng().fill_bytes(&mut secret);
    let secret_component = URL_SAFE_NO_PAD.encode(secret);
    let token = format!("tte1.{id}.{secret_component}");
    let stored = StoredCredential {
        id,
        label: label.to_owned(),
        secret_digest: digest_secret(id, &secret),
        created_at_ms: now_ms,
        expires_at_ms: now_ms.saturating_add(ttl_ms),
        valid_until_ms: None,
        revoked_at_ms: None,
        rotation_of,
    };
    secret.zeroize();
    IssuedPair {
        stored,
        public: IssuedCredential {
            credential_id: id.to_string(),
            token,
            secret_component,
        },
    }
}

struct ParsedToken {
    id: Uuid,
    secret: [u8; 32],
}

impl ParsedToken {
    fn parse(token: &str) -> Option<Self> {
        let mut parts = token.split('.');
        if parts.next()? != "tte1" {
            return None;
        }
        let id = Uuid::parse_str(parts.next()?).ok()?;
        let decoded = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
        if parts.next().is_some() || decoded.len() != 32 {
            return None;
        }
        let mut secret = [0_u8; 32];
        secret.copy_from_slice(&decoded);
        Some(Self { id, secret })
    }
}

impl Drop for ParsedToken {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

fn digest_secret(id: Uuid, secret: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"teslatlas-edge-hub-credential-v1\0");
    digest.update(id.as_bytes());
    digest.update([0]);
    digest.update(secret);
    hex::encode(digest.finalize())
}

fn validate_label(label: &str) -> Result<(), CredentialError> {
    if (1..=64).contains(&label.len()) && !label.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(CredentialError::InvalidLabel)
    }
}

fn validate_ttl(ttl_ms: i64) -> Result<(), CredentialError> {
    if (1_000..=MAX_TTL_MS).contains(&ttl_ms) {
        Ok(())
    } else {
        Err(CredentialError::InvalidLifetime)
    }
}
