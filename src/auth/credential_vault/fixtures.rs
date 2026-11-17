// SPDX-License-Identifier: AGPL-3.0-only
//! Previous-process producers only. No live vault connection is used to seed.
use super::*;

#[cfg(test)]
thread_local! {
    static PROBE_OBSERVER: std::cell::RefCell<Option<Box<dyn FnMut(&str)>>> = const { std::cell::RefCell::new(None) };
    static POWER_OBSERVER: std::cell::RefCell<Option<Box<dyn FnMut(&str, Option<&Path>)>>> = const { std::cell::RefCell::new(None) };
    static SEAL_OBSERVER: std::cell::RefCell<Option<Box<dyn FnMut(super::super::credential_envelope::KeyId)>>> = const { std::cell::RefCell::new(None) };
    static DIRECTORY_OPEN_FAILURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) fn directory_open_failure(error: &private_directory::PrivateDirectoryError) {
    DIRECTORY_OPEN_FAILURE.with(|slot| *slot.borrow_mut() = Some(format!("{error:?}")));
}
#[cfg(test)]
pub(crate) fn probe_event(point:&str){PROBE_OBSERVER.with(|s|{if let Some(callback)=s.borrow_mut().as_mut(){callback(point);}});}
#[cfg(test)]
pub(in crate::auth) fn probe_observer(callback:Option<Box<dyn FnMut(&str)>>){PROBE_OBSERVER.with(|s|*s.borrow_mut()=callback);}
#[cfg(test)]
pub(crate) fn power_seal(id: super::super::credential_envelope::KeyId) {
    SEAL_OBSERVER.with(|observer| { if let Some(observer)=observer.borrow_mut().as_mut(){observer(id);} });
    power_event("seal",None);
}
#[cfg(test)]
pub(in crate::auth) fn power_seal_observer(observer: Option<Box<dyn FnMut(super::super::credential_envelope::KeyId)>>) {
    SEAL_OBSERVER.with(|slot| *slot.borrow_mut()=observer);
}
/// Test file-system layer: only passed barriers advance its durable image.
#[cfg(test)]
pub(crate) fn power_event(kind: &str, path: Option<&Path>) {
    POWER_OBSERVER.with(|observer| { if let Some(observer) = observer.borrow_mut().as_mut() { observer(kind, path); } });
}
#[cfg(test)]
pub(in crate::auth) fn power_observer(observer: Option<Box<dyn FnMut(&str, Option<&Path>)>>) {
    POWER_OBSERVER.with(|slot| *slot.borrow_mut() = observer);
}

#[cfg(test)]
pub(crate) struct FixtureExclusion;
/// The outcome of the fixture exclusion's next check of one secrets directory, in
/// place of the immediate answer: a test stands in for the wait of tmutil there
/// (M04-T08). Keyed by the directory, so parallel tests do not see each other's.
#[cfg(test)]
pub(crate) type ExclusionWait = Box<dyn FnOnce() -> Result<ExclusionState, CredentialKeyError> + Send>;
#[cfg(test)]
static EXCLUSION_WAITS: Mutex<std::collections::BTreeMap<std::path::PathBuf, ExclusionWait>> = Mutex::new(std::collections::BTreeMap::new());
#[cfg(test)]
pub(crate) fn wait_in_next_exclusion(secrets: &Path, wait: ExclusionWait) {
    EXCLUSION_WAITS.lock().expect("exclusion waits").insert(secrets.to_owned(), wait);
}
/// The wait of the next durability sync of `path`, in place of the sync's own
/// (M04-T09): the catalogue path for the credential connection's barrier, the
/// `secrets/` path for the keyring proof at open. The closure stands in for the
/// wait and returns whether the sync then fails. Keyed by the path, so parallel
/// tests do not see each other's.
#[cfg(test)]
pub(crate) type SyncWait = Box<dyn FnOnce() -> bool + Send>;
#[cfg(test)]
static SYNC_WAITS: Mutex<std::collections::BTreeMap<std::path::PathBuf, SyncWait>> = Mutex::new(std::collections::BTreeMap::new());
#[cfg(test)]
pub(crate) fn wait_in_next_sync(path: &Path, wait: SyncWait) {
    SYNC_WAITS.lock().expect("sync waits").insert(path.to_owned(), wait);
}
/// A key rotation a previous process published and did not finish (M04-T09): the
/// next opening resumes it.
#[cfg(test)]
pub(crate) fn begin_fixture_rotation(data: &Path) {
    let secrets = private_directory::open_validated(&data.join("secrets"), false).expect("secrets");
    let keys = credential_keys::load_keyring(&secrets).expect("keyring").expect("present");
    credential_keys::begin_rotation(&secrets, &FixtureExclusion, &keys, 1_800_086_400).expect("rotation publication");
}
/// Runs `control` once on this thread, just before the next resumed rotation's
/// reseal transaction (M04-T09).
#[cfg(test)]
pub(crate) fn before_next_reseal(control: Box<dyn FnOnce()>) {
    RESEAL_CONTROL.with(|slot| *slot.borrow_mut() = Some(Box::new(move |_| control())));
}
/// Runs the registered wait of `path`, if any: `true` when the sync then fails.
#[cfg(test)]
pub(crate) fn sync_wait(path: &Path) -> bool {
    let wait = SYNC_WAITS.lock().expect("sync waits").remove(path);
    wait.is_some_and(|wait| wait())
}
#[cfg(test)]
pub(crate) fn replace_legacy_supplied_pair(store: &HubStore, data: &Path, access: &str, refresh: &str, schedule: Option<(i64,i64)>) -> Result<Uuid,CredentialStoreError> {
    let mut entry = CredentialEntry::own_login_fixture(Zeroizing::new(access.into()),Zeroizing::new(refresh.into()))?;
    let (expires,next) = schedule.unwrap_or((0,0)); entry.set_schedule(expires,next);
    let registered = store.fixture_vault.current.lock().expect("fixture context");
    let active = registered.upgrade();
    match active {
        Some(vault) => vault.store_supplied_entry(entry,ReplacePolicy::ReplaceAny),
        None => { wait_for_release(store); Ok(seed_entry(data,entry)) },
    }
}
#[cfg(test)]
pub(crate) fn legacy_vault(store: &HubStore, data: &Path) -> Arc<CredentialVault> { open_fixture_vault(store.clone(),data) }
#[cfg(test)]
impl BackupExclusion for FixtureExclusion {
    fn ensure_excluded(&self, secrets: &Path) -> Result<ExclusionState, CredentialKeyError> {
        let wait = EXCLUSION_WAITS.lock().expect("exclusion waits").remove(secrets);
        if let Some(wait) = wait { return wait(); }
        #[cfg(target_os = "macos")] { Ok(ExclusionState::Verified) }
        #[cfg(not(target_os = "macos"))] { Ok(ExclusionState::NotApplicable) }
    }
}
#[cfg(test)]
/// Seeds an Owner API pair as the Hub's own login would store it: since
/// M02-T02 only that origin is admitted at load.
pub(crate) fn seed_legacy_supplied_pair(data: &Path, access: &str, refresh: &str, schedule: Option<(i64, i64)>) -> Uuid {
    let mut entry = CredentialEntry::own_login_fixture(Zeroizing::new(access.to_owned()), Zeroizing::new(refresh.to_owned())).expect("fixture entry");
    let (expires,next) = schedule.unwrap_or((0,0)); entry.set_schedule(expires,next);
    seed_entry(data, entry)
}
#[cfg(test)]
/// Seeds the Hub's own Fleet grant for the refresh fence tests outside
/// `crate::auth` (M09-T08: a supplied Fleet pair is blocked, never refreshed).
pub(crate) fn seed_own_fleet_pair(data: &Path, access: &str, refresh: &str) -> Uuid {
    let vault = open_fixture_vault(HubStore::initialize(data).expect("fixture store"), data);
    seed_own_fleet_grant(&vault, access, refresh).expect("own Fleet grant")
}
/// Stores a Fleet pair as the Hub's own Fleet sign-in does (M09-T08): the
/// only writer of an admitted Fleet pair besides its refresh successor.
#[cfg(debug_assertions)]
pub(crate) fn seed_own_fleet_grant(vault: &CredentialVault, access: &str, refresh: &str) -> Result<Uuid, CredentialStoreError> {
    seed_own_fleet_grant_as(vault, access, refresh, "client-0001", crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800, None)
}
/// [`seed_own_fleet_grant`] with the application, region and lifetime of the
/// grant, sent at `sent_at_s` (the sign-in's own time when `None`), so the
/// stored schedule is `sent_at_s + expires_in` as for a real exchange.
#[cfg(debug_assertions)]
pub(crate) fn seed_own_fleet_grant_as(vault: &CredentialVault, access: &str, refresh: &str, client_id: &str, region: crate::fleet_api::FleetRegion,
    expires_in: u64, sent_at_s: Option<i64>) -> Result<Uuid, CredentialStoreError> {
    let session = vault.begin_sign_in(CredentialSlot::FleetApi)?;
    let sent_at_s = sent_at_s.unwrap_or((session.expires_at_ms - 600_000) / 1000);
    let grant = RedeemedGrant::from_validated_redemption_at(session, Zeroizing::new(access.to_owned()), Zeroizing::new(refresh.to_owned()), expires_in,
        Some(client_id.to_owned()), Some(region), sent_at_s)?;
    vault.store_hub_grant(grant)?;
    vault.stored_generation(CredentialSlot::FleetApi)?.ok_or(CredentialStoreError::SlotOccupied)
}
/// Stores Fleet set-up credentials as the Hub's own Fleet grant sent at `now`
/// (M09-T11): the same tokens, application, region and schedule the retired
/// supplied path wrote, through the only admitted Fleet writer.
#[cfg(debug_assertions)]
pub(crate) fn seed_own_fleet_credentials(vault: &CredentialVault, credentials: &crate::fleet_credentials::FleetSetupCredentials, now: SystemTime) -> Result<Uuid, CredentialStoreError> {
    let entry = credentials.credential_entry(now).map_err(|_| CredentialStoreError::InvalidOrigin)?;
    let sent_at_s = i64::try_from(now.duration_since(UNIX_EPOCH).map_err(|_| CredentialStoreError::InvalidOrigin)?.as_secs()).map_err(|_| CredentialStoreError::InvalidOrigin)?;
    let expires_in = u64::try_from(entry.expires_at - sent_at_s).map_err(|_| CredentialStoreError::InvalidOrigin)?;
    let region = crate::fleet_api::FleetRegion::from_storage_code(entry.region.as_deref().unwrap_or_default()).map_err(|_| CredentialStoreError::InvalidOrigin)?;
    seed_own_fleet_grant_as(vault, &entry.access, &entry.refresh, entry.client_id.as_deref().unwrap_or_default(), region, expires_in, Some(sent_at_s))
}
#[cfg(test)]
pub(crate) fn seed_entry(data: &Path, entry: CredentialEntry) -> Uuid {
    let store = HubStore::initialize(data).expect("fixture store");
    let mut connection = store.take_credential_connection().expect("fixture connection");
    connection.probe_credential_durability().expect("fixture probe");
    let secrets = private_directory::open_validated(&data.join("secrets"), true).expect("fixture directory");
    let keys = credential_keys::load_keyring(&secrets).expect("fixture key inspection")
        .unwrap_or_else(|| credential_keys::create_keyring(&secrets, &FixtureExclusion, 1_800_000_000).expect("fixture key publication"));
    let generation = Uuid::new_v4(); let mut row = entry.row(generation);
    connection.reserve_credential_invocations(keys.keyring().current.id, 2).expect("fixture reservation");
    row.seal(&keys, entry.access.as_bytes(), entry.refresh.as_bytes(), &mut OsNonces).expect("fixture seal");
    // A `source_token` row is written only by the takeover (DEC-172), and a
    // Fleet row only by the Hub's own grant (M09-T08); the refused rows an
    // older Hub could hold are seeded past the write gate.
    if entry.origin == CredentialOrigin::SourceToken || entry.slot == CredentialSlot::FleetApi {
        connection.test_insert_credential_vector(&row).expect("fixture refused pair");
        connection.credential_durability_barrier().expect("fixture pair barrier");
        return generation;
    }
    let binding = (entry.origin == CredentialOrigin::HubLogin).then(|| {
        let secret = own_login_grant_secret().expect("fixture grant secret");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("fixture clock").as_millis() as i64;
        (connection.insert_sign_in_binding(entry.slot, own_login_binding(&secret), now).expect("fixture binding"), secret)
    });
    let provenance = match &binding {
        Some((sign_in_id, grant_secret)) => EntryProvenance::HubGrant { sign_in_id: *sign_in_id, grant_secret },
        None => EntryProvenance::Supplied,
    };
    connection.replace_credential_pair(&row, &provenance, &entry.legacy, ReplacePolicy::ReplaceAny).expect("fixture pair");
    connection.credential_durability_barrier().expect("fixture pair barrier");
    generation
}

/// CT19's expressly admitted canonical vector exception to Supplied seeding.
#[cfg(test)]
pub(in crate::auth) fn seed_rotation_vector(data: &Path) -> Uuid {
    use super::super::credential_envelope::{FixedNonces, KeyId};
    let store = HubStore::initialize(data).expect("vector store");
    let mut connection = store.take_credential_connection().expect("previous process connection");
    connection.set_test_now_ms(1_800_000_000_000);
    let directory = private_directory::open_validated(&data.join("secrets"), true).expect("vector directory");
    let id = KeyId([1,2,3,4,5,6,7,8]);
    let keys = KeyringHandle::new(credential_keys::Keyring { current: credential_keys::KeyMaterial {
        id, created_at_s: 1_800_000_000, key: Zeroizing::new(std::array::from_fn(|i| i as u8)),
    }, previous: None });
    use std::os::unix::fs::PermissionsExt;
    let path = directory.path().join("credential-keys");
    std::fs::write(&path, credential_keys::encode_keyring(keys.keyring())).expect("canonical ring");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("ring mode");
    let generation = Uuid::from_u128(0x40008000000000000001);
    let entry = CredentialEntry::direct_entry(Zeroizing::new("access-0001".into()), Zeroizing::new("refresh-0001".into())).expect("vector entry");
    let mut row = entry.row(generation); row.origin = Some(CredentialOrigin::HubLogin);
    let mut nonces = FixedNonces::new(id, [std::array::from_fn(|i| i as u8), std::array::from_fn(|i| i as u8 + 0x10)]);
    connection.reserve_credential_invocations(id, 2).expect("vector reservation");
    row.seal(&keys, entry.access.as_bytes(), entry.refresh.as_bytes(), &mut nonces).expect("canonical seal");
    connection.test_insert_credential_vector(&row).expect("canonical vector insert");
    connection.begin_credential_refresh(OWNER_SLOT, generation).expect("attempt and fence fixture");
    connection.credential_durability_barrier().expect("fixture proof");
    generation
}
#[cfg(test)]
#[cfg(test)]
const OWNER_SLOT: CredentialSlot = CredentialSlot::OwnerApi;
#[cfg(test)]
/// Seeds Fleet set-up credentials through the Hub's own grant (M09-T11); a
/// supplied Fleet row is refused at the write gate and blocked at load.
pub(crate) fn seed_fleet_setup_credentials(store: &HubStore, data: &Path, credentials: &crate::fleet_credentials::FleetSetupCredentials, now: SystemTime) -> Result<(), crate::fleet_credentials::FleetCredentialError> {
    credentials.credential_entry(now)?;
    let vault = open_fixture_vault(store.clone(), data);
    seed_own_fleet_credentials(&vault, credentials, now).expect("own Fleet grant");
    Ok(())
}
/// The supplied Fleet row an older Hub could hold, written past the write
/// gate: the un-park barrier blocks it and the load refuses it
/// `source_credential_disabled` (M09-T08).
#[cfg(test)]
pub(crate) fn seed_blocked_fleet_setup_credentials(data: &Path, credentials: &crate::fleet_credentials::FleetSetupCredentials, now: SystemTime) -> Uuid {
    seed_entry(data, credentials.credential_entry(now).expect("fixture entry"))
}
#[cfg(test)]
pub(crate) fn validate_fixture_fleet_credentials(store: &HubStore, data: &Path) -> Result<(), crate::fleet_credentials::FleetCredentialError> {
    let vault = open_fixture_vault(store.clone(), data);
    crate::fleet_credentials::validate_stored_fleet_credentials(&crate::CredentialVaultEntry::for_test(vault), data)
}
#[cfg(test)]
pub(crate) fn open_fixture_vault(store: HubStore, data: &Path) -> Arc<CredentialVault> {
    let registry = Arc::clone(&store.fixture_vault);
    let mut registered = registry.current.lock().expect("fixture context");
    if let Some(vault) = registered.upgrade() { return vault; }
    wait_for_release(&store);
    let admission = fixture_admission_locked(&store, data);
    DIRECTORY_OPEN_FAILURE.with(|slot| slot.borrow_mut().take());
    let before = (store.test_credential_connection_in_use(), admission.test_credential_vault_in_use());
    let vault = CredentialVault::open_unregistered(store.clone(), Arc::clone(&admission), data, VaultOptions { exclusion: Some(Arc::new(FixtureExclusion)), ..VaultOptions::default() })
        .unwrap_or_else(|error| {
            let (stage, sqlite) = match &error {
                CredentialStoreError::Store(StoreError::CredentialConnectionTaken) => ("connection_taken", None),
                CredentialStoreError::Store(StoreError::Open(error)) => ("sqlite_open", error.sqlite_error().map(|error| error.extended_code)),
                CredentialStoreError::Store(StoreError::Configure(error)) => ("sqlite_configure", error.sqlite_error().map(|error| error.extended_code)),
                CredentialStoreError::Store(StoreError::CredentialPairStore(error)) => ("sqlite_pair", error.sqlite_error().map(|error| error.extended_code)),
                CredentialStoreError::Key(CredentialKeyError::Io { operation }) => (*operation, None),
                _ => (error.code(), None),
            };
            let store_variant = match &error { CredentialStoreError::Store(error) => Some(std::mem::discriminant(error)), _ => None };
            let directory_failure = DIRECTORY_OPEN_FAILURE.with(|slot| slot.borrow_mut().take());
            panic!("fixture vault: {error:?}; stage={stage} sqlite={sqlite:?} store_variant={store_variant:?} directory_failure={directory_failure:?} connection_before={} admission_before={} connection_after={} admission_after={} admission_owners={} sensitive_access={}",
                before.0, before.1, store.test_credential_connection_in_use(), admission.test_credential_vault_in_use(), Arc::strong_count(&admission), admission.assert_sensitive_access().is_ok());
        });
    assert_ne!(registered.as_ptr(), Arc::as_ptr(&vault), "fixture restart constructs a new vault");
    *registered = Arc::downgrade(&vault);
    vault
}
#[cfg(test)]
pub(crate) fn open_admitted_fixture_vault(store: HubStore, data: &Path, admission: Arc<AdmittedUserHub>) -> Result<Arc<CredentialVault>, CredentialStoreError> {
    open_admitted_fixture_vault_inner(store, data, admission, true, None)
}
#[cfg(test)]
pub(crate) fn open_runtime_fixture_vault(store: HubStore, data: &Path, admission: Arc<AdmittedUserHub>, fence: Option<&crate::credentials::LossObserver>) -> Result<Arc<CredentialVault>, CredentialStoreError> {
    open_admitted_fixture_vault_inner(store, data, admission, false, fence)
}
#[cfg(test)]
fn open_admitted_fixture_vault_inner(store: HubStore, data: &Path, admission: Arc<AdmittedUserHub>, reuse: bool, fence: Option<&crate::credentials::LossObserver>) -> Result<Arc<CredentialVault>, CredentialStoreError> {
    let registry = Arc::clone(&store.fixture_vault);
    let mut registered = registry.current.lock().map_err(|_|CredentialStoreError::LockPoisoned)?;
    if let Some(vault) = registered.upgrade() {
        if !reuse { return Err(StoreError::CredentialConnectionTaken.into()); }
        if !Arc::ptr_eq(&vault.admission, &admission) { return Err(CredentialStoreError::SensitiveAccessUnavailable); }
        return Ok(vault);
    }
    wait_for_release(&store);
    let mut retained = registry.admission.lock().map_err(|_|CredentialStoreError::LockPoisoned)?;
    if retained.as_ref().is_some_and(|retained| !Arc::ptr_eq(retained, &admission)) {
        return Err(CredentialStoreError::SensitiveAccessUnavailable);
    }
    let vault = CredentialVault::open_unregistered_fenced(store, admission, data, VaultOptions { exclusion: Some(Arc::new(FixtureExclusion)), ..VaultOptions::default() }, fence)?;
    assert_ne!(registered.as_ptr(), Arc::as_ptr(&vault), "fixture restart constructs a new vault");
    *retained = Some(Arc::clone(&vault.admission));
    *registered = Arc::downgrade(&vault);
    Ok(vault)
}
#[cfg(test)]
pub(crate) fn fixture_admission(store: &HubStore, data: &Path) -> Arc<AdmittedUserHub> {
    let registered = store.fixture_vault.current.lock().expect("fixture context");
    match registered.upgrade() {
        Some(vault) => vault.test_admission(),
        None => {
            wait_for_release(store);
            fixture_admission_locked(store, data)
        },
    }
}
#[cfg(test)]
fn fixture_admission_locked(store: &HubStore, data: &Path) -> Arc<AdmittedUserHub> {
    let mut registered = store.fixture_vault.admission.lock().expect("fixture admission");
    if let Some(admission) = registered.as_ref() { return Arc::clone(admission); }
    let admission = AdmittedUserHub::admit(data).expect("fixture admission");
    *registered = Some(Arc::clone(&admission));
    admission
}
/// A revoked fixture process can restart only after all of its holders finish.
/// Consume its sole store context and close its admission synchronously before
/// opening a fresh context. Ordinary fixture opens retain their admission.
#[cfg(test)]
pub(crate) fn restart_after_revocation(store: HubStore, data: &Path) -> Result<HubStore, &'static str> {
    restart_context(store, data, true)
}
#[cfg(test)]
pub(crate) fn restart_fixture_context(store: HubStore, data: &Path) -> Result<HubStore, &'static str> {
    restart_context(store, data, false)
}
#[cfg(test)]
fn restart_context(store: HubStore, data: &Path, require_revocation: bool) -> Result<HubStore, &'static str> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let registered = loop {
        let registered = store.fixture_vault.current.lock().map_err(|_| "fixture_restart_context_poisoned")?;
        if registered.upgrade().is_none()
            && !store.test_credential_connection_in_use()
            && Arc::strong_count(&store.fixture_vault) == 1
        { break registered; }
        drop(registered);
        if std::time::Instant::now() >= deadline { return Err("fixture_restart_holders_timeout"); }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let mut retained = store.fixture_vault.admission.lock().map_err(|_| "fixture_restart_admission_poisoned")?;
    let mut admission = retained.take().ok_or("fixture_restart_admission_missing")?;
    if require_revocation && admission.assert_sensitive_access().is_ok() {
        *retained = Some(admission);
        return Err("fixture_restart_requires_revocation");
    }
    drop(registered);
    loop {
        match Arc::try_unwrap(admission) {
            Ok(admission) => {
                // Its destructor closes the retained descriptor before returning.
                drop(admission);
                break;
            }
            Err(shared) => {
                admission = shared;
                if std::time::Instant::now() >= deadline {
                    *retained = Some(admission);
                    return Err("fixture_restart_descriptor_timeout");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    drop(retained);
    drop(store);
    HubStore::initialize(data).map_err(|_| "fixture_restart_store_unavailable")
}
#[cfg(test)]
fn wait_for_release(store: &HubStore) {
    let context = &store.fixture_vault;
    let mut release = context.release.lock().expect("fixture release");
    let (guard, timeout) = context.released.wait_timeout_while(
        release, std::time::Duration::from_secs(30), |_| store.test_credential_connection_in_use(),
    ).expect("fixture release");
    release = guard;
    assert!(!timeout.timed_out() || !store.test_credential_connection_in_use(), "fixture_connection_release_timeout");
    drop(release);
}

#[cfg(debug_assertions)]
pub(super) fn seed_pending_for_binary(data: &Path, candidate: u8, fleet: bool, missing_keyring: bool) -> Result<Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    if missing_keyring {
        let store = HubStore::initialize(data)?;
        let admission = AdmittedUserHub::admit(data)?;
        let entry = entry::CredentialVaultEntry::open_for_test(store, admission, data, true)
            .map_err(|error| std::io::Error::other(error.code()))?;
        seed_own_fleet_grant(entry.vault(), "access-0099", "refresh-0099")
            .map_err(super::super::credential_sign_in::CredentialCommandError::from)?;
        drop(entry);
        std::fs::rename(data.join("secrets").join("credential-keys"), data.join("saved-ring"))?;
    }
    let store = HubStore::initialize(data)?;
    let db = store.open()?;
    for ddl in [
        "CREATE TABLE legacy_owner_tokens(singleton_id INTEGER PRIMARY KEY, access BLOB NOT NULL, refresh BLOB NOT NULL, expires_at INTEGER NOT NULL, next_refresh_at INTEGER NOT NULL, credential_generation TEXT) STRICT;",
        "CREATE TABLE fleet_tokens(singleton_id INTEGER PRIMARY KEY, access BLOB NOT NULL, refresh BLOB NOT NULL, client_id TEXT NOT NULL, region TEXT NOT NULL, expires_at INTEGER NOT NULL, next_refresh_at INTEGER NOT NULL, credential_generation TEXT) STRICT;",
        "CREATE TABLE legacy_refresh_receipt_bindings(receipt_id INTEGER PRIMARY KEY REFERENCES outbound_request_receipts(id), attempt_id TEXT NOT NULL UNIQUE, input_credential_generation TEXT NOT NULL, output_credential_generation TEXT) STRICT;",
        "CREATE TABLE legacy_refresh_input_fences(input_credential_generation TEXT PRIMARY KEY NOT NULL COLLATE NOCASE) STRICT, WITHOUT ROWID;",
        "CREATE TABLE fleet_refresh_receipt_bindings(receipt_id INTEGER PRIMARY KEY REFERENCES outbound_request_receipts(id), attempt_id TEXT NOT NULL UNIQUE, input_credential_generation TEXT NOT NULL, output_credential_generation TEXT) STRICT;",
        "CREATE TABLE fleet_refresh_input_fences(input_credential_generation TEXT PRIMARY KEY NOT NULL COLLATE NOCASE) STRICT, WITHOUT ROWID;",
    ] { db.execute_batch(ddl)?; }
    let vectors: serde_json::Value = serde_json::from_slice(&std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/credential-pre-upgrade-vectors.json"))?)?;
    let case = |id: &str| vectors["ciphertext_cases"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap();
    let unhex = |text: &str| -> Vec<u8> { (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i+2], 16).unwrap()).collect() };
    let owner = case("legacy_32");
    let key = Zeroizing::new(unhex(owner["key_hex"].as_str().unwrap()));
    db.execute("INSERT INTO legacy_owner_tokens VALUES(1,?1,?2,1800028800,1800021600,?3)",
        rusqlite::params![unhex(owner["access_ciphertext_hex"].as_str().unwrap()), unhex(owner["refresh_ciphertext_hex"].as_str().unwrap()), owner["generation"].as_str().unwrap()])?;
    db.execute("INSERT INTO legacy_refresh_input_fences(input_credential_generation) VALUES(?1)", [owner["generation"].as_str().unwrap()])?;
    db.execute("UPDATE credential_format_state SET owner_api_state='pending', fleet_api_state=?1, owner_api_blocked=NULL, fleet_api_blocked=NULL, fences_pending=1, old_tables_dropped=0, purge_required=1, wal_scrubbed=0, old_files_removed=0, converted_at_ms=NULL, completed_at_ms=NULL, secrets_exclusion=NULL, durability=NULL WHERE singleton_id=1",
        [if fleet { "pending" } else { "none" }])?;
    let secrets = private_directory::open_validated(&data.join("secrets"), true)?;
    let write = |name: &str, bytes: &[u8]| -> Result<(), Box<dyn std::error::Error>> {
        let path = secrets.path().join(name);
        std::fs::write(&path, bytes)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    };
    match candidate {
        0 => (),
        1 => write("pre-upgrade-owner-api.relocated.key", &key)?,
        2 => {
            let invalid: serde_json::Value = serde_json::from_slice(&std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/credential-pre-upgrade-invalid-pair.json"),
            )?)?;
            let row = invalid["ciphertext_cases"].as_array().unwrap().iter()
                .find(|row| row["id"] == "legacy_crlf_refresh").unwrap();
            db.execute("INSERT OR REPLACE INTO legacy_owner_tokens VALUES(1,?1,?2,1800028800,1800021600,?3)",
                rusqlite::params![unhex(row["access_ciphertext_hex"].as_str().unwrap()), unhex(row["refresh_ciphertext_hex"].as_str().unwrap()), row["generation"].as_str().unwrap()])?;
            db.execute("DELETE FROM legacy_refresh_input_fences", [])?;
            db.execute("INSERT INTO legacy_refresh_input_fences(input_credential_generation) VALUES(?1)",
                [row["generation"].as_str().unwrap()])?;
            let key = Zeroizing::new(unhex(row["key_hex"].as_str().unwrap()));
            write("pre-upgrade-owner-api.relocated.key", &key)?;
        },
        _ => return Err(std::io::Error::other("fixture_candidate_invalid").into()),
    }
    if fleet {
        let row = case("fleet_file");
        db.execute("INSERT INTO fleet_tokens VALUES(1,?1,?2,'client-0001','eu',1800028800,1800021600,?3)",
            rusqlite::params![unhex(row["access_ciphertext_hex"].as_str().unwrap()), unhex(row["refresh_ciphertext_hex"].as_str().unwrap()), row["generation"].as_str().unwrap()])?;
        write("fleet-credentials.key", &unhex(row["key_hex"].as_str().unwrap()))?;
    }
    Ok(key)
}
