// SPDX-License-Identifier: AGPL-3.0-only

// Paired-device bearer rotation: the sealed successor that lets a lost
// rotation response be retried (csf_9a86), the absolute lifetime cap, and the
// one consistent read both rotation paths decide from.
//
// A rotation with the current bearer `T1` issues `T2`, moves `T1` into the
// device's single grace row and stores `T2` there sealed under a key only `T1`
// can derive: HKDF-SHA256 (RFC 5869) over the crate's `hmac_sha256`, with the
// presented bearer as input keying material and the device id as `info`, then
// AES-256-GCM with a random 96-bit nonce and the device id as associated data.
// The Hub stores a bearer only as a digest, so a catalogue copy alone can
// neither authenticate nor recover `T2`; the holder of `T1` retries and gets
// the same `T2` back, and nobody else can mint authority out of the grace row.

/// HKDF salt of the grace key. A change would strand every sealed successor.
const GRACE_SUCCESSOR_SALT: &[u8] = b"teslatlas-hub grace successor v1";
const SEALED_NONCE_BYTES: usize = 12;
const SEALED_TAG_BYTES: usize = 16;

/// Whether `device_name` is one a claim accepts: 1 to 128 UTF-8 bytes and no
/// control character (U+0000..U+001F, U+007F..U+009F). The claim route checks
/// it before it consults the invitation, with this same function the store
/// applies as its first statement.
pub fn validate_paired_device_name(device_name: &str) -> Result<(), StoreError> {
    validate_identity("paired device name", device_name, MAX_DEVICE_NAME_BYTES)
}

/// HKDF-SHA256 extract (RFC 5869 section 2.2): `PRK = HMAC(salt, IKM)`.
fn hkdf_sha256_extract(salt: &[u8], input_key_material: &[u8]) -> Zeroizing<[u8; 32]> {
    Zeroizing::new(crate::protocol::hmac_sha256(salt, input_key_material))
}

/// HKDF-SHA256 expand (RFC 5869 section 2.3) for an output of at most one hash
/// block: `T(1) = HMAC(PRK, info || 0x01)`, the first 32 bytes of any longer
/// output for the same inputs.
fn hkdf_sha256_expand_first_block(
    pseudo_random_key: &[u8; 32],
    info: &[u8],
) -> Zeroizing<[u8; 32]> {
    let mut message = Zeroizing::new(Vec::with_capacity(info.len() + 1));
    message.extend_from_slice(info);
    message.push(1);
    Zeroizing::new(crate::protocol::hmac_sha256(pseudo_random_key, &message))
}

/// The 256-bit key under which one rotation seals its successor. `bearer` is
/// the presented bearer exactly as transmitted (64 hex characters).
fn grace_successor_key(bearer: &str, device_id: Uuid) -> Zeroizing<[u8; 32]> {
    let pseudo_random_key = hkdf_sha256_extract(GRACE_SUCCESSOR_SALT, bearer.as_bytes());
    hkdf_sha256_expand_first_block(&pseudo_random_key, device_id.to_string().as_bytes())
}

/// `nonce(12) || AES-256-GCM(key, successor, aad = device id)`, the second part
/// being ciphertext followed by the 16-byte tag. The nonce is fresh for every
/// seal. The associated data is the device id's hyphenated lowercase text, the
/// same bytes the key derivation takes as `info`.
fn seal_successor(key: &[u8; 32], device_id: Uuid, successor: &str) -> Result<Vec<u8>, StoreError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| StoreError::SealRotationSuccessor)?;
    let mut nonce = [0_u8; SEALED_NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| StoreError::EntropyUnavailable)?;
    let associated_data = device_id.to_string();
    let ciphertext = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: successor.as_bytes(),
                aad: associated_data.as_bytes(),
            },
        )
        .map_err(|_| StoreError::SealRotationSuccessor)?;
    let mut sealed = Vec::with_capacity(SEALED_NONCE_BYTES + ciphertext.len());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

/// Open a blob [`seal_successor`] wrote. Any malformed or unauthentic blob
/// (short, tampered, wrong key or wrong device) is `None`; there is no partial
/// answer.
fn unseal_successor(key: &[u8; 32], device_id: Uuid, sealed: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if sealed.len() < SEALED_NONCE_BYTES + SEALED_TAG_BYTES {
        return None;
    }
    let (nonce, ciphertext) = sealed.split_at(SEALED_NONCE_BYTES);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let associated_data = device_id.to_string();
    cipher
        .decrypt(
            &Nonce::try_from(nonce).ok()?,
            Payload {
                msg: ciphertext,
                aad: associated_data.as_bytes(),
            },
        )
        .ok()
        .map(Zeroizing::new)
}

/// What one rotation request is answered with, decided from a single read.
enum RotationOutcome {
    /// Answered without touching the catalogue: a retry through the grace
    /// window (the sealed successor) or a rotation at the absolute maximum
    /// (the presented bearer).
    Answered(PairedDeviceAccess),
    /// The presented bearer is the device's current one and its lifetime can
    /// still move: issue a successor.
    Issue(RotationTarget),
}

struct RotationTarget {
    device_id: Uuid,
    current_digest: [u8; ACCESS_TOKEN_BYTES],
    stored_expires_at_ms: i64,
    max_expires_at_ms: Option<i64>,
}

impl RotationTarget {
    /// The expiry a successor issued at `now_ms` carries: the sliding lifetime
    /// from now, never earlier than the stored expiry and never beyond the
    /// absolute maximum.
    fn successor_expires_at_ms(&self, now_ms: i64) -> i64 {
        cap_expiry(
            self.stored_expires_at_ms
                .max(now_ms.saturating_add(PAIRED_DEVICE_TOKEN_LIFETIME_MS)),
            self.max_expires_at_ms,
        )
    }
}

/// An expiry limited by a device's absolute maximum. A NULL maximum means no
/// cap: v68 backfilled every existing device and every claim writes one, so
/// the column is only ever NULL in a catalogue edited by hand, and refusing
/// those would lock their owner out.
fn cap_expiry(expires_at_ms: i64, max_expires_at_ms: Option<i64>) -> i64 {
    max_expires_at_ms.map_or(expires_at_ms, |maximum| expires_at_ms.min(maximum))
}

/// The device whose current bearer has digest `token_digest` at `now_ms`, with
/// its absolute maximum: not revoked, not expired and not at its maximum.
/// `connection`'s current snapshot decides, so a second call after a write by
/// another request sees that write.
fn read_current_bearer_device(
    connection: &Connection,
    token_digest: &[u8; ACCESS_TOKEN_BYTES],
    now_ms: i64,
) -> Result<Option<(PairedDeviceRecord, Option<i64>)>, StoreError> {
    connection
        .query_row(
            "SELECT device_id, display_name, created_at_ms, expires_at_ms, revoked_at_ms,
                    last_authenticated_at_ms, max_expires_at_ms
             FROM paired_devices
              WHERE token_sha256 = ?1 AND revoked_at_ms IS NULL AND expires_at_ms > ?2
                AND (max_expires_at_ms IS NULL OR max_expires_at_ms > ?2)",
            params![token_digest.as_slice(), now_ms],
            |row| Ok((paired_device_from_row(row)?, row.get::<_, Option<i64>>(6)?)),
        )
        .optional()
        .map_err(StoreError::Query)
}

/// Decide a rotation for the bearer presented as `access_token` (whose digest
/// is `token_digest`) at `now_ms` from `connection`'s current snapshot. A
/// bearer is rotatable while it is the device's current one, or the one the
/// latest rotation replaced and still inside its 24 hour grace window, and
/// only for a device that is not revoked, not expired and not at or past its
/// absolute maximum. Everything else is [`StoreError::PairingRejected`].
fn rotation_outcome(
    connection: &Connection,
    access_token: &str,
    token_digest: &[u8; ACCESS_TOKEN_BYTES],
    now_ms: i64,
) -> Result<RotationOutcome, StoreError> {
    let row: Option<(Uuid, Vec<u8>, i64, Option<i64>, Option<Vec<u8>>)> = connection
        .query_row(
            "SELECT p.device_id, p.token_sha256, p.expires_at_ms, p.max_expires_at_ms,
                    g.sealed_successor
               FROM paired_devices AS p
               LEFT JOIN paired_device_token_grace AS g ON g.device_id = p.device_id
              WHERE (p.token_sha256 = ?1 OR
                     (g.token_sha256 = ?1 AND g.valid_until_ms > ?2))
                AND p.created_at_ms <= ?2 AND p.revoked_at_ms IS NULL
                AND p.expires_at_ms > ?2
                AND (p.max_expires_at_ms IS NULL OR p.max_expires_at_ms > ?2)",
            params![token_digest.as_slice(), now_ms],
            |row| {
                let id: String = row.get(0)?;
                let id = Uuid::parse_str(&id).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok((id, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            },
        )
        .optional()
        .map_err(StoreError::RotateDevice)?;
    let Some((device_id, current_digest, stored_expires_at_ms, max_expires_at_ms, sealed)) = row
    else {
        return Err(StoreError::PairingRejected);
    };
    let current_digest: [u8; ACCESS_TOKEN_BYTES] = current_digest
        .try_into()
        .map_err(|_| StoreError::PairingRejected)?;

    if constant_time_equal(&current_digest, token_digest) {
        if max_expires_at_ms.is_some_and(|maximum| stored_expires_at_ms >= maximum) {
            // No successor could live longer than this bearer: hand the
            // presented one back, issue and write nothing.
            let access_token =
                DeviceAccessToken::from_wire(access_token).ok_or(StoreError::PairingRejected)?;
            return Ok(RotationOutcome::Answered(PairedDeviceAccess {
                device_id,
                access_token,
                expires_at_ms: cap_expiry(stored_expires_at_ms, max_expires_at_ms),
                at_maximum_lifetime: true,
            }));
        }
        return Ok(RotationOutcome::Issue(RotationTarget {
            device_id,
            current_digest,
            stored_expires_at_ms,
            max_expires_at_ms,
        }));
    }

    // The presented bearer is the grace one: answer with the successor it
    // sealed, if and only if that successor is still the device's current
    // bearer. A pre-v68 grace row has nothing sealed, and a blob that does not
    // open under this bearer and this device is no answer either.
    let sealed = sealed.ok_or(StoreError::PairingRejected)?;
    let key = grace_successor_key(access_token, device_id);
    let plaintext =
        unseal_successor(&key, device_id, &sealed).ok_or(StoreError::PairingRejected)?;
    let successor = std::str::from_utf8(&plaintext)
        .ok()
        .and_then(DeviceAccessToken::from_wire)
        .ok_or(StoreError::PairingRejected)?;
    if !constant_time_equal(&successor.digest(), &current_digest) {
        return Err(StoreError::PairingRejected);
    }
    Ok(RotationOutcome::Answered(PairedDeviceAccess {
        device_id,
        access_token: successor,
        expires_at_ms: cap_expiry(stored_expires_at_ms, max_expires_at_ms),
        at_maximum_lifetime: false,
    }))
}

// Test seam. The two credential writes more than one request can race for (the
// coalesced authentication write and a rotation's issue) decide from a
// read-only snapshot and then ask for the writer. A test installs a hook that
// runs between the two, so a rendezvous or a competing write there is exact
// instead of a matter of thread scheduling.

/// Which credential write a hook runs in front of.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CredentialWriteStage {
    Authenticate,
    Rotate,
}

#[cfg(test)]
#[derive(Clone)]
struct CredentialWriteHook(Arc<dyn Fn(CredentialWriteStage) + Send + Sync>);

#[cfg(test)]
impl CredentialWriteHook {
    fn new(hook: impl Fn(CredentialWriteStage) + Send + Sync + 'static) -> Self {
        Self(Arc::new(hook))
    }
}

#[cfg(test)]
impl std::fmt::Debug for CredentialWriteHook {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialWriteHook")
    }
}

#[cfg(test)]
impl HubStore {
    fn set_credential_write_hook(&self, hook: Option<CredentialWriteHook>) {
        *self
            .credential_write_hook
            .lock()
            .expect("credential write hook lock") = hook;
    }

    /// Run the installed hook, if any, without holding its lock.
    fn before_credential_write(&self, stage: CredentialWriteStage) {
        let hook = self
            .credential_write_hook
            .lock()
            .expect("credential write hook lock")
            .clone();
        if let Some(hook) = hook {
            (hook.0)(stage);
        }
    }
}
