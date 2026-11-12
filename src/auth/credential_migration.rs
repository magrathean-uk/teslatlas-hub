// SPDX-License-Identifier: AGPL-3.0-only

//! Phase B of the credential format migration (C10 R43 to R49, R75, R80).
//!
//! Phase B runs in an admitted process, under the data-directory lock, on the
//! once-taken credential connection. Source credentials are disabled
//! (M02-T02): it refuses every pending pre-upgrade token row without reading a
//! key or decrypting it, then maps fences and attempts, purges residue, scrubs
//! the WAL, removes the neutral key files and records completion. Every step is
//! idempotent and re-runnable at the next admitted open.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use rustix::fs::statvfs;
use uuid::Uuid;

use crate::{
    auth::{
        credential_envelope::{
            CredentialSlot, EnvelopeError,
        },
        credential_keys::{
            BackupExclusion, CredentialKeyError, ExclusionState, KeyringHandle, create_keyring,
            load_keyring,
        },
    },
    db::{
        CredentialConnection, CredentialFormatState, CredentialMigrationAttempt,
        CredentialMigrationBlockReason, CredentialMigrationLegacyFence, CredentialMigrationPlan,
        CredentialMigrationRefreshFence, CredentialSecretsExclusion, CredentialSlotFormatState,
        LegacyFenceIdentity, PreUpgradeRefreshBinding, SealedCredentialPair, StoreError,
    },
    hub_user_process::AdmittedUserHub,
    platform::private_directory::{EntryState, PrivateDirectory},
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
/// R49's removal list.
const PRE_UPGRADE_FILES: [&str; 7] = [REL_K1, REL_K2, RST_K1, RST_K2, OLD_K4, OLD_K5, OLD_K6];
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
    pub(crate) exclusion: &'a dyn BackupExclusion,
    pub(crate) now_ms: i64,
}

/// The migration receipt of one run.
#[derive(Debug)]
#[cfg_attr(not(test), expect(dead_code, reason = "read by the test build only"))]
pub(crate) struct CredentialMigrationReport {
    pub(crate) state: CredentialFormatState,
    #[allow(dead_code, reason = "receipt field, not read yet")]
    pub(crate) unsafe_candidates: Vec<&'static str>,
    #[allow(dead_code, reason = "receipt field, not read yet")]
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

/// Source credentials are disabled (M02-T02): a pending slot's pre-upgrade
/// pair is source material. It is refused here, before any candidate key file
/// is inspected or read, before any decryption and before anything is sealed
/// or stored: the slot is recorded blocked, its old row stays sealed under the
/// old key until the Hub's own login replaces it or the slot is cleared, and
/// the load reports `source_credential_disabled`.
fn refuse_pending_slot(slot: CredentialSlot) -> (CredentialSlot, CredentialMigrationBlockReason) {
    note_event(&format!("refused:{}", slot.as_str()));
    (slot, CredentialMigrationBlockReason::OldPairInvalid)
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
            // Steps 3 and 4: every pending slot is refused source material;
            // no key is read and nothing is decrypted or converted.
            for slot in pending_slots {
                parts.blocked.push(refuse_pending_slot(slot));
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
        /// Retired with pre-upgrade decryption (M02-T02); older fixtures
        /// still set them, and nothing reads them.
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
pub(in crate::auth) mod g24_m2_m;
