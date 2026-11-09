// SPDX-License-Identifier: AGPL-3.0-only

//! Phase B of the credential format migration (C10 R43 to R49, R75, R80).
//!
//! Phase B runs in an admitted process, under the data-directory lock, on the
//! once-taken credential connection. It converts the pre-upgrade token rows
//! into sealed pairs, maps fences and attempts, purges residue, scrubs the WAL,
//! removes the neutral key files and records completion. Every step is
//! idempotent and re-runnable at the next admitted open. Plaintext exists only
//! between steps 4 and 5, for one slot at a time, in zeroising buffers.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};

use rustix::{
    fs::{fstat, statvfs},
    process::geteuid,
};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    auth::{
        credential_envelope::{
            CredentialOrigin, CredentialSlot, EnvelopeError, NonceSource, RefreshOwner,
            canonical_token,
        },
        credential_keys::{
            BackupExclusion, CredentialKeyError, ExclusionState, KeyringHandle, create_keyring,
            load_keyring,
        },
    },
    credentials::OwnerTokens,
    db::{
        CredentialConnection, CredentialFormatState, CredentialMigrationAttempt,
        CredentialMigrationBlockReason, CredentialMigrationLegacyFence, CredentialMigrationPlan,
        CredentialMigrationRefreshFence, CredentialSecretsExclusion, CredentialSlotFormatState,
        LegacyFenceIdentity, OutboundRequestOutcome, PreUpgradeReceiptState,
        PreUpgradeRefreshBinding, SealedCredentialPair, StoreError,
    },
    hub_user_process::AdmittedUserHub,
    platform::private_directory::{EntryState, PrivateDirectory},
    protocol::CursorKey,
    source_token_envelope::{
        SourceDbTokenError, decrypt_legacy_owner_tokens, legacy_refresh_credential_generation,
    },
};

/// The last store version before the credential-format rung (R50).
pub(crate) const LAST_PRE_C10_VERSION: i32 = 78;
const REL_K1: &str = "pre-upgrade-owner-api.relocated.key";
const RST_K1: &str = "pre-upgrade-owner-api.key";
const REL_K2: &str = "pre-upgrade-owner-api.relocated-previous.key";
const RST_K2: &str = "pre-upgrade-fleet-api.key";
const OLD_K4: &str = "fleet-credentials.key";
const OLD_K5: &str = ".fleet-credentials.pending.key";
const OLD_K6: &str = ".fleet-credentials-key-migration";
/// R45's candidate order, neutral names only (R80).
const OWNER_CANDIDATES: [&str; 3] = [REL_K1, RST_K1, REL_K2];
const FLEET_CANDIDATES: [&str; 3] = [OLD_K4, RST_K2, OLD_K5];
/// R49's removal list.
const PRE_UPGRADE_FILES: [&str; 7] = [REL_K1, REL_K2, RST_K1, RST_K2, OLD_K4, OLD_K5, OLD_K6];
const OWNER_KEY_MAX_BYTES: u64 = 16_384;
const FLEET_KEY_BYTES: u64 = 32;
const STAGING_PREFIX: &str = ".hub-restore-";
const STAGING_SUFFIX: &str = ".tmp";

/// M's own floor read (R50, M2P-85 item 8): production takes the store's
/// minimum version; an M-owned cfg(test) override raises it.
pub(crate) fn store_floor() -> i32 {
    #[cfg(test)]
    if let Some(floor) = seam::with(|state| state.floor) {
        return floor;
    }
    crate::db::MIN_STORE_VERSION
}

/// Whether `name` is exactly `.hub-restore-<canonical lowercase hyphenated
/// UUID v4>.tmp` (E58): no prefix, case or near-match rule.
pub(crate) fn is_restore_staging_name(name: &str) -> bool {
    let Some(inner) = name
        .strip_prefix(STAGING_PREFIX)
        .and_then(|rest| rest.strip_suffix(STAGING_SUFFIX))
    else {
        return false;
    };
    match Uuid::parse_str(inner) {
        Ok(uuid) => uuid.get_version_num() == 4 && uuid.hyphenated().to_string() == inner,
        Err(_) => false,
    }
}

/// Records one M-level operation (test builds only).
pub(crate) fn note_event(event: &str) {
    #[cfg(test)]
    seam::note(event);
    #[cfg(not(test))]
    let _ = event;
}

/// A named boundary of a run; `false` stops the run (test builds only).
pub(crate) fn boundary_continues(point: &str) -> bool {
    #[cfg(test)]
    return seam::boundary(point);
    #[cfg(not(test))]
    {
        let _ = point;
        true
    }
}

/// Everything an admitted vault supplies to one Phase B run.
pub(crate) struct CredentialPhaseB<'a> {
    pub(crate) data_dir: &'a Path,
    pub(crate) sqlite_temp_dir: &'a Path,
    pub(crate) admission: &'a std::sync::Arc<AdmittedUserHub>,
    pub(crate) secrets: &'a PrivateDirectory,
    pub(crate) keys: &'a mut Option<KeyringHandle>,
    pub(crate) cursor_key: Option<&'a CursorKey>,
    pub(crate) exclusion: &'a dyn BackupExclusion,
    pub(crate) nonces: &'a mut dyn NonceSource,
    pub(crate) now_ms: i64,
}

/// The migration receipt of one run.
#[derive(Debug)]
pub(crate) struct CredentialMigrationReport {
    pub(crate) state: CredentialFormatState,
    pub(crate) unsafe_candidates: Vec<&'static str>,
    pub(crate) opaque_binding_outputs: u64,
}

pub(crate) enum CredentialMigrationError {
    Key(CredentialKeyError),
    Envelope(EnvelopeError),
    Store(StoreError),
    Io { operation: &'static str },
}

impl std::fmt::Debug for CredentialMigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Key(error) => f.debug_tuple("Key").field(error).finish(),
            Self::Envelope(error) => f.debug_tuple("Envelope").field(error).finish(),
            Self::Store(error) => f.debug_tuple("Store").field(error).finish(),
            Self::Io { operation } => f.debug_struct("Io").field("operation", operation).finish(),
        }
    }
}

impl From<StoreError> for CredentialMigrationError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<EnvelopeError> for CredentialMigrationError {
    fn from(error: EnvelopeError) -> Self {
        Self::Envelope(error)
    }
}

impl From<CredentialKeyError> for CredentialMigrationError {
    fn from(error: CredentialKeyError) -> Self {
        Self::Key(error)
    }
}

type PhaseResult<T> = Result<T, CredentialMigrationError>;

enum SlotOutcome {
    Converted(Box<SealedCredentialPair>),
    Blocked(CredentialMigrationBlockReason),
}

fn proceed(point: &str) -> PhaseResult<()> {
    if boundary_continues(point) {
        Ok(())
    } else {
        Err(CredentialMigrationError::Io {
            operation: "test_stop",
        })
    }
}

fn slot_state(state: &CredentialFormatState, slot: CredentialSlot) -> CredentialSlotFormatState {
    match slot {
        CredentialSlot::OwnerApi => state.owner_api_state,
        CredentialSlot::FleetApi => state.fleet_api_state,
    }
}

fn directory_barrier(secrets: &PrivateDirectory) -> PhaseResult<()> {
    #[cfg(test)]
    {
        let fail = seam::with(|state| {
            state.directory_barriers += 1;
            state.directory_barrier_fail_at == Some(state.directory_barriers)
        });
        seam::note("directory_barrier");
        if fail {
            return Err(CredentialKeyError::DurabilityUnavailable.into());
        }
    }
    secrets
        .durable_sync()
        .map_err(|_| CredentialKeyError::DurabilityUnavailable.into())
}

/// R44 step 2: load the keyring, or create it when no credential row exists.
fn ensure_keyring(
    connection: &mut CredentialConnection,
    context: &mut CredentialPhaseB<'_>,
) -> PhaseResult<()> {
    if context.keys.is_some() {
        return Ok(());
    }
    match load_keyring(context.secrets)? {
        Some(handle) => {
            *context.keys = Some(handle);
            Ok(())
        }
        None => {
            let rows = connection
                .stored_generation(CredentialSlot::OwnerApi)?
                .is_some()
                || connection
                    .stored_generation(CredentialSlot::FleetApi)?
                    .is_some();
            if rows {
                return Err(CredentialKeyError::KeyringMissing.into());
            }
            // R18: the exclusion is verified before the keyring exists, and the
            // outcome is recorded either way.
            match context.exclusion.ensure_excluded(context.secrets.path()) {
                Ok(ExclusionState::Verified) => {
                    connection.record_credential_secrets_exclusion(
                        CredentialSecretsExclusion::Verified,
                    )?;
                }
                Ok(ExclusionState::NotApplicable) => {
                    connection.record_credential_secrets_exclusion(
                        CredentialSecretsExclusion::NotApplicable,
                    )?;
                }
                Err(error) => {
                    let _ = connection
                        .record_credential_secrets_exclusion(CredentialSecretsExclusion::Failed);
                    return Err(error.into());
                }
            }
            let now_s = u64::try_from(context.now_ms / 1000).unwrap_or_default();
            let handle = create_keyring(context.secrets, context.exclusion, now_s)?;
            *context.keys = Some(handle);
            Ok(())
        }
    }
}

/// One candidate key file (R45): inspected without following links, with the
/// owner and size checks on the open descriptor, and read once.
fn read_candidate(
    secrets: &PrivateDirectory,
    name: &'static str,
    exact_size: bool,
    unsafe_names: &mut Vec<&'static str>,
) -> PhaseResult<Option<Zeroizing<Vec<u8>>>> {
    note_event(&format!("inspect:{name}"));
    let skip = |unsafe_names: &mut Vec<&'static str>| {
        if !unsafe_names.contains(&name) {
            unsafe_names.push(name);
        }
    };
    let descriptor =
        match secrets
            .inspect_entry(name)
            .map_err(|_| CredentialMigrationError::Io {
                operation: "inspect_candidate",
            })? {
            EntryState::Absent => return Ok(None),
            EntryState::Unsafe(_) => {
                skip(unsafe_names);
                return Ok(None);
            }
            EntryState::Regular(descriptor) => descriptor,
        };
    let facts = fstat(&descriptor).map_err(|_| CredentialMigrationError::Io {
        operation: "stat_candidate",
    })?;
    let size = u64::try_from(facts.st_size).unwrap_or_default();
    let size_ok = if exact_size {
        size == FLEET_KEY_BYTES
    } else {
        (1..=OWNER_KEY_MAX_BYTES).contains(&size)
    };
    if facts.st_uid != geteuid().as_raw() || !size_ok {
        skip(unsafe_names);
        return Ok(None);
    }
    note_event(&format!("read:{name}"));
    let mut bytes = Zeroizing::new(Vec::with_capacity(size as usize + 1));
    File::from(descriptor)
        .take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CredentialMigrationError::Io {
            operation: "read_candidate",
        })?;
    if bytes.len() as u64 != size {
        skip(unsafe_names);
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// R45: candidate keys in order, identical contents once.
fn candidate_keys(
    context: &CredentialPhaseB<'_>,
    slot: CredentialSlot,
    unsafe_names: &mut Vec<&'static str>,
) -> PhaseResult<Vec<Zeroizing<Vec<u8>>>> {
    let (names, exact) = match slot {
        CredentialSlot::OwnerApi => (OWNER_CANDIDATES, false),
        CredentialSlot::FleetApi => (FLEET_CANDIDATES, true),
    };
    let mut keys: Vec<Zeroizing<Vec<u8>>> = Vec::new();
    for name in names {
        if let Some(key) = read_candidate(context.secrets, name, exact, unsafe_names)?
            && !keys.iter().any(|known| known.as_slice() == key.as_slice())
        {
            keys.push(key);
        }
    }
    if slot == CredentialSlot::FleetApi
        && let Some(cursor) = context.cursor_key
    {
        let derived = Zeroizing::new(cursor.fleet_credential_encryption_key());
        let key = Zeroizing::new(derived.to_vec());
        if !keys.iter().any(|known| known.as_slice() == key.as_slice()) {
            keys.push(key);
        }
    }
    Ok(keys)
}

/// What one candidate key did to a slot's old ciphertext.
enum Decrypted {
    /// OLD-D authenticated both values and the token pair was accepted.
    Tokens(OwnerTokens),
    /// OLD-D authenticated the values but the canonical token pair was refused:
    /// the slot is `old_pair_invalid` and no further candidate is tried.
    Refused,
    /// The key does not authenticate: the next candidate is tried.
    NoMatch,
}

/// OLD-D, called opaquely. `SourceDbTokenError::InvalidPlaintext` is the
/// variant OLD-D returns when it authenticates the plaintext but
/// `OwnerTokens` refuses the canonical token pair (coordinator fact,
/// M2P-86 item 7); any other error means the candidate does not authenticate.
fn decrypt_pre_upgrade(
    slot: CredentialSlot,
    key: &[u8],
    access: &[u8],
    refresh: &[u8],
) -> Decrypted {
    #[cfg(test)]
    {
        seam::with(|state| state.decrypt_log.push((slot, key.to_vec())));
        if seam::with(|state| state.fake_pre_upgrade) {
            let prefix = format!(
                "OLD:{}:",
                key.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            let (Some(access), Some(refresh)) = (
                access.strip_prefix(prefix.as_bytes()),
                refresh.strip_prefix(prefix.as_bytes()),
            ) else {
                return Decrypted::NoMatch;
            };
            return match OwnerTokens::from_file_bytes(
                Zeroizing::new(access.to_vec()),
                Zeroizing::new(refresh.to_vec()),
            ) {
                Ok(tokens) => Decrypted::Tokens(tokens),
                Err(_) => Decrypted::Refused,
            };
        }
    }
    let _ = slot;
    classify_decryption(decrypt_legacy_owner_tokens(key, access, refresh))
}

/// The arm that maps OLD-D's verdict to the slot's fate (R44 steps 3 and 4,
/// R48): `InvalidPlaintext` is authenticated but refused (a coordinator fact,
/// M2P-86 item 7), every other error means the candidate does not
/// authenticate.
fn classify_decryption(result: Result<OwnerTokens, SourceDbTokenError>) -> Decrypted {
    match result {
        Ok(tokens) => Decrypted::Tokens(tokens),
        Err(SourceDbTokenError::InvalidPlaintext) => Decrypted::Refused,
        Err(_) => Decrypted::NoMatch,
    }
}

/// OLD-G, called opaquely.
fn generation_for(tokens: &OwnerTokens) -> Uuid {
    #[cfg(test)]
    if seam::with(|state| state.fake_generation) {
        return Uuid::new_v5(
            &Uuid::from_u128(0x00000000_0000_4000_8000_000000000003),
            tokens.refresh_token().as_bytes(),
        );
    }
    legacy_refresh_credential_generation(tokens)
}

/// R46's refresh owner: Hub for the Fleet slot unconditionally; Hub for the
/// Legacy slot only when its generation is the output of a completed,
/// successful binding (M2P-73(a)); a Started or failed binding never counts.
fn refresh_owner(
    slot: CredentialSlot,
    generation: Uuid,
    bindings: &[PreUpgradeRefreshBinding],
) -> RefreshOwner {
    if slot == CredentialSlot::FleetApi {
        return RefreshOwner::Hub;
    }
    let evidence = bindings.iter().any(|binding| {
        binding.output_generation == Some(generation)
            && matches!(
                binding.receipt_state,
                PreUpgradeReceiptState::Completed(OutboundRequestOutcome::Success)
            )
    });
    if evidence {
        RefreshOwner::Hub
    } else {
        RefreshOwner::External
    }
}

/// R44 steps 3 and 4 for one slot: select the key, decrypt, build and seal the
/// migrated pair. The plaintext is dropped before this function returns.
fn convert_slot(
    connection: &mut CredentialConnection,
    context: &mut CredentialPhaseB<'_>,
    slot: CredentialSlot,
    bindings: &[PreUpgradeRefreshBinding],
    unsafe_names: &mut Vec<&'static str>,
) -> PhaseResult<SlotOutcome> {
    let row = connection
        .read_pre_upgrade_credential_row(slot)?
        .ok_or(StoreError::CredentialPairInvalid)?;
    let candidates = candidate_keys(context, slot, unsafe_names)?;
    let mut found = None;
    for key in &candidates {
        match decrypt_pre_upgrade(slot, key, &row.encrypted_access, &row.encrypted_refresh) {
            Decrypted::Tokens(tokens) => {
                found = Some(tokens);
                break;
            }
            Decrypted::Refused => {
                return Ok(SlotOutcome::Blocked(
                    CredentialMigrationBlockReason::OldPairInvalid,
                ));
            }
            Decrypted::NoMatch => {}
        }
    }
    let Some(tokens) = found else {
        return Ok(SlotOutcome::Blocked(
            CredentialMigrationBlockReason::OldKeyUnavailable,
        ));
    };
    let access = canonical_token(tokens.access_token().as_bytes());
    let refresh = canonical_token(tokens.refresh_token().as_bytes());
    if access.is_empty() || refresh.is_empty() {
        return Ok(SlotOutcome::Blocked(
            CredentialMigrationBlockReason::OldPairInvalid,
        ));
    }
    let generation = row.generation.unwrap_or_else(|| generation_for(&tokens));
    let fleet = slot == CredentialSlot::FleetApi;
    let mut pair = SealedCredentialPair {
        account_id: 1,
        slot,
        origin: fleet.then_some(CredentialOrigin::FleetOauth),
        refresh_owner: refresh_owner(slot, generation, bindings),
        generation,
        access_envelope: Vec::new(),
        refresh_envelope: Vec::new(),
        refresh_present: true,
        refresh_fingerprint: None,
        expires_at: row.expires_at,
        next_refresh_at: row.next_refresh_at,
        client_id: row.client_id.clone(),
        region: row.region.clone(),
        migrated: true,
        written_at_ms: context.now_ms,
    };
    let keys = context
        .keys
        .as_ref()
        .ok_or(CredentialKeyError::KeyringMissing)?;
    connection.reserve_and_seal_credential_pair(
        &mut pair,
        keys,
        access,
        refresh,
        &mut *context.nonces,
    )?;
    Ok(SlotOutcome::Converted(Box::new(pair)))
}

struct PlanParts {
    pairs: Vec<SealedCredentialPair>,
    blocked: Vec<(CredentialSlot, CredentialMigrationBlockReason)>,
    attempts: Vec<CredentialMigrationAttempt>,
    legacy_fences: Vec<CredentialMigrationLegacyFence>,
    refresh_fences: Vec<CredentialMigrationRefreshFence>,
    opaque_binding_outputs: u64,
}

/// R47: attempts, consolidated legacy fences with their identities and the
/// migrated fingerprint fences. S validates every part inside its transaction.
fn initial_mapping(
    connection: &mut CredentialConnection,
    now_ms: i64,
    owner: &[PreUpgradeRefreshBinding],
    fleet: &[PreUpgradeRefreshBinding],
    parts: &mut PlanParts,
) -> PhaseResult<()> {
    let mut used_ids: BTreeSet<Uuid> = BTreeSet::new();
    for (slot, bindings) in [
        (CredentialSlot::OwnerApi, owner),
        (CredentialSlot::FleetApi, fleet),
    ] {
        for binding in bindings {
            let mut attempt_id = binding.attempt_id;
            // An attempt id present in both stores gets a fresh UUID v4 on the
            // Fleet copy; ids label rows and nothing refers to them.
            while used_ids.contains(&attempt_id) {
                attempt_id = Uuid::new_v4();
            }
            used_ids.insert(attempt_id);
            parts.attempts.push(CredentialMigrationAttempt {
                receipt_id: binding.receipt_id,
                attempt_id,
                slot,
                input_generation: binding.input_generation,
                output_generation: binding.output_generation,
            });
        }
    }
    let mut fenced: BTreeSet<Uuid> = BTreeSet::new();
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        fenced.extend(connection.read_pre_upgrade_refresh_fences(slot)?);
    }
    let outputs: BTreeSet<Uuid> = owner
        .iter()
        .chain(fleet)
        .filter_map(|binding| binding.output_generation)
        .collect();
    let mut earliest: BTreeMap<Uuid, i64> = BTreeMap::new();
    for binding in owner.iter().chain(fleet) {
        earliest
            .entry(binding.input_generation)
            .and_modify(|time| *time = (*time).min(binding.started_at_ms))
            .or_insert(binding.started_at_ms);
    }
    for generation in &fenced {
        let anchored = parts
            .pairs
            .iter()
            .any(|pair| pair.generation == *generation);
        if !anchored && outputs.contains(generation) {
            parts.opaque_binding_outputs += 1;
        }
        parts.legacy_fences.push(CredentialMigrationLegacyFence {
            generation: *generation,
            identity: if anchored {
                LegacyFenceIdentity::Anchored
            } else {
                LegacyFenceIdentity::Opaque
            },
            fenced_at_ms: earliest.get(generation).copied().unwrap_or(now_ms),
        });
    }
    let mut fingerprints: BTreeSet<[u8; 32]> = BTreeSet::new();
    for pair in &parts.pairs {
        if fenced.contains(&pair.generation) {
            let fingerprint = pair
                .refresh_fingerprint
                .ok_or(StoreError::CredentialPairInvalid)?;
            if fingerprints.insert(fingerprint) {
                parts.refresh_fences.push(CredentialMigrationRefreshFence {
                    fingerprint,
                    fenced_at_ms: now_ms,
                });
            }
        }
    }
    Ok(())
}

/// R75 step 1: the shortfall of free space for the purge, zero when admitted.
fn purge_shortfall(context: &CredentialPhaseB<'_>) -> PhaseResult<u64> {
    let io = |operation| CredentialMigrationError::Io { operation };
    let catalogue = std::fs::metadata(context.data_dir.join("hub.sqlite"))
        .map_err(|_| io("catalogue_size"))?
        .len();
    let free = |dir: &Path| -> PhaseResult<u64> {
        let stat = statvfs(dir).map_err(|_| io("free_space"))?;
        Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
    };
    let (free_data, free_temp) = (free(context.data_dir)?, free(context.sqlite_temp_dir)?);
    #[cfg(test)]
    let (free_data, free_temp) =
        seam::with(|state| state.free_bytes).unwrap_or((free_data, free_temp));
    #[cfg(unix)]
    let same_volume = {
        use std::os::unix::fs::MetadataExt;
        let dev = |dir: &Path| std::fs::metadata(dir).map(|meta| meta.dev());
        dev(context.data_dir).map_err(|_| io("volume"))?
            == dev(context.sqlite_temp_dir).map_err(|_| io("volume"))?
    };
    let mut shortfall = catalogue.saturating_mul(2).saturating_sub(free_data);
    if !same_volume {
        shortfall = shortfall.saturating_add(catalogue.saturating_sub(free_temp));
    }
    Ok(shortfall)
}

/// R44 step 8 with R49 and E58: remove every present neutral key file and
/// exact per-key staging file through the directory descriptor, then the
/// directory barrier, then (only then) the marker.
fn remove_neutral_files(
    connection: &mut CredentialConnection,
    context: &CredentialPhaseB<'_>,
    state: &CredentialFormatState,
    mut barrier_done: bool,
    unsafe_names: &mut Vec<&'static str>,
) -> PhaseResult<()> {
    let listed = context
        .secrets
        .entry_names()
        .map_err(|_| CredentialMigrationError::Io {
            operation: "list_secrets",
        })?;
    for name in &listed {
        note_event(&format!("list_returned:{name}"));
    }
    let mut targets: Vec<String> = PRE_UPGRADE_FILES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    targets.extend(
        listed
            .iter()
            .filter(|name| is_restore_staging_name(name))
            .cloned(),
    );
    let mut removed_any = false;
    for name in targets {
        note_event(&format!("inspect:{name}"));
        let entry =
            context
                .secrets
                .inspect_entry(&name)
                .map_err(|_| CredentialMigrationError::Io {
                    operation: "inspect_removal",
                })?;
        match entry {
            EntryState::Absent => {}
            EntryState::Unsafe(_) => {
                if let Some(known) = PRE_UPGRADE_FILES.iter().find(|known| **known == name)
                    && !unsafe_names.contains(known)
                {
                    unsafe_names.push(known);
                }
            }
            EntryState::Regular(descriptor) => {
                drop(descriptor);
                // R49: no file is removed before the conversion commit's
                // barrier has passed, in this open or an earlier one.
                if !barrier_done {
                    connection.credential_durability_barrier()?;
                    barrier_done = true;
                }
                note_event(&format!("remove:{name}"));
                context
                    .secrets
                    .remove(&name)
                    .map_err(|_| CredentialMigrationError::Io {
                        operation: "remove_candidate",
                    })?;
                removed_any = true;
                proceed(&format!("after_removal:{name}"))?;
            }
        }
    }
    if removed_any || !state.old_files_removed {
        directory_barrier(context.secrets)?;
    }
    if !state.old_files_removed {
        connection.mark_credential_files_removed()?;
        note_event("files_marker");
        proceed("after_files_marker")?;
    }
    Ok(())
}

pub(crate) fn run_phase_b(
    connection: &mut CredentialConnection,
    context: &mut CredentialPhaseB<'_>,
    before_commit: &mut dyn FnMut(),
) -> Result<CredentialMigrationReport, CredentialMigrationError> {
    let mut unsafe_names: Vec<&'static str> = Vec::new();
    let mut opaque_binding_outputs = 0u64;
    // The data-directory lock and its descriptors are revalidated before any work.
    context
        .admission
        .assert_sensitive_access()
        .map_err(|_| CredentialMigrationError::Io {
            operation: "admission",
        })?;
    let mut state = connection.credential_format_state()?;
    // R50: above the recorded floor raise, no pre-C10 data is read or decrypted.
    if store_floor() > LAST_PRE_C10_VERSION && state.converted_at_ms.is_none() {
        return Err(StoreError::UnsupportedSchema(LAST_PRE_C10_VERSION).into());
    }
    let mut committed_now = false;
    // Step 1: a completed state jumps to step 8.
    if state.completed_at_ms.is_none() {
        if state.converted_at_ms.is_none() {
            let pending_slots: Vec<CredentialSlot> =
                [CredentialSlot::OwnerApi, CredentialSlot::FleetApi]
                    .into_iter()
                    .filter(|slot| slot_state(&state, *slot) == CredentialSlotFormatState::Pending)
                    .collect();
            // Step 2.
            if !pending_slots.is_empty() {
                ensure_keyring(connection, context)?;
                proceed("after_keyring")?;
            }
            let owner_bindings =
                connection.read_pre_upgrade_refresh_bindings(CredentialSlot::OwnerApi)?;
            let fleet_bindings =
                connection.read_pre_upgrade_refresh_bindings(CredentialSlot::FleetApi)?;
            let mut parts = PlanParts {
                pairs: Vec::new(),
                blocked: Vec::new(),
                attempts: Vec::new(),
                legacy_fences: Vec::new(),
                refresh_fences: Vec::new(),
                opaque_binding_outputs: 0,
            };
            // Steps 3 and 4, one slot at a time.
            for slot in pending_slots {
                let bindings = match slot {
                    CredentialSlot::OwnerApi => &owner_bindings,
                    CredentialSlot::FleetApi => &fleet_bindings,
                };
                match convert_slot(connection, context, slot, bindings, &mut unsafe_names)? {
                    SlotOutcome::Converted(pair) => parts.pairs.push(*pair),
                    SlotOutcome::Blocked(reason) => parts.blocked.push((slot, reason)),
                }
            }
            if state.fences_pending {
                initial_mapping(
                    connection,
                    context.now_ms,
                    &owner_bindings,
                    &fleet_bindings,
                    &mut parts,
                )?;
            }
            // Step 5: one counted commit, then its durability barrier (inside S).
            note_event("conversion_commit");
            let plan = CredentialMigrationPlan {
                pairs: &parts.pairs,
                blocked_slots: &parts.blocked,
                attempts: &parts.attempts,
                legacy_fences: &parts.legacy_fences,
                refresh_fences: &parts.refresh_fences,
                now_ms: context.now_ms,
            };
            connection.commit_credential_migration(&plan, before_commit)?;
            note_event("conversion_barrier");
            opaque_binding_outputs = parts.opaque_binding_outputs;
            committed_now = true;
            drop(parts);
            proceed("after_conversion")?;
            state = connection.credential_format_state()?;
        }
        // R44 step 5, last sentence: when the conversion did not commit in this
        // run, a failed barrier of an earlier open is repeated before going on.
        if state.converted_at_ms.is_some() && !committed_now {
            connection.credential_durability_barrier()?;
            note_event("conversion_barrier_repeat");
            committed_now = true;
        }
        // Step 6: the residue purge.
        if state.converted_at_ms.is_some() && state.purge_required {
            let shortfall = purge_shortfall(context)?;
            if shortfall > 0 {
                connection.defer_credential_purge(shortfall)?;
            } else {
                proceed("before_vacuum")?;
                note_event("vacuum");
                connection.vacuum_credential_catalogue()?;
                note_event("purge_commit");
                connection.commit_credential_purge(before_commit)?;
                proceed("after_purge_commit")?;
            }
            state = connection.credential_format_state()?;
        }
        // Step 7: the recorded WAL scrub.
        if state.converted_at_ms.is_some() && !state.purge_required && !state.wal_scrubbed {
            connection.mark_credential_wal_scrubbed()?;
            note_event("scrub_marker");
            proceed("after_scrub_marker")?;
            state = connection.credential_format_state()?;
        }
    }
    // Step 8: neutral file removal, whenever converted.
    if state.converted_at_ms.is_some() {
        remove_neutral_files(
            connection,
            context,
            &state,
            committed_now,
            &mut unsafe_names,
        )?;
        state = connection.credential_format_state()?;
        // Step 9: completion, only after every cleanup step.
        if state.completed_at_ms.is_none()
            && !state.purge_required
            && state.wal_scrubbed
            && state.old_files_removed
        {
            connection.mark_credential_migration_complete(context.now_ms)?;
            note_event("complete_marker");
            proceed("after_completion")?;
            state = connection.credential_format_state()?;
        }
    }
    Ok(CredentialMigrationReport {
        state,
        unsafe_candidates: unsafe_names,
        opaque_binding_outputs,
    })
}

/// Test-only seams of spec section 10 (decrypt seams, available-bytes seam,
/// boundary seam, the directory-barrier seam, the floor seam, the payload seam
/// and the event log that stands in for `IoSpy` at M's own call level). None
/// changes production behaviour: production never consults them.
#[cfg(test)]
pub(crate) mod seam {
    use std::{cell::RefCell, path::Path};

    use crate::auth::credential_envelope::CredentialSlot;

    pub(crate) enum PayloadAction {
        Continue,
        /// The stage fails; the backup removes its unpublished payload.
        Fail,
        /// The run stops as a crash would: nothing is cleaned up.
        Crash,
    }

    #[derive(Default)]
    pub(crate) struct State {
        pub(crate) fake_pre_upgrade: bool,
        pub(crate) fake_generation: bool,
        pub(crate) decrypt_log: Vec<(CredentialSlot, Vec<u8>)>,
        pub(crate) free_bytes: Option<(u64, u64)>,
        pub(crate) boundary: Option<Box<dyn FnMut(&str) -> bool>>,
        pub(crate) directory_barrier_fail_at: Option<usize>,
        pub(crate) directory_barriers: usize,
        pub(crate) events: Vec<String>,
        pub(crate) floor: Option<i32>,
        pub(crate) payload_hook: Option<Box<dyn FnMut(&str, &Path) -> PayloadAction>>,
        pub(crate) crashed: bool,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
    }

    pub(crate) fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
        STATE.with(|state| f(&mut state.borrow_mut()))
    }

    pub(crate) fn reset() {
        with(|state| *state = State::default());
    }

    pub(crate) fn note(event: impl Into<String>) {
        with(|state| state.events.push(event.into()));
    }

    /// Runs the boundary hook outside the state borrow. `false` stops the run.
    pub(crate) fn boundary(name: &str) -> bool {
        let hook = with(|state| state.boundary.take());
        let Some(mut hook) = hook else {
            return true;
        };
        let proceed = hook(name);
        with(|state| {
            if state.boundary.is_none() {
                state.boundary = Some(hook);
            }
        });
        proceed
    }

    /// One pre-upgrade token row for [`seed_pre_upgrade_store`].
    pub(crate) struct PreRow {
        pub(crate) access: Vec<u8>,
        pub(crate) refresh: Vec<u8>,
        pub(crate) expires_at: i64,
        pub(crate) next_refresh_at: i64,
        pub(crate) generation: Option<uuid::Uuid>,
    }

    /// Turns a freshly initialised store into a hand-built pre-upgrade store:
    /// the six predecessor tables of A3664 to A3669, the given rows and a
    /// `pending` state row (spec section 10: partial masks never occur here).
    pub(crate) fn seed_pre_upgrade_store(
        db: &rusqlite::Connection,
        owner: Option<&PreRow>,
        fleet: Option<&PreRow>,
        fences_pending: bool,
    ) {
        for ddl in [
            "CREATE TABLE legacy_owner_tokens (singleton_id INTEGER PRIMARY KEY,
              access BLOB NOT NULL, refresh BLOB NOT NULL, expires_at INTEGER NOT NULL,
              next_refresh_at INTEGER NOT NULL, credential_generation TEXT) STRICT;",
            "CREATE TABLE fleet_tokens (singleton_id INTEGER PRIMARY KEY,
              access BLOB NOT NULL, refresh BLOB NOT NULL, client_id TEXT NOT NULL,
              region TEXT NOT NULL, expires_at INTEGER NOT NULL, next_refresh_at INTEGER NOT NULL,
              credential_generation TEXT) STRICT;",
            "CREATE TABLE legacy_refresh_receipt_bindings (
              receipt_id INTEGER PRIMARY KEY REFERENCES outbound_request_receipts(id) ON DELETE CASCADE,
              attempt_id TEXT NOT NULL UNIQUE, input_credential_generation TEXT NOT NULL,
              output_credential_generation TEXT) STRICT;",
            "CREATE TABLE legacy_refresh_input_fences (
              input_credential_generation TEXT PRIMARY KEY NOT NULL COLLATE NOCASE) STRICT, WITHOUT ROWID;",
            "CREATE TABLE fleet_refresh_receipt_bindings (
              receipt_id INTEGER PRIMARY KEY REFERENCES outbound_request_receipts(id) ON DELETE CASCADE,
              attempt_id TEXT NOT NULL UNIQUE, input_credential_generation TEXT NOT NULL,
              output_credential_generation TEXT) STRICT;",
            "CREATE TABLE fleet_refresh_input_fences (
              input_credential_generation TEXT PRIMARY KEY NOT NULL COLLATE NOCASE) STRICT, WITHOUT ROWID;",
        ] {
            db.execute_batch(ddl).expect("create a predecessor table");
        }
        db.execute(
            "UPDATE credential_format_state SET owner_api_state=?1, fleet_api_state=?2,
             owner_api_blocked=NULL, fleet_api_blocked=NULL, fences_pending=?3,
             old_tables_dropped=0, purge_required=1, wal_scrubbed=0, old_files_removed=0,
             attempts_migrated=0, fences_fingerprinted=0, legacy_fences=0,
             purge_needed_bytes=NULL, secrets_exclusion=NULL, durability=NULL,
             durability_probed_at_ms=NULL, started_at_ms=NULL, converted_at_ms=NULL,
             completed_at_ms=NULL WHERE singleton_id=1",
            rusqlite::params![
                if owner.is_some() { "pending" } else { "none" },
                if fleet.is_some() { "pending" } else { "none" },
                fences_pending
            ],
        )
        .expect("rewrite the state row");
        if let Some(row) = owner {
            db.execute(
                "INSERT INTO legacy_owner_tokens(singleton_id, access, refresh, expires_at,
                 next_refresh_at, credential_generation) VALUES(1, ?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    row.access,
                    row.refresh,
                    row.expires_at,
                    row.next_refresh_at,
                    row.generation.map(|g| g.to_string())
                ],
            )
            .expect("insert the Legacy row");
        }
        if let Some(row) = fleet {
            db.execute(
                "INSERT INTO fleet_tokens(singleton_id, access, refresh, client_id, region,
                 expires_at, next_refresh_at, credential_generation)
                 VALUES(1, ?1, ?2, 'client-0001', 'eu', ?3, ?4, ?5)",
                rusqlite::params![
                    row.access,
                    row.refresh,
                    row.expires_at,
                    row.next_refresh_at,
                    row.generation.map(|g| g.to_string())
                ],
            )
            .expect("insert the Fleet row");
        }
    }

    pub(crate) fn payload_stage(stage: &str, payload: &Path) -> PayloadAction {
        let hook = with(|state| state.payload_hook.take());
        let Some(mut hook) = hook else {
            return PayloadAction::Continue;
        };
        let action = hook(stage, payload);
        with(|state| {
            if matches!(action, PayloadAction::Crash) {
                state.crashed = true;
            }
            if state.payload_hook.is_none() {
                state.payload_hook = Some(hook);
            }
        });
        action
    }

    /// Whether the last payload stage asked for a crash (nothing cleaned up).
    pub(crate) fn take_crash() -> bool {
        with(|state| std::mem::take(&mut state.crashed))
    }
}

#[cfg(test)]
#[path = "credential_migration/tests.rs"]
mod g24_m2_m;
