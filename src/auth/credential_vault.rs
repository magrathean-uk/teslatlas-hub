// SPDX-License-Identifier: AGPL-3.0-only

//! Credential custody. The connection and commit counters have one lock.

use std::{path::{Path, PathBuf}, sync::{Arc, Mutex, MutexGuard, RwLock}, time::{SystemTime, UNIX_EPOCH}};
use uuid::Uuid;
use zeroize::Zeroizing;
use super::{credential_envelope::{CredentialSlot, CredentialOrigin, RefreshOwner, EnvelopeError, canonical_token, refresh_fingerprint, parse_header, ENVELOPE_HEADER_BYTES, OsNonces}, credential_keys::{self, BackupExclusion, CredentialKeyError, ExclusionState, KeyringHandle, SystemBackupExclusion}, credential_migration::{self, CredentialPhaseB, CredentialMigrationError}};
use crate::{db::{HubStore, CredentialConnection, CredentialPairPresence, SealedCredentialPair, StoreError, EntryProvenance, ReplacePolicy, RefreshCompletion, LegacyCandidates, OutboundRequestReceiptId, OutboundRequestOutcome, CredentialSecretsExclusion, CredentialSlotFormatState}, hub_user_process::AdmittedUserHub, platform::private_directory::{self, PrivateDirectory, EntryState, durable_sync_fd}};

pub(crate) mod types;
pub mod entry;
pub(crate) use types::*;
#[cfg(any(test, debug_assertions))]
pub(crate) mod fixtures;
#[cfg(test)]
thread_local! {
    static PROBE_CONTROL: std::cell::RefCell<Option<Box<dyn FnMut(&mut CredentialConnection)>>> = const { std::cell::RefCell::new(None) };
    static SUCCESSOR_GENERATION_FIXTURE: std::cell::Cell<Option<Uuid>> = const { std::cell::Cell::new(None) };
    static OPEN_CONTROL: std::cell::RefCell<Option<Box<dyn FnMut(&mut CredentialConnection)>>> = const { std::cell::RefCell::new(None) };
    static ROTATION_NONCES: std::cell::RefCell<Option<super::credential_envelope::FixedNonces>> = const { std::cell::RefCell::new(None) };
    static ROTATION_TRACE: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
    static RESEAL_CONTROL: std::cell::RefCell<Option<Box<dyn FnOnce(&mut CredentialConnection)>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
fn rotation_note(event: &'static str) { ROTATION_TRACE.with(|events| events.borrow_mut().push(event)); }
#[cfg(test)]
fn rotation_key_events() {
    for event in credential_keys::take_key_events() {
        if matches!(event, "file_barrier" | "directory_barrier") { rotation_note(event); }
    }
}
#[cfg(test)]
fn with_successor_generation_fixture<T>(generation: Uuid, action: impl FnOnce() -> T) -> T {
    struct Reset(Option<Uuid>);
    impl Drop for Reset { fn drop(&mut self) { SUCCESSOR_GENERATION_FIXTURE.with(|slot| slot.set(self.0)); } }
    let reset = Reset(SUCCESSOR_GENERATION_FIXTURE.with(|slot| slot.replace(Some(generation))));
    let result = action(); drop(reset); result
}

pub(crate) enum CredentialStoreError {
    Key(CredentialKeyError), Envelope { slot: CredentialSlot, error: EnvelopeError },
    FingerprintMismatch(CredentialSlot), SlotOccupied, RefreshTokenSpent,
    FreshSignInRequired, EmptyAccessToken, EmptyRefreshToken, TokenTooLarge,
    InvalidOrigin, SuccessorNotCanonical, MigrationPending(CredentialSlot),
    MigrationBlocked(CredentialSlot), DurabilityUnavailable, BackupExclusionFailed,
    SensitiveAccessUnavailable, LockPoisoned, InvalidTokenBytes, SignIn(SignInError),
    Store(StoreError),
}
impl std::fmt::Debug for CredentialStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let slot = match self {
            Self::Envelope { slot, .. } | Self::FingerprintMismatch(slot)
                | Self::MigrationPending(slot) | Self::MigrationBlocked(slot) => Some(slot),
            _ => None,
        };
        f.debug_struct("CredentialStoreError").field("code", &self.code()).field("slot", &slot).finish()
    }
}
impl CredentialStoreError {
    pub(in crate::auth) fn code(&self) -> &'static str {
        fn envelope(error: &EnvelopeError) -> &'static str {
            match error {
                EnvelopeError::UnknownKey => "unknown_key", EnvelopeError::BindingMismatch(_) => "binding_mismatch",
                EnvelopeError::UnknownVersion(_) | EnvelopeError::UnknownAlgorithm(_) => "unknown_format",
                EnvelopeError::OverCap => "over_cap", EnvelopeError::TokenTooLarge => "token_too_large",
                EnvelopeError::EntropyUnavailable => "credential_store_unavailable",
                EnvelopeError::Truncated | EnvelopeError::BadMagic | EnvelopeError::Corrupt => "corrupt",
            }
        }
        match self {
            Self::Key(error) => match error {
                CredentialKeyError::KeyringMissing => "keyring_missing", CredentialKeyError::KeyringUnreadable => "keyring_unreadable",
                CredentialKeyError::KeyringUnsafe(_) => "keyring_unsafe", CredentialKeyError::KeyringInvalid => "keyring_invalid",
                CredentialKeyError::KeyringUnsupportedVersion(_) => "keyring_newer", CredentialKeyError::DurabilityUnavailable => "durability_unavailable",
                CredentialKeyError::BackupExclusionFailed => "secrets_backup_exclusion_failed", _ => "credential_store_unavailable",
            },
            Self::Envelope { error, .. } => envelope(error), Self::FingerprintMismatch(_) | Self::InvalidTokenBytes => "corrupt",
            Self::SlotOccupied => "slot_occupied", Self::RefreshTokenSpent => "refresh_token_spent", Self::FreshSignInRequired => "fresh_sign_in_required",
            Self::EmptyAccessToken => "empty_access_token", Self::EmptyRefreshToken => "empty_refresh_token", Self::TokenTooLarge => "token_too_large",
            Self::InvalidOrigin => "invalid_origin", Self::SuccessorNotCanonical => "successor_not_canonical",
            Self::MigrationPending(_) => "migration_pending", Self::MigrationBlocked(_) => "migration_blocked",
            Self::DurabilityUnavailable => "durability_unavailable", Self::BackupExclusionFailed => "secrets_backup_exclusion_failed",
            Self::SensitiveAccessUnavailable => "sensitive_access_unavailable", Self::LockPoisoned => "credential_lock_poisoned",
            Self::SignIn(error) => error.code(), Self::Store(StoreError::CredentialEnvelope(error)) => envelope(error),
            Self::Store(_) => "credential_store_unavailable",
        }
    }
}
impl From<StoreError> for CredentialStoreError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::CatalogueDurability(_) => Self::DurabilityUnavailable,
            StoreError::SlotOccupied => Self::SlotOccupied,
            StoreError::RefreshTokenSpent => Self::RefreshTokenSpent,
            StoreError::FreshSignInRequired => Self::FreshSignInRequired,
            StoreError::InvalidOrigin => Self::InvalidOrigin,
            StoreError::SignInBindingInvalid => Self::SignIn(SignInError::BindingInvalid),
            StoreError::SignInExpired => Self::SignIn(SignInError::Expired),
            other => Self::Store(other),
        }
    }
}
impl From<CredentialKeyError> for CredentialStoreError {
    fn from(error: CredentialKeyError) -> Self {
        match error {
            CredentialKeyError::DurabilityUnavailable => Self::DurabilityUnavailable,
            CredentialKeyError::BackupExclusionFailed => Self::BackupExclusionFailed,
            other => Self::Key(other),
        }
    }
}
impl From<CredentialMigrationError> for CredentialStoreError {
    fn from(error: CredentialMigrationError) -> Self {
        match error {
            CredentialMigrationError::Key(e) => e.into(),
            CredentialMigrationError::Store(e) => e.into(),
            CredentialMigrationError::Envelope(error) => Self::Envelope { slot: CredentialSlot::OwnerApi, error },
            CredentialMigrationError::Io { .. } => Self::SensitiveAccessUnavailable,
        }
    }
}

/// Secrets deliberately have neither Clone nor Display.
pub(crate) struct CredentialPlain {
    pub slot: CredentialSlot, pub origin: Option<CredentialOrigin>, pub refresh_owner: RefreshOwner,
    pub generation: Uuid, pub access: Zeroizing<String>, pub refresh: Zeroizing<String>,
    pub refresh_present: bool, pub expires_at: i64, pub next_refresh_at: i64,
    pub client_id: Option<String>, pub region: Option<String>,
}
impl std::fmt::Debug for CredentialPlain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialPlain").field("slot", &self.slot).field("origin", &self.origin)
            .field("refresh_owner", &self.refresh_owner).field("refresh_present", &self.refresh_present)
            .field("expires_at", &self.expires_at).field("next_refresh_at", &self.next_refresh_at)
            .field("access_len", &self.access.len()).field("refresh_len", &self.refresh.len()).finish()
    }
}

#[derive(Default)]
pub(crate) struct VaultOptions {
    #[cfg(any(test, debug_assertions))] pub(crate) exclusion: Option<Arc<dyn BackupExclusion + Send + Sync>>,
    #[cfg(test)] pub(crate) now_ms: Option<i64>,
    #[cfg(test)] pub(crate) probe_failure: bool,
}
struct VaultState {
    connection: CredentialConnection, commits_issued: u64, commits_proven: u64,
    read_only: bool, exclusion_verified: bool, secrets: Option<PrivateDirectory>,
    resume_error: Option<CredentialStoreError>,
    #[cfg(test)] block_completion_barriers: bool,
}
struct VaultGuard<'a> {
    state: Option<MutexGuard<'a, VaultState>>,
    #[cfg(test)] seam: Option<Arc<VaultLockSeam>>,
}
impl std::ops::Deref for VaultGuard<'_> {
    type Target = VaultState;
    fn deref(&self) -> &VaultState { self.state.as_deref().unwrap() }
}
impl std::ops::DerefMut for VaultGuard<'_> {
    fn deref_mut(&mut self) -> &mut VaultState { self.state.as_deref_mut().unwrap() }
}
impl Drop for VaultGuard<'_> {
    fn drop(&mut self) {
        drop(self.state.take());
        #[cfg(test)] if let Some(seam) = &self.seam { seam.record("release"); }
    }
}
#[cfg(test)]
#[derive(Default)]
struct VaultLockSeam {
    events: Mutex<Vec<(std::thread::ThreadId, &'static str)>>,
    changed: std::sync::Condvar,
    rotation_gate: Mutex<bool>,
    gate_changed: std::sync::Condvar,
}
#[cfg(test)]
struct ReadPauseSeam {
    point: &'static str,
    state: Mutex<(bool, bool)>,
    changed: std::sync::Condvar,
}
#[cfg(test)]
impl ReadPauseSeam {
    fn new(point: &'static str) -> Arc<Self> { Arc::new(Self { point, state: Mutex::new((false,false)), changed: std::sync::Condvar::new() }) }
    fn pause(&self, point: &'static str) {
        if self.point != point { return; }
        let mut state = self.state.lock().unwrap(); if state.0 { return; } state.0 = true; self.changed.notify_all();
        let (_guard, timed) = self.changed.wait_timeout_while(state, std::time::Duration::from_secs(10), |state| !state.1).unwrap();
        assert!(!timed.timed_out(), "read pause acknowledgement");
    }
    fn wait(&self) {
        let state = self.state.lock().unwrap();
        let (_guard, timed) = self.changed.wait_timeout_while(state, std::time::Duration::from_secs(10), |state| !state.0).unwrap();
        assert!(!timed.timed_out(), "read reached boundary");
    }
    fn release(&self) { self.state.lock().unwrap().1 = true; self.changed.notify_all(); }
}
#[cfg(test)]
impl VaultLockSeam {
    fn record(&self, event: &'static str) {
        self.events.lock().unwrap().push((std::thread::current().id(), event));
        self.changed.notify_all();
    }
    fn wait_for(&self, thread: Option<std::thread::ThreadId>, event: &'static str) {
        let events = self.events.lock().unwrap();
        let (events, timeout) = self.changed.wait_timeout_while(events, std::time::Duration::from_secs(10), |events| {
            !events.iter().any(|(id, item)| thread.is_none_or(|wanted| wanted == *id) && *item == event)
        }).unwrap();
        assert!(!timeout.timed_out() || events.iter().any(|(id, item)| thread.is_none_or(|wanted| wanted == *id) && *item == event), "lock acknowledgement");
    }
    fn pause_rotation(&self) {
        self.record("rotation_step1");
        let gate = self.rotation_gate.lock().unwrap();
        let (gate, timeout) = self.gate_changed.wait_timeout_while(gate, std::time::Duration::from_secs(10), |gate| !*gate).unwrap();
        assert!(!timeout.timed_out(), "rotation gate");
        drop(gate);
    }
    fn release_rotation(&self) { *self.rotation_gate.lock().unwrap() = true; self.gate_changed.notify_all(); }
}
impl VaultState {
    fn barrier(&mut self) -> Result<(), CredentialStoreError> {
        let started = self.commits_issued;
        #[cfg(debug_assertions)] entry::test_boundary("credential_barrier")?;
        self.connection.credential_durability_barrier()?;
        self.commits_proven = started;
        #[cfg(debug_assertions)] entry::test_boundary("credential_barrier_passed")?;
        Ok(())
    }
    fn guard_read(&mut self) -> Result<(), CredentialStoreError> {
        if self.commits_proven < self.commits_issued { self.barrier()?; }
        Ok(())
    }
    fn writable(&self, exclusion: bool) -> Result<(), CredentialStoreError> {
        if self.read_only { return Err(CredentialStoreError::DurabilityUnavailable); }
        if exclusion && !self.exclusion_verified { return Err(CredentialStoreError::BackupExclusionFailed); }
        Ok(())
    }
}
pub(crate) struct CredentialVault {
    store: HubStore, admission: Arc<AdmittedUserHub>, data_dir: PathBuf,
    lock: Mutex<VaultState>,
    keys: RwLock<Option<KeyringHandle>>,
    #[cfg(any(test, debug_assertions))] options: VaultOptions,
    #[cfg(test)] lock_seam: Mutex<Option<Arc<VaultLockSeam>>>,
    #[cfg(test)] read_pause: Mutex<Option<Arc<ReadPauseSeam>>>,
}
impl CredentialVault {
    /// Open once at the runtime composition boundary, before building managers.
    pub(crate) fn open_for_runtime(
        store: HubStore, admission: Arc<AdmittedUserHub>, data_dir: &Path,
        loss: Option<&crate::credentials::LossObserver>,
    ) -> Result<Arc<Self>, CredentialStoreError> {
        admission.assert_sensitive_access().map_err(|_|CredentialStoreError::SensitiveAccessUnavailable)?;
        admission.assert_store_path(data_dir).map_err(|_|CredentialStoreError::SensitiveAccessUnavailable)?;
        if loss.is_some_and(|lost|lost()) { return Err(CredentialStoreError::SensitiveAccessUnavailable); }
        #[cfg(test)]
        { fixtures::open_runtime_fixture_vault(store, data_dir, admission) }
        #[cfg(not(test))]
        { Self::open(store, admission, data_dir, VaultOptions::default()) }
    }
    #[cfg(test)]
    pub(crate) fn test_admission(&self)->Arc<AdmittedUserHub>{Arc::clone(&self.admission)}
    pub(crate) fn open(store: HubStore, admission: Arc<AdmittedUserHub>, data_dir: &Path, options: VaultOptions) -> Result<Arc<Self>, CredentialStoreError> {
        #[cfg(test)]
        let registry = Arc::clone(&store.fixture_vault);
        #[cfg(test)]
        let mut registered = registry.current.lock().map_err(|_| CredentialStoreError::LockPoisoned)?;
        #[cfg(test)]
        if registered.upgrade().is_some() { return Err(StoreError::CredentialConnectionTaken.into()); }
        #[cfg(test)]
        let mut retained = registry.admission.lock().map_err(|_|CredentialStoreError::LockPoisoned)?;
        #[cfg(test)]
        if retained.as_ref().is_some_and(|retained| !Arc::ptr_eq(retained, &admission)) {
            return Err(CredentialStoreError::SensitiveAccessUnavailable);
        }
        let vault = Self::open_unregistered(store, admission, data_dir, options)?;
        #[cfg(test)]
        {
            *retained = Some(Arc::clone(&vault.admission));
            *registered = Arc::downgrade(&vault);
        }
        Ok(vault)
    }
    fn open_unregistered(store: HubStore, admission: Arc<AdmittedUserHub>, data_dir: &Path, options: VaultOptions) -> Result<Arc<Self>, CredentialStoreError> {
        admission.assert_sensitive_access().map_err(|_| CredentialStoreError::SensitiveAccessUnavailable)?;
        admission.assert_store_path(data_dir).map_err(|_| CredentialStoreError::SensitiveAccessUnavailable)?;
        let admission_lease = admission.take_credential_vault().ok_or(StoreError::CredentialConnectionTaken)?;
        let mut connection = store.take_credential_connection()?;
        connection.bind_vault_admission(admission_lease);
        #[cfg(not(any(test, debug_assertions)))] let VaultOptions {} = options;
        let vault = Arc::new(Self { store, admission, data_dir: data_dir.to_owned(),
            #[cfg(any(test, debug_assertions))] options,
            #[cfg(test)] lock_seam: Mutex::new(None),
            #[cfg(test)] read_pause: Mutex::new(None),
            keys: RwLock::new(None), lock: Mutex::new(VaultState { connection, commits_issued: 0,
                commits_proven: 0, read_only: false, exclusion_verified: false, secrets: None, resume_error: None,
                #[cfg(test)] block_completion_barriers: false }) });
        {
            let mut state = vault.lock()?;
            #[cfg(test)]
            if vault.options.probe_failure { state.connection.set_test_barrier_failure(true); }
            #[cfg(test)] PROBE_CONTROL.with(|control|{if let Some(control)=control.borrow_mut().as_mut(){control(&mut state.connection);}});
            if let Err(error)=state.connection.probe_credential_durability(){state.read_only=true;state.resume_error=Some(error.into());}
            #[cfg(debug_assertions)]
            if entry::test_probe_failure() {
                state.read_only = true; state.resume_error = Some(CredentialStoreError::DurabilityUnavailable);
                vault.store.open()?.execute(
                    "UPDATE credential_format_state SET durability='unavailable', durability_probed_at_ms=?1 WHERE singleton_id=1",
                    [vault.now_ms()?],
                ).map_err(StoreError::CredentialPairStore)?;
            }
            #[cfg(test)]
            if vault.options.probe_failure { state.connection.set_test_barrier_failure(false); }
            let secrets_path = data_dir.join("secrets");
            state.secrets = if state.read_only { private_directory::open_validated_read_only(&secrets_path) }
                else { private_directory::open_validated(&secrets_path, true).map(Some) }
                .map_err(|error| {
                    #[cfg(test)] fixtures::directory_open_failure(&error);
                    #[cfg(not(test))] let _ = error;
                    CredentialStoreError::Key(CredentialKeyError::Io { operation: "secrets_directory" })
                })?;
            let exclusion = match &state.secrets {
                Some(secrets) => vault.exclusion().ensure_excluded(secrets.path()),
                None => Err(CredentialKeyError::BackupExclusionFailed),
            };
            state.exclusion_verified = match exclusion {
                Ok(ExclusionState::Verified) => true,
                Ok(ExclusionState::NotApplicable) => !cfg!(target_os = "macos"),
                Err(_) => false,
            };
            if !state.read_only {
                let status = match exclusion { Ok(ExclusionState::Verified) => CredentialSecretsExclusion::Verified,
                    Ok(ExclusionState::NotApplicable) => CredentialSecretsExclusion::NotApplicable,
                    Err(_) => CredentialSecretsExclusion::Failed };
                state.connection.record_credential_secrets_exclusion(status)?;
            }
            let mut keys = vault.keys.write().map_err(|_| CredentialStoreError::LockPoisoned)?;
            *keys = match &state.secrets {
                None => None,
                Some(secrets) if state.read_only => credential_keys::load_keyring_read_only(secrets)?,
                Some(secrets) => credential_keys::load_keyring(secrets)?,
            };
            if keys.is_some() && vault.prove_keyring(&state).is_err() { state.read_only = true; }
            if !state.read_only && state.exclusion_verified {
                #[cfg(test)] OPEN_CONTROL.with(|control| {
                    if let Some(control) = control.borrow_mut().as_mut() { control(&mut state.connection); }
                });
                if let Err(error) = vault.phase_b(&mut state, &mut keys) {
                    if matches!(error, CredentialStoreError::DurabilityUnavailable) { state.read_only = true; }
                    state.resume_error = Some(error);
                }
            }
            if !state.read_only && keys.as_ref().is_some_and(|k| k.keyring().previous.is_some()) {
                if let Err(error) = vault.rotate_locked(&mut state, &mut keys) {
                    if matches!(error, CredentialStoreError::DurabilityUnavailable) { state.read_only = true; }
                    state.resume_error = Some(error);
                }
            }
        }
        Ok(vault)
    }
    fn exclusion(&self) -> &dyn BackupExclusion {
        #[cfg(any(test, debug_assertions))]
        if let Some(exclusion) = &self.options.exclusion { return exclusion.as_ref(); }
        &SystemBackupExclusion
    }
    fn now_ms(&self) -> Result<i64, CredentialStoreError> {
        #[cfg(test)] if let Some(now) = self.options.now_ms { return Ok(now); }
        SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|v| i64::try_from(v.as_millis()).ok())
            .ok_or(CredentialStoreError::SensitiveAccessUnavailable)
    }
    fn lock(&self) -> Result<VaultGuard<'_>, CredentialStoreError> {
        #[cfg(test)] let seam = self.lock_seam.lock().map_err(|_| CredentialStoreError::LockPoisoned)?.clone();
        #[cfg(test)] if let Some(seam) = &seam { seam.record("request"); }
        let state = self.lock.lock().map_err(|_| CredentialStoreError::LockPoisoned)?;
        #[cfg(test)] if let Some(seam) = &seam { seam.record("acquire"); }
        self.admission.assert_sensitive_access().map_err(|_| CredentialStoreError::SensitiveAccessUnavailable)?;
        Ok(VaultGuard { state: Some(state), #[cfg(test)] seam })
    }
    fn prove_keyring(&self, state: &VaultState) -> Result<(), CredentialStoreError> {
        use std::os::fd::AsFd;
        let secrets = state.secrets.as_ref().unwrap();
        match secrets.inspect_entry(credential_keys::KEYRING_NAME).map_err(|_| CredentialStoreError::DurabilityUnavailable)? {
            EntryState::Regular(fd) => durable_sync_fd(fd.as_fd()).map_err(|_| CredentialStoreError::DurabilityUnavailable)?,
            _ => return Err(CredentialKeyError::KeyringMissing.into()),
        }
        secrets.durable_sync().map_err(|_| CredentialStoreError::DurabilityUnavailable)
    }
    fn phase_b(&self, state: &mut VaultState, keys: &mut Option<KeyringHandle>) -> Result<(), CredentialStoreError> {
        // K7 is needed only while Fleet conversion is pending. Complete opens
        // do not load the cursor key or any pre-upgrade key material.
        let format = state.connection.credential_format_state()?;
        let cursor = if format.fleet_api_state == CredentialSlotFormatState::Pending {
            crate::legacy_owner_credentials::load_existing_cursor_key_bytes(&self.data_dir)
                .map_err(|_| CredentialStoreError::Key(CredentialKeyError::KeyringUnreadable))?
                .map(|bytes| {
                    let key: Zeroizing<[u8; 32]> = Zeroizing::new(bytes.as_slice().try_into().map_err(|_| CredentialStoreError::Key(CredentialKeyError::KeyringInvalid))?);
                    Ok::<_, CredentialStoreError>(crate::protocol::CursorKey::from_bytes(*key))
                }).transpose()?
        } else { None };
        let VaultState { connection, commits_issued, secrets, .. } = state;
        let mut context = CredentialPhaseB { data_dir: &self.data_dir, sqlite_temp_dir: &self.data_dir,
            admission: &self.admission, secrets: secrets.as_ref().unwrap(), keys, cursor_key: cursor.as_ref(),
            exclusion: self.exclusion(), nonces: &mut OsNonces, now_ms: self.now_ms()? };
        credential_migration::run_phase_b(connection, &mut context, &mut || *commits_issued += 1)?;
        // M has passed the barriers of every counted commit before returning.
        state.commits_proven = state.commits_issued;
        Ok(())
    }
    /// TRANSITIONAL: V2 reads presence without exposing a pair or generation.
    pub(crate) fn credential_pair_presence(&self, slot: CredentialSlot) -> Result<Option<CredentialPairPresence>, CredentialStoreError> {
        let _state = self.lock()?;
        self.store.credential_pair_presence(slot).map_err(Into::into)
    }
    pub(crate) fn load(&self, slot: CredentialSlot) -> Result<Option<CredentialPlain>, CredentialStoreError> {
        let mut state = self.lock()?;
        self.guard_read(&mut state)?;
        let format = state.connection.credential_format_state()?;
        let (phase, blocked) = match slot {
            CredentialSlot::OwnerApi => (format.owner_api_state, format.owner_api_blocked),
            CredentialSlot::FleetApi => (format.fleet_api_state, format.fleet_api_blocked),
        };
        if phase == CredentialSlotFormatState::Pending {
            return Err(if blocked.is_some() { CredentialStoreError::MigrationBlocked(slot) } else { CredentialStoreError::MigrationPending(slot) });
        }
        let row = state.connection.load_credential_pair(slot).map_err(|error| match error {
            StoreError::CredentialEnvelope(error) => CredentialStoreError::Envelope { slot, error },
            other => other.into(),
        })?;
        #[cfg(test)] self.pause_read("after_snapshot");
        let keys = self.keys.read().map_err(|_| CredentialStoreError::LockPoisoned)?;
        drop(state);
        row.map(|row| Self::decrypt(&row, keys.as_ref().ok_or(CredentialKeyError::KeyringMissing)?)).transpose()
    }
    fn decrypt(row: &SealedCredentialPair, keys: &KeyringHandle) -> Result<CredentialPlain, CredentialStoreError> {
        let (access, refresh) = row.open(keys).map_err(|error| CredentialStoreError::Envelope { slot: row.slot, error })?;
        if row.refresh_present && !row.refresh_fingerprint.as_ref().is_some_and(|stored|
            crate::protocol::constant_time_eq(stored, &refresh_fingerprint(canonical_token(&refresh)))) {
            return Err(CredentialStoreError::FingerprintMismatch(row.slot));
        }
        fn text(bytes: &[u8]) -> Result<Zeroizing<String>, CredentialStoreError> {
            let source = std::str::from_utf8(bytes).map_err(|_| CredentialStoreError::InvalidTokenBytes)?;
            let mut result = Zeroizing::new(String::with_capacity(source.len())); result.push_str(source); Ok(result)
        }
        Ok(CredentialPlain { slot: row.slot, origin: row.origin, refresh_owner: row.refresh_owner,
            generation: row.generation, access: text(&access)?, refresh: text(&refresh)?, refresh_present: row.refresh_present,
            expires_at: row.expires_at, next_refresh_at: row.next_refresh_at, client_id: row.client_id.clone(), region: row.region.clone() })
    }
    pub(crate) fn stored_generation(&self, slot: CredentialSlot) -> Result<Option<Uuid>, CredentialStoreError> {
        let mut state = self.lock()?; self.guard_read(&mut state)?;
        let result = state.connection.stored_generation(slot).map_err(Into::into);
        #[cfg(test)] self.pause_read("after_snapshot");
        result
    }
    fn guard_read(&self, state: &mut VaultState) -> Result<(), CredentialStoreError> {
        let pending = state.commits_proven < state.commits_issued;
        #[cfg(test)] self.pause_read("after_check");
        if pending { state.barrier()?; #[cfg(test)] self.pause_read("after_barrier"); }
        Ok(())
    }
    #[cfg(test)]
    fn pause_read(&self, point: &'static str) {
        let seam = self.read_pause.lock().unwrap().clone();
        if let Some(seam) = seam { seam.pause(point); }
    }
    pub(crate) fn collector_credential_identity(&self, fleet: bool) -> Result<String, CredentialStoreError> {
        let mut state = self.lock()?; state.guard_read()?;
        state.connection.guarded_collector_credential_identity(fleet).map_err(Into::into)
    }
    pub(crate) fn is_completed_output(&self, slot: CredentialSlot, generation: Uuid) -> Result<bool, CredentialStoreError> {
        let mut state = self.lock()?; state.guard_read()?;
        state.connection.generation_is_refresh_output(slot, generation).map_err(Into::into)
    }
    /// TRANSITIONAL: receipt-scoped proof for V2 aftercare and entry operations.
    #[cfg_attr(not(test), expect(dead_code, reason = "TRANSITIONAL: V2 receipt-scoped proof."))]
    pub(crate) fn is_completed_output_for_receipt(&self, slot: CredentialSlot, receipt: OutboundRequestReceiptId, input: Uuid, output: Uuid) -> Result<bool, CredentialStoreError> {
        let mut state = self.lock()?; state.guard_read()?;
        state.connection.credential_refresh_is_complete(slot, receipt, input, output).map_err(Into::into)
    }
    pub(crate) fn has_unresolved_refresh(&self, slot: CredentialSlot) -> Result<bool, CredentialStoreError> {
        let mut state = self.lock()?;
        state.connection.has_unresolved_credential_refresh(slot).map_err(Into::into)
    }
    pub(crate) fn inspect_fleet_refresh_receipt(&self) -> Result<bool, CredentialStoreError> {
        let mut state = self.lock()?; state.guard_read()?;
        state.connection.inspect_fleet_refresh_receipt().map_err(Into::into)
    }
    /// TRANSITIONAL: V2 consumes the admitted account stop through this view.
    pub(in crate::auth) fn collector_account_stop_view(&self) -> Result<Option<(String, String, String)>, CredentialStoreError> {
        let _state = self.lock()?;
        self.store.collector_account_stop_view().map_err(Into::into)
    }
    pub(crate) fn fill_rejected_access_digest(&self, generation: Uuid, digest: Uuid, fuse_generation: Option<Uuid>, consecutive: u8) -> Result<(), CredentialStoreError> {
        let mut state = self.lock()?; state.writable(false)?; state.guard_read()?;
        state.connection.fill_credential_rejected_access_digest(generation, digest, fuse_generation, consecutive)?;
        state.barrier()
    }
    fn slot_writable(state: &mut VaultState, slot: CredentialSlot) -> Result<(), CredentialStoreError> {
        let format = state.connection.credential_format_state()?;
        let (status, blocked) = match slot { CredentialSlot::OwnerApi => (format.owner_api_state, format.owner_api_blocked),
            CredentialSlot::FleetApi => (format.fleet_api_state, format.fleet_api_blocked) };
        if status == CredentialSlotFormatState::Pending && blocked.is_none() { return Err(CredentialStoreError::MigrationPending(slot)); }
        Ok(())
    }
    pub(crate) fn begin_refresh(&self, slot: CredentialSlot, input: Uuid) -> Result<OutboundRequestReceiptId, CredentialStoreError> {
        let mut state = self.lock()?; state.writable(true)?; state.guard_read()?;
        let VaultState { connection, commits_issued, .. } = &mut *state;
        let receipt = connection.begin_credential_refresh_with_commit(slot, input, &mut || *commits_issued += 1)?;
        if let Err(error) = state.barrier() {
            // No request can have left; release the reservation using R32.
            let VaultState { connection, commits_issued, .. } = &mut *state;
            let _ = connection.cancel_unsent_credential_refresh_with_commit(receipt, input, &mut || *commits_issued += 1);
            return Err(error);
        }
        Ok(receipt)
    }
    pub(crate) fn cancel_refresh(&self, receipt: OutboundRequestReceiptId, input: Uuid) -> Result<(), CredentialStoreError> {
        let mut state = self.lock()?; state.writable(false)?;
        let VaultState { connection, commits_issued, .. } = &mut *state;
        connection.cancel_unsent_credential_refresh_with_commit(receipt, input, &mut || *commits_issued += 1)?;
        state.barrier()
    }
    pub(crate) fn close_refresh(&self, receipt: OutboundRequestReceiptId, input: Uuid, outcome: OutboundRequestOutcome, status: Option<u16>, retry_after: Option<u64>, release: bool) -> Result<(), CredentialStoreError> {
        let mut state = self.lock()?; state.writable(false)?;
        let VaultState { connection, commits_issued, .. } = &mut *state;
        connection.close_credential_refresh_attempt_with_commit(receipt, input, outcome, status, retry_after, release, &mut || *commits_issued += 1)?;
        state.barrier()
    }
    pub(crate) fn persist_refreshed(&self, slot: CredentialSlot, receipt: OutboundRequestReceiptId, input: Uuid, output: Uuid, access: &str, refresh: &str, expires: i64, next: i64) -> Result<RefreshCompletion, CredentialStoreError> {
        if canonical_token(access.as_bytes()) != access.as_bytes() || canonical_token(refresh.as_bytes()) != refresh.as_bytes() {
            return Err(CredentialStoreError::SuccessorNotCanonical);
        }
        if access.is_empty() { return Err(CredentialStoreError::EmptyAccessToken); }
        if refresh.is_empty() { return Err(CredentialStoreError::EmptyRefreshToken); }
        let mut state = self.lock()?; state.writable(true)?; state.guard_read()?;
        if state.connection.credential_refresh_is_complete(slot, receipt, input, output)? {
            state.barrier()?; return Ok(RefreshCompletion::AlreadyComplete);
        }
        let mut row = state.connection.load_credential_pair(slot)?.ok_or(StoreError::CredentialPairInvalid)?;
        #[cfg(test)]
        let fixture_generation = SUCCESSOR_GENERATION_FIXTURE.with(std::cell::Cell::get);
        #[cfg(test)]
        let legacy = match fixture_generation {
            Some(generation) => LegacyCandidates::for_successor_with_old_g(access, refresh, |_, _| Some(generation))?,
            None => LegacyCandidates::for_successor(access, refresh)?,
        };
        #[cfg(not(test))]
        let legacy = LegacyCandidates::for_successor(access, refresh)?;
        let mut keys = self.keys.write().map_err(|_| CredentialStoreError::LockPoisoned)?;
        if keys.is_none() { return Err(CredentialKeyError::KeyringMissing.into()); }
        self.reserve(&mut state, &mut keys, 2)?;
        row.generation = output; row.expires_at = expires; row.next_refresh_at = next;
        row.seal(keys.as_ref().ok_or(CredentialKeyError::KeyringMissing)?, access.as_bytes(), refresh.as_bytes(), &mut OsNonces)?;
        let VaultState { connection, commits_issued, .. } = &mut *state;
        let result = connection.complete_credential_refresh_with_commit(receipt, input, &row, &legacy, &mut || *commits_issued += 1)?;
        #[cfg(test)] if state.block_completion_barriers { state.connection.set_test_barrier_failure(true); }
        state.barrier()?; Ok(result)
    }
    fn reserve(&self, state: &mut VaultState, keys: &mut Option<KeyringHandle>, count: u64) -> Result<(), CredentialStoreError> {
        state.writable(true)?;
        if keys.is_none() {
            let created = credential_keys::create_keyring(state.secrets.as_ref().unwrap(), self.exclusion(), (self.now_ms()? / 1000) as u64);
            *keys = Some(Self::key_result(state, created)?);
        }
        let id = keys.as_ref().unwrap().keyring().current.id;
        if state.connection.credential_reserved_invocations(id)?.checked_add(count).is_none_or(|total| total > credential_keys::FORCED_ROTATION_INVOCATIONS) {
            self.rotate_locked(state, keys)?;
        }
        let id = keys.as_ref().unwrap().keyring().current.id;
        let VaultState { connection, commits_issued, .. } = state;
        connection.reserve_credential_invocations_with_commit(id, count, &mut || *commits_issued += 1)?;
        state.commits_proven = state.commits_issued;
        Ok(())
    }
    /// TRANSITIONAL: V2's rotation command calls this locked entry point.
    pub(crate) fn rotate(&self) -> Result<(), CredentialStoreError> {
        let mut state = self.lock()?; state.writable(true)?;
        let mut keys = self.keys.write().map_err(|_| CredentialStoreError::LockPoisoned)?;
        self.rotate_locked(&mut state, &mut keys)
    }
    #[cfg(test)]
    pub(crate) fn test_rotation_barrier_failure(&self) {
        self.lock().expect("fixture state").connection.set_test_barrier_failure_at(Some(2));
        assert!(matches!(self.rotate(),Err(CredentialStoreError::DurabilityUnavailable)),"rotation cut after row commit");
    }
    fn rotate_locked(&self, state: &mut VaultState, keys: &mut Option<KeyringHandle>) -> Result<(), CredentialStoreError> {
        state.writable(true)?; state.guard_read()?;
        if keys.as_ref().ok_or(CredentialKeyError::KeyringMissing)?.keyring().previous.is_none() {
            let rotated = credential_keys::begin_rotation(state.secrets.as_ref().unwrap(), self.exclusion(), keys.as_ref().unwrap(), (self.now_ms()? / 1000) as u64);
            #[cfg(test)] rotation_key_events();
            *keys = Some(Self::key_result(state, rotated)?);
        }
        #[cfg(test)] {
            let seam = self.lock_seam.lock().unwrap().clone();
            if let Some(seam) = seam { seam.pause_rotation(); }
        }
        let keys_ref = keys.as_ref().unwrap();
        let mut pairs = Vec::new();
        let mut changed = 0;
        for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
            if let Some(row) = state.connection.load_credential_pair(slot)? {
                let current = keys_ref.keyring().current.id;
                let header_current = [&row.access_envelope, &row.refresh_envelope].into_iter().all(|blob| {
                    let header: Result<&[u8; ENVELOPE_HEADER_BYTES], _> = blob.get(..ENVELOPE_HEADER_BYTES).unwrap_or_default().try_into();
                    header.ok().and_then(|header| parse_header(header).ok()).is_some_and(|header| header.key_id == current)
                });
                if !header_current { changed += 1; }
                pairs.push((row, !header_current));
            }
        }
        if changed > 0 {
            let VaultState { connection, commits_issued, .. } = state;
            connection.reserve_credential_invocations_with_commit(keys_ref.keyring().current.id, changed * 2, &mut || {
                *commits_issued += 1;
                #[cfg(test)] rotation_note("reservation_commit");
            })?;
            #[cfg(test)] rotation_note("reservation_barrier");
            state.commits_proven = state.commits_issued;
            for (row, needs_seal) in &mut pairs {
                if *needs_seal {
                    let plain = Self::decrypt(row, keys_ref)?;
                    #[cfg(test)]
                    let sealed = ROTATION_NONCES.with(|source| match source.borrow_mut().as_mut() {
                        Some(source) => row.seal(keys_ref, plain.access.as_bytes(), plain.refresh.as_bytes(), source),
                        None => row.seal(keys_ref, plain.access.as_bytes(), plain.refresh.as_bytes(), &mut OsNonces),
                    });
                    #[cfg(not(test))]
                    let sealed = row.seal(keys_ref, plain.access.as_bytes(), plain.refresh.as_bytes(), &mut OsNonces);
                    sealed?;
                }
            }
            let rows: Vec<_> = pairs.into_iter().map(|(row, _)| row).collect();
            let VaultState { connection, commits_issued, .. } = state;
            #[cfg(test)] RESEAL_CONTROL.with(|control| {
                if let Some(control) = control.borrow_mut().take() { control(connection); }
            });
            connection.reseal_credential_pairs(&rows, &mut || {
                *commits_issued += 1;
                #[cfg(test)] rotation_note("reseal_commit");
            })?;
        }
        state.barrier()?;
        #[cfg(test)] rotation_note("reseal_barrier");
        let finished = credential_keys::finish_rotation(state.secrets.as_ref().unwrap(), keys_ref);
        #[cfg(test)] rotation_key_events();
        *keys = Some(Self::key_result(state, finished)?);
        state.connection.scrub_credential_wal()?;
        #[cfg(test)] if let Some(seam) = self.lock_seam.lock().expect("rotation trace").as_ref() { seam.record("rotation_finished"); }
        Ok(())
    }
    fn key_result(state: &mut VaultState, result: Result<KeyringHandle, CredentialKeyError>) -> Result<KeyringHandle, CredentialStoreError> {
        if matches!(result, Err(CredentialKeyError::DurabilityUnavailable)) { state.read_only = true; }
        result.map_err(Into::into)
    }
    pub(in crate::auth) fn store_supplied_entry(&self, entry: CredentialEntry, policy: ReplacePolicy) -> Result<Uuid, CredentialStoreError> {
        let mut state = self.lock()?; state.writable(true)?; Self::slot_writable(&mut state, entry.slot)?;
        state.connection.check_supplied_credential_entry(entry.slot, entry.refresh.as_bytes(), &entry.legacy, policy)?;
        let generation = Uuid::new_v4();
        let mut row = entry.row(generation);
        let mut keys = self.keys.write().map_err(|_| CredentialStoreError::LockPoisoned)?;
        self.reserve(&mut state, &mut keys, 2)?;
        #[cfg(debug_assertions)] entry::test_boundary("entry_reserved")?;
        if entry.refresh.is_empty() {
            use super::credential_envelope::{CredentialColumn, EnvelopeKeys, EnvelopePlace, seal_value};
            let keys = keys.as_ref().unwrap();
            let id = keys.keyring().current.id;
            let place = EnvelopePlace { table: 1, slot: row.slot, column: CredentialColumn::Access,
                origin: row.origin, owner: row.refresh_owner, account_id: row.account_id, generation };
            let key = keys.envelope_key(id).ok_or(CredentialKeyError::KeyringMissing)?;
            row.access_envelope = seal_value(key, id, &place, entry.access.as_bytes(), &mut OsNonces)
                .map_err(|error| CredentialStoreError::Envelope { slot: row.slot, error })?;
            let refresh_place = EnvelopePlace { column: CredentialColumn::Refresh, ..place };
            row.refresh_envelope = seal_value(key, id, &refresh_place, b"", &mut OsNonces)
                .map_err(|error| CredentialStoreError::Envelope { slot: row.slot, error })?;
        } else {
            row.seal(keys.as_ref().unwrap(), entry.access.as_bytes(), entry.refresh.as_bytes(), &mut OsNonces)?;
        }
        #[cfg(debug_assertions)] entry::test_boundary("entry_sealed")?;
        let VaultState { connection, commits_issued, .. } = &mut *state;
        connection.replace_credential_pair_with_commit(&row, &EntryProvenance::Supplied, &entry.legacy, policy, &mut || *commits_issued += 1)?;
        #[cfg(debug_assertions)] entry::test_boundary("entry_committed")?;
        state.barrier()?; Ok(generation)
    }
    pub(in crate::auth) fn restore_credentials(&self, source: &Path, recovery_key: &[u8])
        -> Result<super::credential_recovery::CredentialRecoveryReport, super::credential_recovery::CredentialRecoveryError>
    {
        use super::credential_recovery::{CredentialRecoveryError, restore_credentials_with_connection};
        let mut state = self.lock().map_err(|error| CredentialRecoveryError::Vault(error.into()))?;
        state.writable(false).map_err(|error| CredentialRecoveryError::Vault(error.into()))?;
        state.guard_read().map_err(|error| CredentialRecoveryError::Vault(error.into()))?;
        restore_credentials_with_connection(&self.store, &mut state.connection, &self.data_dir, source, recovery_key)
    }
    /// TRANSITIONAL: V2 sign-out owns the clear operation and its follow-up scrub.
    pub(crate) fn clear_credential_pair(&self, slot: CredentialSlot) -> Result<(), CredentialStoreError> {
        let mut state = self.lock()?; state.writable(false)?; Self::slot_writable(&mut state, slot)?;
        state.guard_read()?;
        if !state.connection.credential_clear_needed(slot)? { return Ok(()); }
        let VaultState { connection, commits_issued, .. } = &mut *state;
        connection.clear_credential_pair_with_commit(slot, &mut || *commits_issued += 1)?;
        state.barrier()
    }
    /// TRANSITIONAL: the V2 sign-in scope calls this after its preconditions.
    pub(in crate::auth) fn begin_sign_in(&self, slot: CredentialSlot) -> Result<SignInSession, CredentialStoreError> {
        use sha2::{Digest, Sha256};
        let mut state = self.lock()?; state.writable(true)?;
        let mut grant_secret = Zeroizing::new([0u8; 32]); let mut nonce = Zeroizing::new([0u8; 32]);
        getrandom::fill(grant_secret.as_mut()).map_err(|_| CredentialStoreError::Key(CredentialKeyError::EntropyUnavailable))?;
        getrandom::fill(nonce.as_mut()).map_err(|_| CredentialStoreError::Key(CredentialKeyError::EntropyUnavailable))?;
        let mut hash = Sha256::new(); hash.update(b"teslatlas-hub/sign-in-binding/v1\0"); hash.update(grant_secret.as_slice());
        let binding: [u8; 32] = hash.finalize_reset().into();
        let now = self.now_ms()?;
        let expires_at_ms = now.checked_add(600_000).ok_or(CredentialStoreError::SignIn(SignInError::Expired))?;
        let id = state.connection.insert_sign_in_binding(slot, binding, now)?;
        state.barrier()?;
        Ok(SignInSession { id, slot, grant_secret, state: nonce, expires_at_ms,
            drop_observer: super::credential_sign_in::current_secret_observer() })
    }
    /// TRANSITIONAL: V2 supplies a grant from its validated redemption only.
    pub(in crate::auth) fn store_hub_grant(&self, grant: RedeemedGrant) -> Result<StoredHubGrant, CredentialStoreError> {
        let mut state = self.lock()?; state.writable(true)?;
        let now = self.now_ms()?;
        if grant.session.expires_at_ms <= now { return Err(CredentialStoreError::SignIn(SignInError::Expired)); }
        let slot = grant.session.slot;
        let mut keys = self.keys.write().map_err(|_| CredentialStoreError::LockPoisoned)?;
        // E56: only a genuine live binding can recover a missing keyring for a
        // pending slot. Authenticate it before publishing any replacement key.
        state.connection.validate_credential_grant(slot, grant.session.id, &grant.session.grant_secret)?;
        match Self::slot_writable(&mut state, slot) {
            Ok(()) => (),
            Err(CredentialStoreError::MigrationPending(_)) => {
                if keys.is_none() {
                    if !matches!(state.resume_error, Some(CredentialStoreError::Key(CredentialKeyError::KeyringMissing))) { return Err(CredentialStoreError::MigrationPending(slot)); }
                    let created = credential_keys::create_keyring(state.secrets.as_ref().unwrap(), self.exclusion(), (now / 1000) as u64);
                    *keys = Some(Self::key_result(&mut state, created)?);
                }
                self.phase_b(&mut state, &mut keys)?;
                Self::slot_writable(&mut state, slot)?;
            }
            Err(error) => return Err(error),
        }
        let generation = Uuid::new_v4();
        let (expires_at, next_refresh_at) = if slot == CredentialSlot::OwnerApi {
            let lifetime = serde_json::Value::from(grant.expires_in);
            let schedule = crate::legacy_auth::schedule::Schedule::after_send(grant.sent_at_s, Some(&lifetime), &grant.access);
            (schedule.expires_at, schedule.next_refresh_at)
        } else {
            let expires = grant.sent_at_s.checked_add(i64::try_from(grant.expires_in).map_err(|_| CredentialStoreError::SignIn(SignInError::AnswerInvalid))?).ok_or(CredentialStoreError::SignIn(SignInError::AnswerInvalid))?;
            (expires, expires - (grant.expires_in / 4).clamp(1, 1800) as i64)
        };
        let mut row = SealedCredentialPair { account_id: 1, slot,
            origin: Some(if slot == CredentialSlot::OwnerApi { CredentialOrigin::HubLogin } else { CredentialOrigin::FleetOauth }),
            refresh_owner: RefreshOwner::Hub, generation, access_envelope: Vec::new(), refresh_envelope: Vec::new(), refresh_present: true,
            refresh_fingerprint: None, expires_at, next_refresh_at,
            client_id: grant.client_id.clone(), region: grant.region.map(|region| region.storage_code().to_owned()), migrated: false, written_at_ms: 0 };
        let legacy = LegacyCandidates::for_successor(&grant.access, &grant.refresh)?;
        self.reserve(&mut state, &mut keys, 2)?;
        row.seal(keys.as_ref().unwrap(), grant.access.as_bytes(), grant.refresh.as_bytes(), &mut OsNonces)?;
        let provenance = EntryProvenance::HubGrant { sign_in_id: grant.session.id, grant_secret: &grant.session.grant_secret };
        let VaultState { connection, commits_issued, .. } = &mut *state;
        connection.replace_credential_pair_with_commit(&row, &provenance, &legacy, ReplacePolicy::ReplaceAny, &mut || *commits_issued += 1)?;
        state.barrier()?; Ok(StoredHubGrant::new(slot, generation))
    }
    /// TRANSITIONAL: V2 owns release and service restart after this call.
    pub(in crate::auth) fn finish_hub_grant_aftercare(&self, stored: StoredHubGrant) -> Result<(), CredentialStoreError> {
        let mut state = self.lock()?; state.writable(false)?;
        let mut keys = self.keys.write().map_err(|_| CredentialStoreError::LockPoisoned)?;
        let _ = stored;
        self.phase_b(&mut state, &mut keys)
    }
    /// TRANSITIONAL: V2 invokes the bounded WAL scrub after its clear completes.
    pub(crate) fn post_clear_scrub(&self) -> Result<bool, CredentialStoreError> {
        let mut state = self.lock()?; state.writable(false)?;
        state.connection.scrub_credential_wal().map_err(Into::into)
    }
    /// Complete migration cleanup after the independently barriered clears.
    /// The entry retains this process's one vault throughout the operation.
    pub(in crate::auth) fn finish_clear_aftercare(&self) -> Result<(), CredentialStoreError> {
        let mut state = self.lock()?;
        state.writable(false)?;
        state.guard_read()?;
        let mut keys = self.keys.write().map_err(|_| CredentialStoreError::LockPoisoned)?;
        self.phase_b(&mut state, &mut keys)
    }
    /// TRANSITIONAL: V2's explicit handover changes the refresh owner.
    #[cfg_attr(not(test), expect(dead_code, reason = "TRANSITIONAL: V2 refresh-owner handover."))]
    pub(in crate::auth) fn handover_owner_api_refresh_to_hub(&self) -> Result<(), CredentialStoreError> {
        let mut state = self.lock()?; state.writable(false)?; state.guard_read()?;
        let slot = CredentialSlot::OwnerApi;
        let Some(mut row) = state.connection.load_credential_pair(slot)? else { return Ok(()); };
        let mut keys = self.keys.write().map_err(|_| CredentialStoreError::LockPoisoned)?;
        let plain = Self::decrypt(&row, keys.as_ref().ok_or(CredentialKeyError::KeyringMissing)?)?;
        if row.refresh_owner == RefreshOwner::Hub { return Ok(()); }
        // R36 is exempt from exclusion unless its reservation requires rotation.
        let id = keys.as_ref().unwrap().keyring().current.id;
        if state.connection.credential_reserved_invocations(id)? + 2 > credential_keys::FORCED_ROTATION_INVOCATIONS { self.rotate_locked(&mut state, &mut keys)?; }
        let VaultState { connection, commits_issued, .. } = &mut *state;
        connection.reserve_credential_invocations_with_commit(keys.as_ref().unwrap().keyring().current.id, 2, &mut || *commits_issued += 1)?;
        state.commits_proven = state.commits_issued;
        row.refresh_owner = RefreshOwner::Hub;
        row.seal(keys.as_ref().unwrap(), plain.access.as_bytes(), plain.refresh.as_bytes(), &mut OsNonces)?;
        let VaultState { connection, commits_issued, .. } = &mut *state;
        connection.set_credential_refresh_owner_with_commit(&row, row.generation, &mut || *commits_issued += 1)?;
        state.barrier()
    }
}

#[cfg(test)]
impl CredentialVault {
    pub(in crate::auth) fn test_guarded_read_with_exchange(
        self: &Arc<Self>, point: &'static str, release_answer: impl FnOnce(),
    ) -> Option<Uuid> {
        if point == "after_barrier" {
            self.lock().expect("fixture state").connection.set_test_barrier_failure_at(Some(1));
            assert!(matches!(self.clear_credential_pair(CredentialSlot::FleetApi), Err(CredentialStoreError::DurabilityUnavailable)), "CT99 preceding unproven commit");
            self.lock().expect("fixture state").connection.set_test_barrier_failure_at(None);
        }
        let trace = Arc::new(VaultLockSeam::default());
        *self.lock_seam.lock().expect("trace") = Some(Arc::clone(&trace));
        let gate = ReadPauseSeam::new(point);
        *self.read_pause.lock().expect("read seam") = Some(Arc::clone(&gate));
        let reader = { let vault = Arc::clone(self); std::thread::spawn(move || vault.stored_generation(CredentialSlot::OwnerApi)) };
        let reader_id = reader.thread().id(); gate.wait(); release_answer();
        let writer_id = {
            let events = trace.events.lock().expect("events");
            let (events, timeout) = trace.changed.wait_timeout_while(events, std::time::Duration::from_secs(10), |events|
                !events.iter().any(|(id, event)| *id != reader_id && *event == "request")).expect("writer request wait");
            assert!(!timeout.timed_out(), "CT99 detached manager requested the lock");
            let writer = events.iter().find(|(id, event)| *id != reader_id && *event == "request").expect("request acknowledgement").0;
            assert!(!events.iter().any(|(id, event)| *id == writer && *event == "acquire"), "CT99 manager completion waits for the reader");
            writer
        };
        gate.release(); let observed = reader.join().expect("reader thread").expect("preceding snapshot");
        trace.wait_for(Some(writer_id), "acquire");
        let events = trace.events.lock().expect("events");
        let index = |id, event| events.iter().position(|(thread, name)| *thread == id && *name == event).expect("ordered lock event");
        assert!(index(writer_id, "request") < index(reader_id, "release") && index(reader_id, "release") < index(writer_id, "acquire"), "CT99 actual manager completion follows reader release");
        drop(events);
        *self.read_pause.lock().expect("disarm read seam") = None;
        observed
    }
    pub(in crate::auth) fn test_block_completion_barriers(&self, block: bool) {
        let mut state = self.lock().expect("test vault lock");
        state.block_completion_barriers = block;
        if !block { state.connection.set_test_barrier_failure(false); }
    }
    pub(in crate::auth) fn test_commit_counts(&self) -> (u64, u64) {
        let state = self.lock().expect("test vault lock"); (state.commits_issued, state.commits_proven)
    }
}

#[cfg(test)]
#[path = "credential_vault/tests.rs"]
mod g24_m2_v1;
