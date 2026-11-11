// SPDX-License-Identifier: AGPL-3.0-only

//! Part M component tests (spec section 10: CT45 to CT62, CT87, CT90, CT91,
//! CT93 and CT101). Synthetic data only; every store, directory and key file
//! lives in a per-test private temporary directory.

use super::*;
use crate::auth::credential_envelope::{CredentialSlot, OsNonces, refresh_fingerprint};
use crate::auth::credential_keys::{
    BackupExclusion, CredentialKeyError, ExclusionState, KeyringHandle, check_keyring,
};
use crate::db::{
    BeginRefusal, CredentialFormatState, CredentialMigrationBlockReason,
    CredentialSecretsExclusion, CredentialSlotFormatState, HubStore, StoreError,
};
use crate::hub_user_process::AdmittedUserHub;
use crate::platform::private_directory::{PrivateDirectory, open_validated};
use rusqlite::{Connection, params};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

const T0_MS: i64 = 1_800_000_000_000;
const SCHEDULE: (i64, i64) = (1_800_028_800, 1_800_021_600);
const RUNG: i32 = 79;

// Neutral candidate and cleanup names, spelled out here as an oracle that is
// independent of the production lists (spec sections 0.2 and 5).
const REL_K1: &str = "pre-upgrade-owner-api.relocated.key";
const REL_K2: &str = "pre-upgrade-owner-api.relocated-previous.key";
const RST_K1: &str = "pre-upgrade-owner-api.key";
const RST_K2: &str = "pre-upgrade-fleet-api.key";
const FLEET_K4: &str = "fleet-credentials.key";
const FLEET_K5: &str = ".fleet-credentials.pending.key";
const FLEET_K6: &str = ".fleet-credentials-key-migration";
const NEUTRAL_FILES: [&str; 7] = [REL_K1, REL_K2, RST_K1, RST_K2, FLEET_K4, FLEET_K5, FLEET_K6];

fn uuid_n(n: u128) -> Uuid {
    Uuid::from_u128(0x00000000_0000_4000_8000_000000000000 | n)
}

/// Stand-ins for the old names of spec section 0.2, built at run time from the
/// accepted N1 helper so that no source line spells one (gap GAP-OLDNAMES: the
/// exact old file names are not in any permitted input).
fn stand_in_name(suffix: &str) -> String {
    format!(
        "{}-{suffix}",
        crate::runtime::name_test_support::forbidden_name()
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// `FakePreUpgrade` ciphertext (spec section 10): opens only with `key`.
fn fake_ct(key: &[u8], plain: &[u8]) -> Vec<u8> {
    let mut out = b"OLD:".to_vec();
    out.extend_from_slice(hex(key).as_bytes());
    out.push(b':');
    out.extend_from_slice(plain);
    out
}

fn key32(seed: u8) -> Vec<u8> {
    (0..32).map(|i| seed.wrapping_add(i)).collect()
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

struct FakeExclusion(Result<ExclusionState, CredentialKeyError>);

impl BackupExclusion for FakeExclusion {
    fn ensure_excluded(&self, _: &Path) -> Result<ExclusionState, CredentialKeyError> {
        self.0
    }
}

#[derive(Clone)]
struct PriorRow {
    access: Vec<u8>,
    refresh: Vec<u8>,
    expires: i64,
    next: i64,
    generation: Option<Uuid>,
}

impl PriorRow {
    /// A row sealed under `key` with the fake pre-upgrade format.
    fn fake(key: &[u8], access: &str, refresh: &str, generation: Option<Uuid>) -> Self {
        Self {
            access: fake_ct(key, access.as_bytes()),
            refresh: fake_ct(key, refresh.as_bytes()),
            expires: SCHEDULE.0,
            next: SCHEDULE.1,
            generation,
        }
    }
}

const PREDECESSOR_DDL: [&str; 6] = [
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
      output_credential_generation TEXT) STRICT;
     CREATE UNIQUE INDEX legacy_refresh_receipt_bindings_output
      ON legacy_refresh_receipt_bindings(output_credential_generation)
      WHERE output_credential_generation IS NOT NULL;",
    "CREATE TABLE legacy_refresh_input_fences (
      input_credential_generation TEXT PRIMARY KEY NOT NULL COLLATE NOCASE) STRICT, WITHOUT ROWID;",
    "CREATE TABLE fleet_refresh_receipt_bindings (
      receipt_id INTEGER PRIMARY KEY REFERENCES outbound_request_receipts(id) ON DELETE CASCADE,
      attempt_id TEXT NOT NULL UNIQUE, input_credential_generation TEXT NOT NULL,
      output_credential_generation TEXT) STRICT;
     CREATE UNIQUE INDEX fleet_refresh_receipt_bindings_output
      ON fleet_refresh_receipt_bindings(output_credential_generation)
      WHERE output_credential_generation IS NOT NULL;",
    "CREATE TABLE fleet_refresh_input_fences (
      input_credential_generation TEXT PRIMARY KEY NOT NULL COLLATE NOCASE) STRICT, WITHOUT ROWID;",
];

const PREDECESSOR_NAMES: [&str; 6] = [
    "legacy_owner_tokens",
    "fleet_tokens",
    "legacy_refresh_receipt_bindings",
    "legacy_refresh_input_fences",
    "fleet_refresh_receipt_bindings",
    "fleet_refresh_input_fences",
];

/// One synthetic admitted process: store, the once-taken credential connection,
/// the data-directory lock, the secrets directory and the keyring handle.
struct Env {
    _root: tempfile::TempDir,
    data: PathBuf,
    store: Option<HubStore>,
    conn: Option<crate::db::CredentialConnection>,
    keys: Option<KeyringHandle>,
    admission: Arc<AdmittedUserHub>,
    secrets: PrivateDirectory,
    cursor: Option<crate::protocol::CursorKey>,
    exclusion: FakeExclusion,
    fake: bool,
}

impl Env {
    /// A fresh store at the rung (CT45) with the fake decrypt seams off.
    fn fresh() -> Self {
        let root = crate::private_tempdir().unwrap();
        let store = HubStore::initialize(root.path()).unwrap();
        Self::assemble(root, store)
    }

    fn assemble(root: tempfile::TempDir, store: HubStore) -> Self {
        let data = root.path().to_path_buf();
        Self::assemble_at(root, data, store)
    }

    fn assemble_at(root: tempfile::TempDir, data: PathBuf, store: HubStore) -> Self {
        let admission = AdmittedUserHub::for_test(&data).unwrap();
        let secrets = open_validated(&data.join("secrets"), true).unwrap();
        let mut conn = store.take_credential_connection().unwrap();
        conn.set_test_now_ms(T0_MS);
        Self {
            _root: root,
            data,
            store: Some(store),
            conn: Some(conn),
            keys: None,
            admission,
            secrets,
            cursor: None,
            exclusion: FakeExclusion(Ok(ExclusionState::Verified)),
            fake: false,
        }
    }

    /// A pre-upgrade store built by hand: both slots as given, the six
    /// predecessor tables of A3664 to A3669 and a `pending` state row.
    fn hand_built(
        owner: Option<&PriorRow>,
        fleet: Option<&PriorRow>,
        fences_pending: bool,
    ) -> Self {
        let mut env = Self::fresh();
        env.fake = true;
        {
            let db = env.db();
            for ddl in PREDECESSOR_DDL {
                db.execute_batch(ddl).unwrap();
            }
            db.execute(
                "UPDATE credential_format_state SET owner_api_state=?1, fleet_api_state=?2,
                 owner_api_blocked=NULL, fleet_api_blocked=NULL, fences_pending=?3,
                 old_tables_dropped=0, purge_required=1, wal_scrubbed=0, old_files_removed=0,
                 attempts_migrated=0, fences_fingerprinted=0, legacy_fences=0,
                 purge_needed_bytes=NULL, secrets_exclusion=NULL, durability=NULL,
                 durability_probed_at_ms=NULL, started_at_ms=NULL, converted_at_ms=NULL,
                 completed_at_ms=NULL WHERE singleton_id=1",
                params![
                    if owner.is_some() { "pending" } else { "none" },
                    if fleet.is_some() { "pending" } else { "none" },
                    fences_pending
                ],
            )
            .unwrap();
            if let Some(row) = owner {
                db.execute(
                    "INSERT INTO legacy_owner_tokens(singleton_id, access, refresh, expires_at,
                     next_refresh_at, credential_generation) VALUES(1, ?1, ?2, ?3, ?4, ?5)",
                    params![
                        row.access,
                        row.refresh,
                        row.expires,
                        row.next,
                        row.generation.map(|g| g.to_string())
                    ],
                )
                .unwrap();
            }
            if let Some(row) = fleet {
                db.execute(
                    "INSERT INTO fleet_tokens(singleton_id, access, refresh, client_id, region,
                     expires_at, next_refresh_at, credential_generation)
                     VALUES(1, ?1, ?2, 'client-0001', 'eu', ?3, ?4, ?5)",
                    params![
                        row.access,
                        row.refresh,
                        row.expires,
                        row.next,
                        row.generation.map(|g| g.to_string())
                    ],
                )
                .unwrap();
            }
        }
        env
    }

    /// A store from fixture item 1's catalogue (real OLD-D ciphertext), taken
    /// through the ordinary admitted writable open that runs the rung.
    fn from_catalogue(name: &str) -> Self {
        let root = crate::private_tempdir().unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        write_private(&root.path().join("hub.sqlite"), &fs::read(source).unwrap());
        let store = HubStore::initialize(root.path()).unwrap();
        Self::assemble(root, store)
    }

    fn data(&self) -> &Path {
        &self.data
    }

    fn secrets_path(&self) -> PathBuf {
        self.data().join("secrets")
    }

    fn db(&self) -> &Connection {
        self.conn.as_ref().unwrap().test_connection()
    }

    fn put(&self, name: &str, bytes: &[u8]) {
        write_private(&self.secrets_path().join(name), bytes);
    }

    fn state(&mut self) -> CredentialFormatState {
        self.conn
            .as_mut()
            .unwrap()
            .credential_format_state()
            .unwrap()
    }

    fn listing(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.secrets_path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// One admitted open's Phase B, with the seams of this environment.
    fn run(&mut self) -> Result<CredentialMigrationReport, CredentialMigrationError> {
        self.run_with(&mut || {})
    }

    fn run_with(
        &mut self,
        before_commit: &mut dyn FnMut(),
    ) -> Result<CredentialMigrationReport, CredentialMigrationError> {
        seam::with(|s| {
            s.fake_pre_upgrade = self.fake;
            s.fake_generation = self.fake;
        });
        let mut context = CredentialPhaseB {
            data_dir: &self.data,
            sqlite_temp_dir: &self.data,
            admission: &self.admission,
            secrets: &self.secrets,
            keys: &mut self.keys,
            cursor_key: self.cursor.as_ref(),
            exclusion: &self.exclusion,
            nonces: &mut OsNonces,
            now_ms: T0_MS,
        };
        run_phase_b(self.conn.as_mut().unwrap(), &mut context, before_commit)
    }

    /// A restarted process: the connection, the store and the keyring handle
    /// are dropped and the store is opened again (the rung runs zero times).
    fn reopen(&mut self) {
        self.conn = None;
        self.store = None;
        self.keys = None;
        let store = HubStore::initialize(&self.data).unwrap();
        let mut conn = store.take_credential_connection().unwrap();
        conn.set_test_now_ms(T0_MS);
        self.store = Some(store);
        self.conn = Some(conn);
    }

    fn stored(&mut self, slot: CredentialSlot) -> Option<crate::db::SealedCredentialPair> {
        self.conn
            .as_mut()
            .unwrap()
            .load_credential_pair(slot)
            .unwrap()
    }

    fn plaintexts(&mut self, slot: CredentialSlot) -> (Vec<u8>, Vec<u8>) {
        let pair = self.stored(slot).expect("migrated row present");
        if self.keys.is_none() {
            self.keys = crate::auth::credential_keys::load_keyring(&self.secrets).unwrap();
        }
        let (a, r) = pair
            .open(self.keys.as_ref().expect("keyring handle"))
            .unwrap();
        (a.to_vec(), r.to_vec())
    }

    fn table_exists(&self, name: &str) -> bool {
        self.db()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    }

    fn predecessors_present(&self) -> usize {
        PREDECESSOR_NAMES
            .iter()
            .filter(|n| self.table_exists(n))
            .count()
    }

    fn user_version(&self) -> i32 {
        self.db()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    fn insert_receipt(&self, slot: CredentialSlot, id: i64, started: i64, outcome: &str) {
        let fleet = slot == CredentialSlot::FleetApi;
        let done = outcome != "started";
        self.db()
            .execute(
                "INSERT INTO outbound_request_receipts(id, correlation_id, started_at_ms,
                 completed_at_ms, duration_ms, transport, operation, safety_class, precondition,
                 outcome) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'token_refresh', 'non_wake_endpoint',
                 'not_required', ?7)",
                params![
                    id,
                    uuid_n(40_000 + id as u128).to_string(),
                    started,
                    done.then_some(started + 10),
                    done.then_some(10_i64),
                    if fleet { "fleet_api" } else { "legacy_auth" },
                    outcome
                ],
            )
            .unwrap();
    }

    /// One predecessor binding row with its receipt; `receipt` is the start
    /// time and the stored outcome of the joined receipt row.
    fn binding(
        &self,
        slot: CredentialSlot,
        id: i64,
        attempt: Uuid,
        input: &str,
        output: Option<&str>,
        receipt: (i64, &str),
    ) {
        let (started, outcome) = receipt;
        self.insert_receipt(slot, id, started, outcome);
        let table = if slot == CredentialSlot::FleetApi {
            "fleet_refresh_receipt_bindings"
        } else {
            "legacy_refresh_receipt_bindings"
        };
        self.db()
            .execute(
                &format!(
                    "INSERT INTO {table}(receipt_id, attempt_id, input_credential_generation,
                     output_credential_generation) VALUES(?1, ?2, ?3, ?4)"
                ),
                params![id, attempt.to_string(), input, output],
            )
            .unwrap();
    }

    fn fence(&self, slot: CredentialSlot, generation: &str) {
        let table = if slot == CredentialSlot::FleetApi {
            "fleet_refresh_input_fences"
        } else {
            "legacy_refresh_input_fences"
        };
        self.db()
            .execute(
                &format!("INSERT INTO {table}(input_credential_generation) VALUES(?1)"),
                [generation],
            )
            .unwrap();
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn legacy_fences(&self) -> Vec<(String, String, i64)> {
        let mut statement = self
            .db()
            .prepare(
                "SELECT lower(legacy_generation), identity, fenced_at_ms
                 FROM credential_legacy_fences ORDER BY 1",
            )
            .unwrap();
        statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn all_files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![self.data().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.is_file() {
                    out.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        out
    }
}

/// Replaces a predecessor fence table with one under a non-canonical case of
/// its name, populated (E-A2, M2P-73(b), M2P-74(d) N-2): SQLite would refuse a
/// plain rename to a name that differs only in case.
fn recase_fence_table(env: &Env, canonical: &str, recased: &str) {
    env.db()
        .execute_batch(&format!(
            "DROP TABLE {canonical};
             CREATE TABLE \"{recased}\" (input_credential_generation TEXT PRIMARY KEY NOT NULL
               COLLATE NOCASE) STRICT, WITHOUT ROWID;
             INSERT INTO \"{recased}\" VALUES('00000000-0000-4000-8000-0000000000ff');"
        ))
        .unwrap();
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Whether any 16-byte run of `value` occurs in `haystack` (CT58's scan).
fn has_16_byte_run(haystack: &[u8], value: &[u8]) -> bool {
    value.len() >= 16 && value.windows(16).any(|run| contains(haystack, run))
}

fn is_complete(state: &CredentialFormatState) -> bool {
    state.completed_at_ms.is_some()
        && state.converted_at_ms.is_some()
        && !state.purge_required
        && state.wal_scrubbed
        && state.old_files_removed
        && state.old_tables_dropped
}

/// The event log as a message shows it: a stand-in name is replaced by its
/// alias label, so that no failure text carries the decoded value.
fn alias_view(events: &[String]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            event.replace(
                crate::runtime::name_test_support::forbidden_name(),
                "OLD-K1",
            )
        })
        .collect()
}

fn events_matching(prefix: &str) -> Vec<String> {
    seam::with(|s| {
        s.events
            .iter()
            .filter(|e| e.starts_with(prefix))
            .cloned()
            .collect()
    })
}

fn event_index(events: &[String], needle: &str) -> Option<usize> {
    events.iter().position(|e| e == needle)
}

// ---------------------------------------------------------------- CT45

#[test]
fn ct045_fresh_store() {
    seam::reset();
    // Case fresh_state_row: HubStore::initialize on an empty directory.
    let mut env = Env::fresh();
    let state = env.state();
    assert_eq!(
        state.owner_api_state,
        CredentialSlotFormatState::None,
        "CT45.state_row owner"
    );
    assert_eq!(
        state.fleet_api_state,
        CredentialSlotFormatState::None,
        "CT45.state_row fleet"
    );
    assert!(!state.fences_pending, "CT45.state_row fences_pending");
    assert!(
        state.old_tables_dropped,
        "CT45.state_row old_tables_dropped"
    );
    assert!(!state.purge_required, "CT45.state_row purge_required");
    assert!(state.wal_scrubbed, "CT45.state_row wal_scrubbed");
    assert!(state.old_files_removed, "CT45.state_row old_files_removed");
    assert!(
        state.converted_at_ms.is_some(),
        "CT45.state_row converted_at_ms"
    );
    assert!(
        state.completed_at_ms.is_some(),
        "CT45.state_row completed_at_ms"
    );
    assert_eq!(
        env.predecessors_present(),
        0,
        "CT45.state_row OLD-T1 to OLD-T6 absent"
    );
    assert_eq!(
        env.user_version(),
        RUNG,
        "CT45.state_row user_version is the rung"
    );

    // Case first_open_writes_nothing: Phase B on the fresh store writes
    // nothing (PRAGMA data_version on an observer connection) and runs no VACUUM.
    let observer = Connection::open(env.data().join("hub.sqlite")).unwrap();
    let before: i64 = observer
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    let before_state = format!("{:?}", env.state());
    seam::reset();
    let report = env
        .run()
        .expect("CT45.first_open Phase B succeeds on a fresh store");
    let after: i64 = observer
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "CT45.first_open data_version unchanged");
    assert_eq!(
        format!("{:?}", env.state()),
        before_state,
        "CT45.first_open state row unchanged"
    );
    assert_eq!(
        format!("{:?}", report.state),
        before_state,
        "CT45.first_open report state"
    );
    assert!(
        events_matching("vacuum").is_empty(),
        "CT45.first_open runs no VACUUM"
    );
    for write in [
        "conversion_commit",
        "conversion_barrier",
        "purge_commit",
        "scrub_marker",
        "files_marker",
        "complete_marker",
    ] {
        assert!(
            events_matching(write).is_empty(),
            "CT45.first_open records no {write} step on a complete store"
        );
    }
    assert!(
        env.keys.is_none(),
        "CT45.first_open creates no keyring on a store with no slot"
    );
    assert!(
        env.listing().is_empty(),
        "CT45.first_open leaves the secrets directory empty"
    );

    // Case second_open_rung_zero_times: a restarted admitted open keeps version
    // and state (the rung is a no-op at its own version).
    env.reopen();
    assert_eq!(env.user_version(), RUNG, "CT45.second_open user_version");
    assert_eq!(
        format!("{:?}", env.state()),
        before_state,
        "CT45.second_open state"
    );

    // Case below_rung_state_view (R43, M2P-16): below the rung the
    // non-mutating HubStore view returns None, queries no credential table and
    // leaves the catalogue bytes alone.
    let env = Env::fresh();
    env.db()
        .execute_batch(
            "DROP TABLE credential_refresh_attempts; DROP TABLE credential_refresh_fences;
             DROP TABLE credential_legacy_fences; DROP TABLE credential_pairs;
             DROP TABLE credential_sign_ins; DROP TABLE credential_key_usage;
             DROP TABLE credential_format_state; PRAGMA user_version=78;",
        )
        .unwrap();
    let snapshot = |env: &Env| -> Vec<Vec<u8>> {
        let mut out = vec![fs::read(env.data().join("hub.sqlite")).unwrap()];
        out.push(fs::read(env.data().join("hub.sqlite-wal")).unwrap_or_default());
        out
    };
    let bytes_before = snapshot(&env);
    let view = env
        .store
        .as_ref()
        .unwrap()
        .credential_format_state()
        .unwrap();
    assert!(view.is_none(), "CT45.below_rung state view is None");
    assert_eq!(
        snapshot(&env),
        bytes_before,
        "CT45.below_rung catalogue bytes unchanged"
    );
    // Status, doctor, preflight and read-only lanes, the rung counter and the
    // M-RUNG controls belong to V1 and V2 (pending, not claimed here).
}

// ---------------------------------------------------------------- CT46 to CT48

fn legacy_row(refresh: &str) -> (PriorRow, Vec<u8>) {
    let key = key32(1);
    (
        PriorRow::fake(&key, "access-0001", refresh, Some(uuid_n(0xaa))),
        key,
    )
}

/// Previous-process input producer for V1's admitted-open proofs. All six
/// predecessor tables and the fake decryptor are the accepted M fixtures.
pub(in crate::auth) fn seed_admitted_open_fixture(fleet: bool, outcome: Option<&str>, fenced: bool, candidate: usize) -> tempfile::TempDir {
    let (mut row, key) = legacy_row("refresh-0001");
    if fleet { row.generation = None; }
    let env = Env::hand_built((!fleet).then_some(&row), fleet.then_some(&row), outcome.is_some() || fenced);
    let slot = if fleet { CredentialSlot::FleetApi } else { CredentialSlot::OwnerApi };
    let names = if fleet { [FLEET_K4, RST_K2, FLEET_K5] } else { [REL_K1, RST_K1, REL_K2] };
    env.put(names[candidate], &key);
    if let Some(outcome) = outcome {
        env.binding(slot, 1, uuid_n(0x1001), &uuid_n(0xa9).to_string(), Some(&uuid_n(0xaa).to_string()), (1_799_999_000_000, outcome));
    }
    if fenced { env.fence(slot, &uuid_n(0xaa).to_string()); }
    // Returning only the root drops the admission, credential connection, store,
    // key handles and directory descriptor before V1's target open begins.
    env._root
}
pub(in crate::auth) fn seed_admitted_fence_mapping() -> tempfile::TempDir {
    let (row, key) = legacy_row("refresh-0001"); let env = Env::hand_built(Some(&row), None, true);
    env.put(REL_K1, &key);
    env.binding(CredentialSlot::OwnerApi, 1, uuid_n(0x1001), &uuid_n(0xa9).to_string(), Some(&uuid_n(0xaa).to_string()), (1_799_999_000_000, "success"));
    env.binding(CredentialSlot::OwnerApi, 2, uuid_n(0x1002), &uuid_n(0xaa).to_string(), None, (1_799_999_100_000, "started"));
    env.fence(CredentialSlot::OwnerApi, &uuid_n(0xa9).to_string()); env.fence(CredentialSlot::OwnerApi, &uuid_n(0xaa).to_string());
    env._root
}
pub(in crate::auth) fn seed_admitted_blocked_owner() -> tempfile::TempDir {
    let (owner, _) = legacy_row("refresh-0001"); let fleet_key = key32(7);
    let fleet = PriorRow::fake(&fleet_key, "fleet-access-0001", "fleet-refresh-0001", None);
    let env = Env::hand_built(Some(&owner), Some(&fleet), false); env.put(FLEET_K4, &fleet_key); env._root
}

/// Previous-process inputs shared with the admitted-open proofs. No live
/// credential handle or admission escapes this producer.
pub(in crate::auth) fn seed_admitted_large() -> tempfile::TempDir {
    let key = key32(1);
    let token = "t".repeat(16_000);
    let row = PriorRow::fake(&key, &token, &token, Some(uuid_n(0xaa)));
    let env = Env::hand_built(Some(&row), None, false);
    env.put(REL_K1, &key);
    env._root
}

pub(in crate::auth) fn seed_admitted_catalogue() -> (tempfile::TempDir, Vec<Vec<u8>>) {
    let vector = catalogue_vector("residue");
    let env = Env::from_catalogue("catalogue-residue.sqlite");
    env.put(REL_K1, &vector_bytes(&vector["legacy_current"], "key_hex"));
    env.put(REL_K2, &vector_bytes(&vector["earlier_replaced_deleted"], "key_hex"));
    env.put(FLEET_K4, &vector_bytes(&vector["fleet_current"], "key_hex"));
    (env._root, earlier_values(&vector).into_iter().map(|(_, value)| value).collect())
}

pub(in crate::auth) fn seed_admitted_cursor_fleet() -> (tempfile::TempDir, String, String) {
    let all = vectors();
    let row = all["ciphertext_cases"].as_array().unwrap().iter().find(|row| row.get("cursor_key_hex").is_some()).unwrap();
    let prior = PriorRow { access: vector_bytes(row,"access_ciphertext_hex"), refresh: vector_bytes(row,"refresh_ciphertext_hex"), expires: SCHEDULE.0, next: SCHEDULE.1, generation: Some(row["generation"].as_str().unwrap().parse().unwrap()) };
    let env = Env::hand_built(None,Some(&prior),false);
    env.put("hub-cursor.key", &vector_bytes(row,"cursor_key_hex"));
    (env._root,row["access_plaintext"].as_str().unwrap().into(),row["refresh_plaintext"].as_str().unwrap().into())
}

pub(in crate::auth) fn seed_admitted_candidate_set(fleet: bool, mask: usize, authentic: usize, equal: bool) -> tempfile::TempDir {
    let (mut row, key) = legacy_row("refresh-0001");
    if fleet { row.generation = None; }
    let env = Env::hand_built((!fleet).then_some(&row), fleet.then_some(&row), false);
    let names = if fleet { [FLEET_K4, RST_K2, FLEET_K5] } else { [REL_K1, RST_K1, REL_K2] };
    for (index, name) in names.into_iter().enumerate() {
        let different = key32(17 + index as u8);
        if mask & (1 << index) != 0 { env.put(name, if equal || index == authentic { &key } else { &different }); }
    }
    env._root
}

#[test]
fn ct046_owner_api_conversion() {
    seam::reset();
    let (row, key) = legacy_row("refresh-0001");
    let mut env = Env::hand_built(Some(&row), None, false);
    env.put(REL_K1, &key);
    let mut commits = 0usize;
    let report = env.run_with(&mut || commits += 1)
        .expect("CT46 Phase B converts the Legacy slot");
    assert_eq!(commits, 3, "R55 counts reservation, conversion and purge COMMITs");
    let report_on_replay = env.run_with(&mut || commits += 1)
        .expect("CT46 completed Phase B replay");
    assert!(is_complete(&report_on_replay.state));
    assert_eq!(commits, 3, "completed Phase B replay issues no counted COMMIT");
    assert_eq!(
        report.unsafe_candidates.len(),
        0,
        "CT46 no unsafe candidate"
    );
    let pair = env
        .stored(CredentialSlot::OwnerApi)
        .expect("CT46 Legacy row present");
    assert!(pair.origin.is_none(), "CT46 origin is NULL (not recorded)");
    assert_eq!(
        pair.refresh_owner,
        crate::auth::credential_envelope::RefreshOwner::External,
        "CT46 owner external without a completed binding"
    );
    assert_eq!(pair.generation, uuid_n(0xaa), "CT46 generation kept");
    assert_eq!(
        (pair.expires_at, pair.next_refresh_at),
        SCHEDULE,
        "CT46 schedule verbatim"
    );
    assert!(pair.migrated, "CT46 migrated = 1");
    assert_eq!(
        pair.refresh_fingerprint,
        Some(refresh_fingerprint(b"refresh-0001")),
        "CT46 fingerprint F(refresh-0001)"
    );
    let (access, refresh) = env.plaintexts(CredentialSlot::OwnerApi);
    assert_eq!(access, b"access-0001", "CT46 access opens to its plaintext");
    assert_eq!(
        refresh, b"refresh-0001",
        "CT46 refresh opens to its plaintext"
    );
    let state = env.state();
    assert_eq!(
        state.owner_api_state,
        CredentialSlotFormatState::Migrated,
        "CT46 slot migrated"
    );
    assert!(
        is_complete(&state),
        "CT46 converted, cleaned and complete: {state:?}"
    );
    assert_eq!(
        env.predecessors_present(),
        0,
        "CT46 OLD-T1 to OLD-T6 absent"
    );
    assert!(
        !env.listing().contains(&REL_K1.to_owned()),
        "CT46 REL-K1 removed"
    );
    assert!(
        check_keyring(&env.secrets).unwrap().is_some(),
        "CT46 a keyring exists in the secrets directory"
    );
    assert_eq!(
        env.user_version(),
        RUNG,
        "CT46 user_version equals the rung"
    );
}

#[test]
fn ct047_owner_by_evidence() {
    // Cases: the refresh owner follows a completed, successful binding whose
    // output is the row's generation (R46, M2P-73(a)); a Started or failed
    // binding never makes it Hub-owned. The oracle is independent of S's.
    use crate::auth::credential_envelope::RefreshOwner;
    for (case, outcome, expected) in [
        ("success", "success", RefreshOwner::Hub),
        ("started", "started", RefreshOwner::External),
        ("http_error", "http_error", RefreshOwner::External),
        ("cancelled", "cancelled", RefreshOwner::External),
    ] {
        seam::reset();
        let (row, key) = legacy_row("refresh-0001");
        let mut env = Env::hand_built(Some(&row), None, true);
        env.binding(
            CredentialSlot::OwnerApi,
            1,
            uuid_n(0x1001),
            &uuid_n(0xa9).to_string(),
            Some(&uuid_n(0xaa).to_string()),
            (1_799_999_000_000, outcome),
        );
        env.put(REL_K1, &key);
        env.run()
            .unwrap_or_else(|e| panic!("CT47.{case} Phase B failed: {e:?}"));
        let pair = env.stored(CredentialSlot::OwnerApi).unwrap();
        assert_eq!(pair.refresh_owner, expected, "CT47.{case} refresh owner");
    }
}

#[test]
fn ct048_fleet_conversion() {
    use crate::auth::credential_envelope::{CredentialOrigin, RefreshOwner};
    let cursor = crate::protocol::CursorKey::from_bytes([9; 32]);
    let k7 = cursor.fleet_credential_encryption_key().to_vec();
    // (case, key file, key content, cursor supplied, key used for the row)
    let cases: [(&str, Option<&str>, Vec<u8>, bool, Vec<u8>); 3] = [
        ("k4_file", Some(FLEET_K4), key32(7), false, key32(7)),
        ("k5_file", Some(FLEET_K5), key32(8), false, key32(8)),
        ("k7_only", None, Vec::new(), true, k7),
    ];
    for (case, name, content, with_cursor, row_key) in cases {
        seam::reset();
        let row = PriorRow::fake(&row_key, "access-0001", "refresh-0001", None);
        let mut env = Env::hand_built(None, Some(&row), false);
        if let Some(name) = name {
            env.put(name, &content);
        }
        if with_cursor {
            env.cursor = Some(cursor.clone());
        }
        env.run()
            .unwrap_or_else(|e| panic!("CT48.{case} Phase B failed: {e:?}"));
        let pair = env.stored(CredentialSlot::FleetApi).unwrap();
        assert_eq!(
            pair.origin,
            Some(CredentialOrigin::FleetOauth),
            "CT48.{case} origin"
        );
        assert_eq!(
            pair.refresh_owner,
            RefreshOwner::Hub,
            "CT48.{case} owner hub"
        );
        assert_eq!(
            pair.generation,
            Uuid::new_v5(&uuid_n(3), b"refresh-0001"),
            "CT48.{case} generation is FakeOldG over the decrypted pair"
        );
        assert_eq!(
            pair.client_id.as_deref(),
            Some("client-0001"),
            "CT48.{case} client"
        );
        assert_eq!(pair.region.as_deref(), Some("eu"), "CT48.{case} region");
        assert_eq!(
            (pair.expires_at, pair.next_refresh_at),
            SCHEDULE,
            "CT48.{case} schedule"
        );
        let (access, refresh) = env.plaintexts(CredentialSlot::FleetApi);
        assert_eq!(
            (access, refresh),
            (b"access-0001".to_vec(), b"refresh-0001".to_vec())
        );
        assert!(is_complete(&env.state()), "CT48.{case} complete");
        assert!(
            env.listing()
                .iter()
                .all(|n| !NEUTRAL_FILES.contains(&n.as_str())),
            "CT48.{case} files removed"
        );
    }
}

// ---------------------------------------------------------------- CT49, CT50

fn fenced_owner_env() -> (Env, Uuid, Uuid) {
    let (ga, gcur) = (uuid_n(0xa1), uuid_n(0xaa));
    let (row, key) = legacy_row("refresh-0001");
    let env = Env::hand_built(Some(&row), None, true);
    env.binding(
        CredentialSlot::OwnerApi,
        1,
        uuid_n(0x2001),
        &ga.to_string(),
        Some(&gcur.to_string()),
        (1_799_999_000_000, "success"),
    );
    env.binding(
        CredentialSlot::OwnerApi,
        2,
        uuid_n(0x2002),
        &gcur.to_string(),
        None,
        (1_799_999_500_000, "started"),
    );
    env.fence(CredentialSlot::OwnerApi, &ga.to_string());
    env.fence(CredentialSlot::OwnerApi, &gcur.to_string());
    env.put(REL_K1, &key);
    (env, ga, gcur)
}

#[test]
fn ct049_fence_mapping() {
    seam::reset();
    let (mut env, ga, gcur) = fenced_owner_env();
    env.run().expect("CT49 Phase B maps the fences");
    assert_eq!(
        env.count(
            "SELECT COUNT(*) FROM credential_refresh_attempts WHERE migrated=1 AND fence_id IS NULL"
        ),
        2,
        "CT49 attempts b1 and b2 migrated with no fence link"
    );
    assert_eq!(
        env.count("SELECT COUNT(*) FROM credential_refresh_attempts"),
        2,
        "CT49 two attempts"
    );
    let output: String = env
        .db()
        .query_row(
            "SELECT output_generation FROM credential_refresh_attempts WHERE receipt_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(output, gcur.to_string(), "CT49 b1 output is Gcur");
    let fences = env.legacy_fences();
    assert_eq!(
        fences.iter().map(|f| f.0.clone()).collect::<BTreeSet<_>>(),
        BTreeSet::from([ga.to_string(), gcur.to_string()]),
        "CT49 legacy fences Ga and Gcur"
    );
    let identity = |g: Uuid| {
        fences
            .iter()
            .find(|f| f.0 == g.to_string())
            .unwrap()
            .1
            .clone()
    };
    assert_eq!(
        identity(gcur),
        "anchored",
        "CT49 the current generation is anchored"
    );
    assert_eq!(identity(ga), "opaque", "CT49 Ga is opaque");
    assert_eq!(
        env.count("SELECT COUNT(*) FROM credential_refresh_fences WHERE kind='migrated'"),
        1,
        "CT49 one migrated fingerprint fence"
    );
    let stored_fp: Vec<u8> = env
        .db()
        .query_row(
            "SELECT fingerprint FROM credential_refresh_fences",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stored_fp,
        refresh_fingerprint(b"refresh-0001").to_vec(),
        "CT49 fingerprint F(current refresh)"
    );
    assert!(
        env.conn
            .as_mut()
            .unwrap()
            .has_unresolved_credential_refresh(CredentialSlot::OwnerApi)
            .unwrap(),
        "CT49 has_unresolved(owner_api)"
    );
    let pair = env.stored(CredentialSlot::OwnerApi).unwrap();
    assert_eq!(
        pair.refresh_owner,
        crate::auth::credential_envelope::RefreshOwner::Hub,
        "CT49 owner hub from b1's evidence"
    );
    let state = env.state();
    assert_eq!(
        (
            state.attempts_migrated,
            state.fences_fingerprinted,
            state.legacy_fences
        ),
        (2, 1, 2),
        "CT49 counts"
    );
    assert_eq!(
        report_opaque_outputs(&mut env),
        0,
        "CT49 no opaque fence is a binding output: Ga is an input only and Gcur is anchored"
    );
    // cancel_unsent(b2) closes b2's receipt and deletes no fence; begin on the
    // fenced current generation is refused as spent.
    let conn = env.conn.as_mut().unwrap();
    conn.cancel_unsent_credential_refresh(crate::db::OutboundRequestReceiptId(2), gcur)
        .expect("CT49 cancel_unsent closes b2");
    assert_eq!(
        env.count("SELECT COUNT(*) FROM credential_legacy_fences"),
        2,
        "CT49 no legacy fence deleted"
    );
    let refusal = env
        .conn
        .as_mut()
        .unwrap()
        .begin_credential_refresh(CredentialSlot::OwnerApi, gcur);
    assert!(
        matches!(
            refusal,
            Err(StoreError::RefreshOutcomeUnknown {
                reason: BeginRefusal::TokenSpent
            })
        ),
        "CT49 begin(owner_api, Gcur) is TokenSpent: {refusal:?}"
    );
}

/// The receipt's count of opaque fences that are binding outputs, recomputed
/// from the stored tables (an oracle independent of the report field).
fn report_opaque_outputs(env: &mut Env) -> i64 {
    env.count(
        "SELECT COUNT(*) FROM credential_legacy_fences f WHERE f.identity='opaque' AND EXISTS(
           SELECT 1 FROM credential_refresh_attempts a
           WHERE lower(a.output_generation)=lower(f.legacy_generation))",
    )
}

#[test]
fn ct050_dead_current_token() {
    seam::reset();
    let gcur = uuid_n(0xaa);
    let (row, key) = legacy_row("refresh-0001");
    let mut env = Env::hand_built(Some(&row), None, true);
    env.fence(CredentialSlot::OwnerApi, &gcur.to_string());
    env.put(REL_K1, &key);
    env.run()
        .expect("CT50 Phase B converts a slot whose current generation is fenced");
    let refusal = env
        .conn
        .as_mut()
        .unwrap()
        .begin_credential_refresh(CredentialSlot::OwnerApi, gcur);
    assert!(
        matches!(
            refusal,
            Err(StoreError::RefreshOutcomeUnknown {
                reason: BeginRefusal::TokenSpent
            })
        ),
        "CT50 begin(owner_api, Gcur) gives TokenSpent: {refusal:?}"
    );
    assert_eq!(
        env.count("SELECT COUNT(*) FROM credential_refresh_fences WHERE kind='migrated'"),
        1,
        "CT50 the fenced current token has a migrated fingerprint fence"
    );
    assert_eq!(
        env.count("SELECT COUNT(*) FROM credential_refresh_attempts"),
        0,
        "CT50 no attempt"
    );
}

// ---------------------------------------------------------------- CT51, CT52

#[derive(Debug, PartialEq, Eq)]
struct EndState {
    plaintexts: Vec<(Vec<u8>, Vec<u8>)>,
    metadata: Vec<String>,
    fences: Vec<(String, String, i64)>,
    files: Vec<String>,
    counts: (i64, i64, i64),
}

fn end_state(env: &mut Env, slots: &[CredentialSlot]) -> EndState {
    let plaintexts = slots.iter().map(|s| env.plaintexts(*s)).collect();
    let metadata = {
        let mut statement = env
            .db()
            .prepare(
                "SELECT slot || '|' || coalesce(origin,'-') || '|' || refresh_owner || '|' ||
                 generation || '|' || expires_at || '|' || next_refresh_at || '|' || migrated
                 FROM credential_pairs ORDER BY slot",
            )
            .unwrap();
        statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let state = env.state();
    EndState {
        plaintexts,
        metadata,
        fences: env.legacy_fences(),
        files: env
            .listing()
            .into_iter()
            .filter(|n| n != "credential-keys")
            .collect(),
        counts: (
            state.attempts_migrated,
            state.fences_fingerprinted,
            state.legacy_fences,
        ),
    }
}

fn stop_at(point: &str) {
    let point = point.to_owned();
    seam::with(|s| {
        s.boundary = Some(Box::new(move |name| name != point));
    });
}

fn keyring_id(env: &Env) -> Vec<u8> {
    let check = check_keyring(&env.secrets).unwrap().unwrap();
    check.current.id.0.to_vec()
}

fn crash_fixture(fleet: bool) -> Env {
    let (row, key) = legacy_row("refresh-0001");
    let fleet_key = key32(7);
    let fleet_row = PriorRow::fake(&fleet_key, "access-0002", "refresh-0002", None);
    let env = Env::hand_built(Some(&row), fleet.then_some(&fleet_row), true);
    env.binding(
        CredentialSlot::OwnerApi,
        1,
        uuid_n(0x3001),
        &uuid_n(0xa1).to_string(),
        Some(&uuid_n(0xaa).to_string()),
        (1_799_999_000_000, "success"),
    );
    env.fence(CredentialSlot::OwnerApi, &uuid_n(0xa1).to_string());
    env.put(REL_K1, &key);
    if fleet {
        env.put(FLEET_K4, &fleet_key);
    }
    env
}

#[test]
fn ct051_conversion_crash_points() {
    let slots = [CredentialSlot::OwnerApi];
    seam::reset();
    let mut baseline = crash_fixture(false);
    baseline.run().expect("CT51 uninterrupted baseline run");
    let expected = end_state(&mut baseline, &slots);
    assert!(is_complete(&baseline.state()), "CT51 baseline completes");

    // Cases: the stop is a boundary of the run; the rerun is a restarted
    // process. Neutral candidates stay until the conversion barrier passed.
    let stops: [(&str, bool); 6] = [
        ("after_keyring", true),
        ("after_conversion", true),
        ("after_purge_commit", false),
        ("after_scrub_marker", false),
        ("after_removal:pre-upgrade-owner-api.relocated.key", false),
        ("after_files_marker", false),
    ];
    for (point, files_must_remain) in stops {
        seam::reset();
        let mut env = crash_fixture(false);
        stop_at(point);
        let stopped = env.run();
        assert!(
            stopped.is_err(),
            "CT51.{point} the run stops at its boundary"
        );
        assert!(
            env.state().completed_at_ms.is_none(),
            "CT51.{point} only the run that finishes step 9 sets completed_at_ms"
        );
        if files_must_remain {
            assert!(
                env.listing().contains(&REL_K1.to_owned()),
                "CT51.{point} no neutral candidate is removed before the conversion barrier"
            );
        }
        let key_at_stop = env
            .secrets_path()
            .join("credential-keys")
            .exists()
            .then(|| keyring_id(&env));
        seam::with(|s| s.boundary = None);
        env.reopen();
        env.run()
            .unwrap_or_else(|e| panic!("CT51.{point} rerun failed: {e:?}"));
        assert!(is_complete(&env.state()), "CT51.{point} rerun completes");
        if let Some(id) = key_at_stop {
            assert_eq!(
                keyring_id(&env),
                id,
                "CT51.{point} the keyring created at the stop is reused"
            );
        }
        assert_eq!(
            end_state(&mut env, &slots),
            expected,
            "CT51.{point} same end state as an uninterrupted run"
        );
    }

    // Case failed_conversion_barrier: the barrier after the step 5 commit fails
    // (one slot: call 1 records the exclusion, call 2 is the invocation
    // reservation, call 3 the conversion barrier). No file is removed, no purge runs, and the next open repeats
    // the barrier before going on.
    seam::reset();
    let mut env = crash_fixture(false);
    env.conn
        .as_mut()
        .unwrap()
        .set_test_barrier_failure_at(Some(3));
    let failed = env.run();
    assert!(failed.is_err(), "CT51.failed_barrier Phase B stops");
    assert!(
        env.listing().contains(&REL_K1.to_owned()),
        "CT51.failed_barrier REL-K1 stays"
    );
    assert!(
        events_matching("remove:").is_empty(),
        "CT51.failed_barrier no removal attempted"
    );
    assert!(
        events_matching("vacuum").is_empty(),
        "CT51.failed_barrier no purge attempted"
    );
    env.reopen();
    seam::with(|s| s.events.clear());
    env.run().expect("CT51.failed_barrier rerun completes");
    {
        let events = seam::with(|s| s.events.clone());
        let repeat = event_index(&events, "conversion_barrier_repeat");
        let first_later = events.iter().position(|e| {
            matches!(e.as_str(), "vacuum" | "purge_commit" | "scrub_marker")
                || e.starts_with("remove:")
        });
        assert!(
            matches!((repeat, first_later), (Some(r), Some(l)) if r < l),
            "CT51.failed_barrier the next open repeats the barrier before the purge, scrub or removal: {:?}",
            alias_view(&events)
        );
    }
    assert_eq!(
        end_state(&mut env, &slots),
        expected,
        "CT51.failed_barrier end state"
    );

    // Case vacuum_interrupt: a VACUUM stopped by the interrupt seam leaves
    // purge_required = 1; the rerun purges.
    seam::reset();
    let mut env = crash_fixture(false);
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handler_flag = flag.clone();
    let _ = env.db().progress_handler(
        1,
        Some(move || handler_flag.load(std::sync::atomic::Ordering::SeqCst)),
    );
    let arm = flag.clone();
    seam::with(|s| {
        s.boundary = Some(Box::new(move |name| {
            if name == "before_vacuum" {
                arm.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            true
        }));
    });
    assert!(
        env.run().is_err(),
        "CT51.vacuum_interrupt the interrupted VACUUM is an error"
    );
    flag.store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = env.db().progress_handler(1, None::<fn() -> bool>);
    let state = env.state();
    assert!(
        state.purge_required && state.converted_at_ms.is_some(),
        "CT51.vacuum_interrupt purge still required"
    );
    seam::with(|s| s.boundary = None);
    env.reopen();
    env.run()
        .expect("CT51.vacuum_interrupt rerun purges and completes");
    assert_eq!(
        end_state(&mut env, &slots),
        expected,
        "CT51.vacuum_interrupt end state"
    );

    // Case fleet_key_names: the Fleet slot runs the same stop with its key in
    // OLD-K4, then in RST-K2, then in OLD-K5.
    for name in [FLEET_K4, RST_K2, FLEET_K5] {
        seam::reset();
        let fleet_key = key32(7);
        let row = PriorRow::fake(&fleet_key, "access-0002", "refresh-0002", None);
        let mut env = Env::hand_built(None, Some(&row), false);
        env.put(name, &fleet_key);
        stop_at("after_conversion");
        assert!(
            env.run().is_err(),
            "CT51.fleet.{name} stops after conversion"
        );
        assert!(
            env.listing().contains(&name.to_owned()),
            "CT51.fleet.{name} key file stays"
        );
        seam::with(|s| s.boundary = None);
        env.reopen();
        env.run()
            .unwrap_or_else(|e| panic!("CT51.fleet.{name} rerun failed: {e:?}"));
        assert!(is_complete(&env.state()), "CT51.fleet.{name} complete");
        assert!(
            !env.listing().contains(&name.to_owned()),
            "CT51.fleet.{name} key file removed"
        );
        assert_eq!(
            env.plaintexts(CredentialSlot::FleetApi).1,
            b"refresh-0002",
            "CT51.fleet.{name} plaintext"
        );
    }

    // Case rerun_after_keyring (E56-PENDING): with a row in credential_pairs
    // and no keyring, step 2 stops with KeyringMissing (R17), creates no
    // keyring and leaves the pending slot pending. Once a keyring appears the
    // same connection runs Phase B again and converts it; the sign-in
    // orchestration itself is V1 and V2.
    seam::reset();
    let mut env = crash_fixture(false);
    env.db()
        .execute(
            "INSERT INTO credential_pairs(account_id, slot, origin, refresh_owner, generation,
              access_envelope, refresh_envelope, refresh_present, refresh_fingerprint, expires_at,
              next_refresh_at, client_id, region, migrated, written_at_ms)
             VALUES(1, 'fleet_api', 'fleet_oauth', 'hub', ?1, zeroblob(100), zeroblob(100), 1,
              zeroblob(32), 1800028800, 1800021600, 'client-0001', 'eu', 0, 1)",
            [uuid_n(0xee).to_string()],
        )
        .unwrap();
    let missing = env.run();
    assert!(
        matches!(
            missing,
            Err(CredentialMigrationError::Key(
                CredentialKeyError::KeyringMissing
            ))
        ),
        "CT51.rerun_after_keyring first run stops with KeyringMissing: {missing:?}"
    );
    assert!(
        !env.listing().contains(&"credential-keys".to_owned()),
        "CT51.rerun_after_keyring no keyring created"
    );
    assert_eq!(
        env.state().owner_api_state,
        CredentialSlotFormatState::Pending,
        "CT51.rerun_after_keyring slot pending"
    );
    assert!(
        env.listing().contains(&REL_K1.to_owned()),
        "CT51.rerun_after_keyring REL-K1 stays"
    );
    let created =
        crate::auth::credential_keys::create_keyring(&env.secrets, &env.exclusion, 1_800_000_000);
    assert!(
        created.is_ok(),
        "CT51.rerun_after_keyring a keyring appears"
    );
    env.run()
        .expect("CT51.rerun_after_keyring the second run on the same connection converts");
    assert_eq!(
        env.state().owner_api_state,
        CredentialSlotFormatState::Migrated,
        "CT51.rerun_after_keyring migrated"
    );

    // Case keyring_exclusion_failed (R18, R44 step 2): the exclusion check
    // fails, no keyring is created, the failure is recorded and Phase B stops.
    seam::reset();
    let mut env = crash_fixture(false);
    env.exclusion = FakeExclusion(Err(CredentialKeyError::BackupExclusionFailed));
    let failed = env.run();
    assert!(
        matches!(
            failed,
            Err(CredentialMigrationError::Key(
                CredentialKeyError::BackupExclusionFailed
            ))
        ),
        "CT51.keyring_exclusion_failed Phase B stops with the exclusion failure: {failed:?}"
    );
    assert!(
        !env.listing().contains(&"credential-keys".to_owned()),
        "CT51.keyring_exclusion_failed no keyring is created"
    );
    let recorded = env.state().secrets_exclusion;
    assert_eq!(
        recorded,
        Some(CredentialSecretsExclusion::Failed),
        "CT51.keyring_exclusion_failed the failure is recorded: {recorded:?}"
    );
    assert_eq!(
        env.state().owner_api_state,
        CredentialSlotFormatState::Pending,
        "CT51.keyring_exclusion_failed the slot stays pending"
    );
}

#[test]
fn ct052_conversion_disk_full() {
    seam::reset();
    // Large tokens need new pages; with the page count pinned the commit of
    // step 5 fails with SQLITE_FULL and rolls back.
    let key = key32(1);
    let big = "t".repeat(16_000);
    let row = PriorRow::fake(&key, &big, &big, Some(uuid_n(0xaa)));
    let mut env = Env::hand_built(Some(&row), None, false);
    env.put(REL_K1, &key);
    let prior_access: Vec<u8> = env
        .db()
        .query_row("SELECT access FROM legacy_owner_tokens", [], |r| r.get(0))
        .unwrap();
    let pages: i64 = env.count("PRAGMA page_count");
    env.db()
        .execute_batch(&format!("PRAGMA max_page_count={pages}"))
        .unwrap();
    let failed = env.run();
    assert!(failed.is_err(), "CT52 step 5 fails with SQLITE_FULL");
    let state = env.state();
    assert_eq!(
        state.owner_api_state,
        CredentialSlotFormatState::Pending,
        "CT52 slot stays pending"
    );
    assert!(state.converted_at_ms.is_none(), "CT52 not converted");
    let still: Vec<u8> = env
        .db()
        .query_row("SELECT access FROM legacy_owner_tokens", [], |r| r.get(0))
        .unwrap();
    assert_eq!(still, prior_access, "CT52 OLD-T1 row byte-identical");
    assert!(
        env.listing().contains(&REL_K1.to_owned()),
        "CT52 REL-K1 present"
    );
    assert_eq!(
        env.count("SELECT COUNT(*) FROM credential_pairs"),
        0,
        "CT52 no new row"
    );
    env.db()
        .execute_batch("PRAGMA max_page_count=1073741823")
        .unwrap();
    env.reopen();
    env.run()
        .expect("CT52 after raising the limit the next open completes");
    assert!(is_complete(&env.state()), "CT52 complete");
}

// ---------------------------------------------------------------- CT53

#[test]
fn ct053_blocked_slot() {
    use crate::auth::credential_envelope::RefreshOwner;
    // Case no_candidate_authenticates: the Legacy candidates all fail while
    // the Fleet slot migrates.
    seam::reset();
    let wrong = key32(99);
    let legacy = PriorRow::fake(&key32(1), "access-0001", "refresh-0001", Some(uuid_n(0xaa)));
    let fleet_key = key32(7);
    let fleet = PriorRow::fake(&fleet_key, "access-0002", "refresh-0002", None);
    let mut env = Env::hand_built(Some(&legacy), Some(&fleet), true);
    env.fence(CredentialSlot::OwnerApi, &uuid_n(0xaa).to_string());
    env.put(REL_K1, &wrong);
    env.put(FLEET_K4, &fleet_key);
    let report = env
        .run()
        .expect("CT53 Phase B finishes with a blocked slot");
    let state = env.state();
    assert_eq!(
        state.fleet_api_state,
        CredentialSlotFormatState::Migrated,
        "CT53 Fleet migrates"
    );
    assert_eq!(
        state.owner_api_state,
        CredentialSlotFormatState::Pending,
        "CT53 Legacy stays pending"
    );
    assert_eq!(
        state.owner_api_blocked,
        Some(CredentialMigrationBlockReason::OldKeyUnavailable),
        "CT53 blocked reason old_key_unavailable"
    );
    assert!(!state.old_tables_dropped, "CT53 old_tables_dropped = 0");
    assert!(!state.fences_pending, "CT53 fences already mapped");
    assert_eq!(
        env.count("SELECT COUNT(*) FROM legacy_owner_tokens"),
        1,
        "CT53 OLD-T1 keeps its row"
    );
    assert_eq!(
        env.count("SELECT COUNT(*) FROM credential_legacy_fences"),
        1,
        "CT53 legacy fence mapped"
    );
    assert!(
        state.completed_at_ms.is_none(),
        "CT53 no completion while a slot waits"
    );
    assert_eq!(
        report.state.owner_api_blocked, state.owner_api_blocked,
        "CT53 report carries the state"
    );
    assert!(
        env.stored(CredentialSlot::OwnerApi).is_none(),
        "CT53 no Legacy row written"
    );

    // Case late_key: a candidate that authenticates appears; the next open
    // converts the slot, anchors its fence and removes the key.
    env.put(REL_K1, &key32(1));
    env.reopen();
    env.run().expect("CT53.late_key next open converts");
    assert_eq!(
        env.state().owner_api_state,
        CredentialSlotFormatState::Migrated,
        "CT53.late_key migrated"
    );
    assert_eq!(
        env.legacy_fences()[0].1,
        "anchored",
        "CT53.late_key identity becomes anchored"
    );
    assert_eq!(
        env.count("SELECT COUNT(*) FROM credential_refresh_fences WHERE kind='migrated'"),
        1,
        "CT53.late_key migrated fingerprint fence"
    );
    assert!(is_complete(&env.state()), "CT53.late_key complete");
    assert!(
        !env.listing().contains(&REL_K1.to_owned()),
        "CT53.late_key REL-K1 removed"
    );
    assert_eq!(
        env.stored(CredentialSlot::OwnerApi).unwrap().refresh_owner,
        RefreshOwner::External
    );

    // Case old_pair_invalid: a decrypted refresh token with an empty canonical
    // form is blocked and the next candidate is not tried: the spec's CR LF
    // example, and a single space as an extra case.
    for (label, refresh) in [("crlf", "\r\n"), ("space", " ")] {
        seam::reset();
        let key = key32(1);
        let invalid = PriorRow::fake(&key, "access-0001", refresh, Some(uuid_n(0xaa)));
        let mut env = Env::hand_built(Some(&invalid), None, false);
        env.put(REL_K1, &key);
        env.put(RST_K1, &key32(2));
        env.run()
            .unwrap_or_else(|e| panic!("CT53.old_pair_invalid.{label} Phase B finishes: {e:?}"));
        let blocked = env.state().owner_api_blocked;
        assert_eq!(
            blocked,
            Some(CredentialMigrationBlockReason::OldPairInvalid),
            "CT53.old_pair_invalid.{label} reason was {blocked:?}"
        );
        assert_eq!(
            decrypt_attempts(CredentialSlot::OwnerApi),
            vec![key.clone()],
            "CT53.old_pair_invalid.{label} the next candidate is not tried"
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM legacy_owner_tokens"),
            1,
            "CT53.old_pair_invalid.{label} row kept"
        );
    }

    // Case production_refused_arm (FR-6): the production classification of OLD-D's
    // verdict. InvalidPlaintext (authenticated but refused) blocks the slot as
    // old_pair_invalid and no further candidate is tried; every other error
    // means the candidate does not authenticate. No approved vector carries an
    // authenticated-but-refused ciphertext (the encryptors are absent), so the
    // arm is exercised through its classification function.
    {
        use crate::source_token_envelope::SourceDbTokenError as E;
        assert!(
            matches!(
                classify_decryption(Err(E::InvalidPlaintext)),
                Decrypted::Refused
            ),
            "CT53.production_refused_arm InvalidPlaintext is a refused pair"
        );
        for other in [
            E::AuthenticationFailed,
            E::UnsupportedEnvelope,
            E::InvalidEncryptionKey,
            E::EmptyEncryptionKey,
            E::EncryptionFailed,
            E::EntropyUnavailable,
        ] {
            assert!(
                matches!(classify_decryption(Err(other)), Decrypted::NoMatch),
                "CT53.production_refused_arm every other error means no match"
            );
        }
        let tokens =
            crate::credentials::OwnerTokens::from_secret_parts("a".to_owned(), "r".to_owned())
                .unwrap();
        assert!(
            matches!(classify_decryption(Ok(tokens)), Decrypted::Tokens(_)),
            "CT53.production_refused_arm an accepted pair converts"
        );
    }

    // Case key_only_under_an_old_name (R48, R80): no keyring and no
    // credential_pairs row, the Legacy key exists only under an old-name
    // stand-in and the Fleet key is OLD-K4. Phase B creates the keyring,
    // converts Fleet and leaves Legacy blocked; the stand-in is untouched.
    seam::reset();
    let legacy = PriorRow::fake(&key32(1), "access-0001", "refresh-0001", Some(uuid_n(0xaa)));
    let mut env = Env::hand_built(Some(&legacy), Some(&fleet), true);
    let stand_in = stand_in_name("legacy.key");
    env.put(&stand_in, &key32(1));
    env.put(FLEET_K4, &fleet_key);
    let meta_before = fs::metadata(env.secrets_path().join(&stand_in)).unwrap();
    let bytes_before = fs::read(env.secrets_path().join(&stand_in)).unwrap();
    let result = env.run();
    assert!(
        !matches!(
            result,
            Err(CredentialMigrationError::Key(
                CredentialKeyError::KeyringMissing
            ))
        ),
        "CT53.stand_in_name never reported as a missing keyring"
    );
    result.expect("CT53.stand_in_name Phase B finishes");
    let state = env.state();
    assert_eq!(
        state.owner_api_blocked,
        Some(CredentialMigrationBlockReason::OldKeyUnavailable)
    );
    assert_eq!(state.fleet_api_state, CredentialSlotFormatState::Migrated);
    assert!(
        check_keyring(&env.secrets).unwrap().is_some(),
        "CT53.stand_in_name keyring created and usable"
    );
    let meta_after = fs::metadata(env.secrets_path().join(&stand_in)).unwrap();
    assert_eq!(
        fs::read(env.secrets_path().join(&stand_in)).unwrap(),
        bytes_before,
        "stand-in bytes"
    );
    assert_eq!(
        (
            meta_before.ino(),
            meta_before.mtime(),
            meta_before.mtime_nsec()
        ),
        (
            meta_after.ino(),
            meta_after.mtime(),
            meta_after.mtime_nsec()
        ),
        "CT53.stand_in_name stand-in inode and mtime"
    );
    assert!(
        seam::with(|s| s.events.iter().all(|e| !e.contains(&stand_in))),
        "CT53.stand_in_name no operation names the stand-in"
    );
    // Relocating the key to its relocated name lets the next open convert.
    env.put(REL_K1, &key32(1));
    env.reopen();
    env.run()
        .expect("CT53.stand_in_name relocated key converts");
    assert!(
        is_complete(&env.state()),
        "CT53.stand_in_name complete after relocation"
    );
    assert!(
        env.listing().contains(&stand_in),
        "CT53.stand_in_name stand-in still present"
    );

    // Case noncanonical_predecessor_refused (E-A2, M2P-73(b)): a populated
    // predecessor table under a non-canonical case is refused with
    // CredentialPairInvalid at version 78 with nothing dropped; the canonical
    // table stays pending.
    seam::reset();
    let mut env = Env::hand_built(Some(&legacy), None, false);
    env.put(REL_K1, &key32(1));
    recase_fence_table(
        &env,
        "legacy_refresh_input_fences",
        "Legacy_Refresh_Input_Fences",
    );
    let refused = env.run();
    assert!(
        matches!(
            refused,
            Err(CredentialMigrationError::Store(
                StoreError::CredentialPairInvalid
            ))
        ),
        "CT53.noncanonical refused with CredentialPairInvalid: {refused:?}"
    );
    assert_eq!(
        env.count("SELECT COUNT(*) FROM legacy_owner_tokens"),
        1,
        "CT53.noncanonical nothing dropped"
    );
    assert!(
        env.listing().contains(&REL_K1.to_owned()),
        "CT53.noncanonical no key removed"
    );
    // The sign-in that discards a blocked slot (R78) is V2's (pending).
}

// ---------------------------------------------------------------- CT54, CT55

fn decrypt_attempts(slot: CredentialSlot) -> Vec<Vec<u8>> {
    seam::with(|s| {
        s.decrypt_log
            .iter()
            .filter(|(l, _)| *l == slot)
            .map(|(_, k)| k.clone())
            .collect()
    })
}

fn removal_then_barrier(case: &str) {
    let events = seam::with(|s| s.events.clone());
    let last_removal = events.iter().rposition(|e| e.starts_with("remove:"));
    let barrier = events.iter().rposition(|e| e == "directory_barrier");
    assert!(
        matches!((last_removal, barrier), (Some(r), Some(b)) if r < b),
        "{case}: the directory barrier is recorded after the removals: {:?}",
        alias_view(&events)
    );
}

#[test]
fn ct054_candidate_matrix() {
    // Legacy candidates in R45's order, each alone and in every pair and triple.
    let owner_names = [REL_K1, RST_K1, REL_K2];
    for mask in 1u8..8 {
        let present: Vec<&str> = owner_names
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, n)| *n)
            .collect();
        // Equal contents: one decryption attempt per distinct content.
        {
            seam::reset();
            let key = key32(11);
            let row = PriorRow::fake(&key, "access-0001", "refresh-0001", Some(uuid_n(0xaa)));
            let mut env = Env::hand_built(Some(&row), None, false);
            for name in &present {
                env.put(name, &key);
            }
            env.run()
                .unwrap_or_else(|e| panic!("CT54.owner.{present:?}.equal failed: {e:?}"));
            assert_eq!(
                decrypt_attempts(CredentialSlot::OwnerApi),
                vec![key.clone()],
                "CT54.owner.{present:?}.equal one attempt"
            );
            assert!(
                is_complete(&env.state()),
                "CT54.owner.{present:?}.equal complete"
            );
            assert!(
                env.listing()
                    .iter()
                    .all(|n| !NEUTRAL_FILES.contains(&n.as_str())),
                "CT54.owner.{present:?}.equal every candidate removed"
            );
            removal_then_barrier(&format!("CT54.owner.{present:?}.equal"));
        }
        // Different contents: each present candidate in turn is the one that authenticates.
        for (position, winner) in present.iter().enumerate() {
            seam::reset();
            let keys: Vec<Vec<u8>> = (0..present.len()).map(|i| key32(20 + i as u8)).collect();
            let row = PriorRow::fake(
                &keys[position],
                "access-0001",
                "refresh-0001",
                Some(uuid_n(0xaa)),
            );
            let mut env = Env::hand_built(Some(&row), None, false);
            for (name, key) in present.iter().zip(&keys) {
                env.put(name, key);
            }
            env.run()
                .unwrap_or_else(|e| panic!("CT54.owner.{present:?}.winner={winner} failed: {e:?}"));
            assert_eq!(
                decrypt_attempts(CredentialSlot::OwnerApi),
                keys[..=position].to_vec(),
                "CT54.owner.{present:?}.winner={winner} attempts follow R45's order up to the winner"
            );
            assert_eq!(
                env.plaintexts(CredentialSlot::OwnerApi).1,
                b"refresh-0001",
                "CT54.owner.{present:?}.winner={winner} converts"
            );
            assert!(
                env.listing()
                    .iter()
                    .all(|n| !NEUTRAL_FILES.contains(&n.as_str())),
                "CT54.owner.{present:?}.winner={winner} all removed"
            );
            removal_then_barrier(&format!("CT54.owner.{present:?}.winner={winner}"));
        }
    }
    // Fleet candidates OLD-K4, RST-K2, OLD-K5 in the same matrix.
    let fleet_names = [FLEET_K4, RST_K2, FLEET_K5];
    for mask in 1u8..8 {
        let present: Vec<&str> = fleet_names
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, n)| *n)
            .collect();
        for (position, winner) in present.iter().enumerate() {
            seam::reset();
            let keys: Vec<Vec<u8>> = (0..present.len()).map(|i| key32(40 + i as u8)).collect();
            let row = PriorRow::fake(&keys[position], "access-0002", "refresh-0002", None);
            let mut env = Env::hand_built(None, Some(&row), false);
            for (name, key) in present.iter().zip(&keys) {
                env.put(name, key);
            }
            env.run()
                .unwrap_or_else(|e| panic!("CT54.fleet.{present:?}.winner={winner} failed: {e:?}"));
            assert_eq!(
                decrypt_attempts(CredentialSlot::FleetApi),
                keys[..=position].to_vec(),
                "CT54.fleet.{present:?}.winner={winner} attempts"
            );
            assert!(
                env.listing()
                    .iter()
                    .all(|n| !NEUTRAL_FILES.contains(&n.as_str())),
                "CT54.fleet.{present:?}.winner={winner} all removed"
            );
        }
    }
    // OLD-K7 comes last: a wrong file candidate is tried first, then the
    // cursor-derived key.
    seam::reset();
    let cursor = crate::protocol::CursorKey::from_bytes([5; 32]);
    let k7 = cursor.fleet_credential_encryption_key().to_vec();
    let row = PriorRow::fake(&k7, "access-0002", "refresh-0002", None);
    let mut env = Env::hand_built(None, Some(&row), false);
    env.cursor = Some(cursor);
    env.put(FLEET_K4, &key32(77));
    env.run()
        .expect("CT54.fleet.k7 converts through the cursor-derived key");
    assert_eq!(
        decrypt_attempts(CredentialSlot::FleetApi),
        vec![key32(77), k7],
        "CT54.fleet.k7 order"
    );
    // With old names also present (stand-ins) every result is the same and no
    // operation is recorded on them (R80).
    seam::reset();
    let key = key32(11);
    let row = PriorRow::fake(&key, "access-0001", "refresh-0001", Some(uuid_n(0xaa)));
    let mut env = Env::hand_built(Some(&row), None, false);
    env.put(REL_K1, &key);
    let stand_ins = [
        stand_in_name("a.key"),
        stand_in_name("b.key"),
        stand_in_name("c.tmp"),
    ];
    for name in &stand_ins {
        env.put(name, &key32(66));
    }
    env.run()
        .expect("CT54.prior_names converts with stand-ins present");
    assert_eq!(
        decrypt_attempts(CredentialSlot::OwnerApi),
        vec![key],
        "CT54.prior_names attempts unchanged"
    );
    for (index, name) in stand_ins.iter().enumerate() {
        assert!(
            env.listing().contains(name),
            "CT54.prior_names stand-in #{index} stays"
        );
        assert!(
            seam::with(|s| s.events.iter().all(|e| !(e.starts_with("inspect:")
                || e.starts_with("remove:")
                || e.starts_with("read:"))
                || !e.contains(name))),
            "CT54.prior_names no operation on stand-in #{index} (alias set OLD-K1, OLD-K2, OLD-K3)"
        );
    }
}

#[test]
fn ct055_complete_store_leftovers() {
    // Case all_files: a complete store with every neutral file present; all are
    // removed at the next admitted open, the state is unchanged and the
    // directory barrier follows the removals.
    seam::reset();
    let mut env = Env::fresh();
    let stand_in = stand_in_name("kept.key");
    for name in NEUTRAL_FILES {
        env.put(name, &key32(3));
    }
    env.put(&stand_in, &key32(4));
    let before = format!("{:?}", env.state());
    let observer = Connection::open(env.data().join("hub.sqlite")).unwrap();
    let data_version_before: i64 = observer
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    seam::with(|s| s.events.clear());
    env.run()
        .expect("CT55.all_files Phase B on a complete store");
    for write in [
        "conversion_commit",
        "conversion_barrier",
        "vacuum",
        "purge_commit",
        "scrub_marker",
        "files_marker",
        "complete_marker",
    ] {
        assert!(
            events_matching(write).is_empty(),
            "CT55.all_files a complete store records no {write} step"
        );
    }
    assert!(
        env.listing() == vec![stand_in.clone()],
        "CT55.all_files only the old-name stand-in (alias OLD-K1) remains"
    );
    assert_eq!(
        format!("{:?}", env.state()),
        before,
        "CT55.all_files state otherwise unchanged"
    );
    let data_version_after: i64 = observer
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        data_version_before, data_version_after,
        "CT55.all_files no catalogue write"
    );
    removal_then_barrier("CT55.all_files");
    assert!(
        seam::with(|s| s
            .events
            .iter()
            .all(|e| !e.contains(&stand_in) || e.starts_with("list_returned:"))),
        "CT55.all_files the stand-in is only ever listed (R80)"
    );

    // Case restored_into_migrated_hub and relocated_after_completion.
    for (case, name) in [
        ("restored_fleet_member", RST_K2),
        ("relocated_after_completion", REL_K1),
    ] {
        seam::reset();
        let mut env = Env::fresh();
        env.put(name, &key32(5));
        env.run()
            .unwrap_or_else(|e| panic!("CT55.{case} failed: {e:?}"));
        assert!(env.listing().is_empty(), "CT55.{case} removed");
        removal_then_barrier(&format!("CT55.{case}"));
    }

    // Case leftover_waits_for_barrier (R49): in a complete store a leftover
    // candidate is removed only after a durability barrier has passed; a
    // failing barrier leaves the file in place and the retry removes it.
    seam::reset();
    let mut env = Env::fresh();
    env.put(RST_K2, &key32(5));
    env.conn
        .as_mut()
        .unwrap()
        .set_test_barrier_failure_at(Some(1));
    assert!(
        env.run().is_err(),
        "CT55.leftover_barrier a failing barrier stops the open"
    );
    assert!(
        env.listing().contains(&RST_K2.to_owned()),
        "CT55.leftover_barrier no file is removed before the barrier passed"
    );
    env.reopen();
    env.run().expect("CT55.leftover_barrier the retry runs");
    assert!(
        env.listing().is_empty(),
        "CT55.leftover_barrier the retry removes the file"
    );

    // Case exact_staging_pattern (E58, M2P-58): only the exact per-key staging
    // name .hub-restore-<canonical lowercase UUID v4>.tmp goes; near matches stay.
    seam::reset();
    let mut env = Env::fresh();
    let exact = format!(
        ".hub-restore-{}.tmp",
        Uuid::from_u128(0x1234_5678_9abc_4def_8123_4567_89ab_cdef)
    );
    let near = [
        format!(
            ".hub-restore-{}.tmp.keep",
            Uuid::from_u128(0x1234_5678_9abc_4def_8123_4567_89ab_cdee)
        ),
        ".hub-restore-".to_owned(),
        ".hub-restore-not-a-uuid.tmp".to_owned(),
        format!(
            "x.hub-restore-{}.tmp",
            Uuid::from_u128(0x1234_5678_9abc_4def_8123_4567_89ab_cdf0)
        ),
        format!(
            ".hub-restore-{}.tmp",
            Uuid::from_u128(0x1234_5678_9abc_1def_8123_4567_89ab_cdf1)
        ),
    ];
    env.put(&exact, &key32(6));
    for name in &near {
        env.put(name, &key32(6));
    }
    env.run().expect("CT55.exact_staging_pattern");
    let listing = env.listing();
    assert!(
        !listing.contains(&exact),
        "CT55.exact_staging_pattern the exact pattern is removed"
    );
    for name in &near {
        assert!(
            listing.contains(name),
            "CT55.exact_staging_pattern near match {name} stays"
        );
    }
    removal_then_barrier("CT55.exact_staging_pattern");

    // Case failed_directory_barrier: the removals happened, the barrier fails;
    // old_files_removed stays 0 on a converted store and is retried.
    seam::reset();
    let mut env = crash_fixture(false);
    seam::with(|s| s.directory_barrier_fail_at = Some(1));
    let failed = env.run();
    assert!(
        failed.is_err(),
        "CT55.failed_barrier Phase B reports the failed directory barrier"
    );
    let state = env.state();
    assert!(
        !state.old_files_removed && state.completed_at_ms.is_none(),
        "CT55.failed_barrier marker not set: {state:?}"
    );
    seam::with(|s| s.directory_barrier_fail_at = None);
    env.reopen();
    env.run().expect("CT55.failed_barrier retried");
    assert!(
        is_complete(&env.state()),
        "CT55.failed_barrier completes on retry"
    );
    // The failed-probe portion of CT55 is V1's (g24_m2_v1::ct103_read_only_complete_cleanup).
}

include!("tests/cases_b.rs");
