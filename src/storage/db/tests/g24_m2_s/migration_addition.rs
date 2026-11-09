// SPDX-License-Identifier: AGPL-3.0-only
use super::*;

fn configure_test_memory_temp_store(connection: &Connection) {
    connection.execute_batch("PRAGMA temp_store=MEMORY").expect(
        "SETUP-TEMP-STORE-OK fixture-owned SQLite handle accepts in-memory temporary storage",
    );
    let temp_store: i64 = connection
        .query_row("PRAGMA temp_store", [], |row| row.get(0))
        .expect("SETUP-TEMP-STORE-READ-OK fixture-owned SQLite temporary storage can be read back");
    assert_eq!(
        temp_store, 2,
        "SETUP-TEMP-STORE-01 fixture-owned SQLite temporary work stays in memory"
    );
}

fn fixture_with_memory_temp_store() -> Fixture {
    // Configuration begins after the imported constructor returns. Its early
    // and implicit HubStore connections are outside this fixture-only proof.
    let fixture = Fixture::new();
    configure_test_memory_temp_store(fixture.conn.test_connection());
    fixture
}

#[path = "migration_addition/atomic.rs"]
mod atomic;
#[path = "migration_addition/cleanup.rs"]
mod cleanup;
#[path = "migration_addition/readers.rs"]
mod readers;
#[path = "migration_addition/snapshot_admission.rs"]
mod snapshot_admission;

const PREDECESSOR_TABLES: [&str; 6] = [
    "legacy_owner_tokens",
    "fleet_tokens",
    "legacy_refresh_receipt_bindings",
    "legacy_refresh_input_fences",
    "fleet_refresh_receipt_bindings",
    "fleet_refresh_input_fences",
];

const STATE_ENUM_COLUMNS: [(&str, &str); 6] = [
    ("owner_api_state", "pending"),
    ("fleet_api_state", "pending"),
    ("owner_api_blocked", "old_key_unavailable"),
    ("fleet_api_blocked", "old_pair_invalid"),
    ("secrets_exclusion", "verified"),
    ("durability", "ok"),
];

fn enum_state_fixture() -> Fixture {
    let f = migration_fixture(true, true, false);
    f.conn
        .test_connection()
        .execute(
            "UPDATE credential_format_state SET owner_api_blocked='old_key_unavailable',
         fleet_api_blocked='old_pair_invalid',secrets_exclusion='verified',
         durability='ok',durability_probed_at_ms=?1,started_at_ms=?1",
            [T0_MS],
        )
        .expect("synthetic enum baseline has valid pending slots, reasons and diagnostics");
    f
}

// SQLite STRICT/NOT NULL normally prevents constructing these corrupt values.
// This own-temp fixture keeps the published column names and other declared
// types, but makes only the enum columns ANY/nullable to exercise read refusal.
fn allow_malformed_enum_fixture_values(f: &Fixture) {
    let db = f.conn.test_connection();
    let columns: Vec<(String, String)> = db
        .prepare("PRAGMA table_info(credential_format_state)")
        .unwrap()
        .query_map([], |row| Ok((row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let definition = columns
        .iter()
        .map(|(name, declared)| {
            let kind = if STATE_ENUM_COLUMNS
                .iter()
                .any(|(column, _)| *column == name.as_str())
            {
                "ANY"
            } else {
                declared.as_str()
            };
            format!("\"{}\" {kind}", name.replace('"', "\"\""))
        })
        .collect::<Vec<_>>()
        .join(",");
    db.execute_batch(&format!(
        "BEGIN IMMEDIATE; CREATE TABLE synthetic_enum_state ({definition}) STRICT;
         INSERT INTO synthetic_enum_state SELECT * FROM credential_format_state;
         DROP TABLE credential_format_state;
         ALTER TABLE synthetic_enum_state RENAME TO credential_format_state; COMMIT;"
    ))
    .expect("construct only a synthetic malformed-state fixture, never a production schema");
}

fn set_state_text_for_refusal(f: &Fixture, column: &str, value: &str) {
    assert!(
        STATE_ENUM_COLUMNS.iter().any(|(name, _)| *name == column),
        "SETUP-ENUM-01 only the six published enum fields are malformed by this fixture"
    );
    let db = f.conn.test_connection();
    db.pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    db.execute(
        &format!("UPDATE credential_format_state SET {column}=?1"),
        [value],
    )
    .unwrap();
    db.pragma_update(None, "ignore_check_constraints", false)
        .unwrap();
}

// These synthetic shapes come solely from the gated A3664..A3669 facts. CHECK
// clauses are deliberately not the oracle: the readers must validate values.
fn predecessor_ddl(index: usize, equivalent: bool) -> String {
    let integer = if equivalent { "integer" } else { "INTEGER" };
    match index {
        0 => format!(
            "CREATE TABLE legacy_owner_tokens (
             singleton_id {integer} PRIMARY KEY,
             access BLOB NOT NULL, refresh BLOB NOT NULL,
             expires_at INTEGER NOT NULL, next_refresh_at INTEGER NOT NULL,
             credential_generation TEXT) STRICT;"
        ),
        1 => format!(
            "CREATE TABLE fleet_tokens (
             singleton_id {integer} PRIMARY KEY,
             access BLOB NOT NULL, refresh BLOB NOT NULL,
             client_id TEXT NOT NULL, region TEXT NOT NULL,
             expires_at INTEGER NOT NULL, next_refresh_at INTEGER NOT NULL,
             credential_generation TEXT) STRICT;"
        ),
        2 | 4 => {
            let table = PREDECESSOR_TABLES[index];
            format!(
                "CREATE TABLE {table} (
                 receipt_id {integer} PRIMARY KEY REFERENCES outbound_request_receipts(id)
                    ON DELETE CASCADE,
                 attempt_id TEXT NOT NULL UNIQUE,
                 input_credential_generation TEXT NOT NULL,
                 output_credential_generation TEXT) STRICT;
                 CREATE UNIQUE INDEX {table}_output ON {table}(output_credential_generation)
                    WHERE output_credential_generation IS NOT NULL;"
            )
        }
        3 | 5 => format!(
            "CREATE TABLE {} (
             input_credential_generation TEXT PRIMARY KEY NOT NULL COLLATE NOCASE
             ) STRICT, WITHOUT ROWID;",
            PREDECESSOR_TABLES[index]
        ),
        _ => unreachable!("six published predecessor shapes"),
    }
}

fn create_predecessors(db: &Connection, mask: u8, equivalent: bool) {
    for index in 0..PREDECESSOR_TABLES.len() {
        if mask & (1 << index) != 0 {
            db.execute_batch(&predecessor_ddl(index, equivalent))
                .expect("create the exact synthetic predecessor metadata shape");
        }
    }
}

fn seed_predecessor_row(f: &Fixture, slot: CredentialSlot, value: Uuid) {
    let db = f.conn.test_connection();
    match slot {
        CredentialSlot::OwnerApi => {
            db.execute(
                "INSERT OR REPLACE INTO legacy_owner_tokens
                 (singleton_id,access,refresh,expires_at,next_refresh_at,credential_generation)
                 VALUES(1,?1,?2,0,0,?3)",
                params![
                    &[0xa1u8, 0xa2][..],
                    &[0xb1u8, 0xb2, 0xb3][..],
                    value.to_string()
                ],
            )
            .unwrap();
        }
        CredentialSlot::FleetApi => {
            db.execute(
                "INSERT OR REPLACE INTO fleet_tokens
                 (singleton_id,access,refresh,client_id,region,expires_at,next_refresh_at,
                  credential_generation) VALUES(1,?1,?2,'synthetic-client','eu',900,600,?3)",
                params![
                    &[0xc1u8, 0xc2, 0xc3][..],
                    &[0xd1u8, 0xd2][..],
                    value.to_string()
                ],
            )
            .unwrap();
        }
    }
}

fn migration_fixture(owner: bool, fleet: bool, fences_pending: bool) -> Fixture {
    let f = fixture_with_memory_temp_store();
    create_predecessors(f.conn.test_connection(), 63, false);
    f.conn
        .test_connection()
        .execute(
            "UPDATE credential_format_state SET owner_api_state=?1,fleet_api_state=?2,
             owner_api_blocked=NULL,fleet_api_blocked=NULL,fences_pending=?3,
             old_tables_dropped=0,purge_required=1,wal_scrubbed=0,old_files_removed=0,
             attempts_migrated=0,fences_fingerprinted=0,legacy_fences=0,
             purge_needed_bytes=NULL,secrets_exclusion=NULL,durability=NULL,
             durability_probed_at_ms=NULL,started_at_ms=NULL,converted_at_ms=NULL,
             completed_at_ms=NULL WHERE singleton_id=1",
            params![
                if owner { "pending" } else { "none" },
                if fleet { "pending" } else { "none" },
                fences_pending
            ],
        )
        .unwrap();
    if owner {
        seed_predecessor_row(&f, CredentialSlot::OwnerApi, generation(101));
    }
    if fleet {
        seed_predecessor_row(&f, CredentialSlot::FleetApi, generation(202));
    }
    f
}

fn migrated_pair(
    f: &mut Fixture,
    slot: CredentialSlot,
    n: u128,
    refresh: &str,
) -> SealedCredentialPair {
    let fleet = slot == CredentialSlot::FleetApi;
    // R46's fixture oracle is independent SQL over the joined receipt. Fleet
    // ownership is unconditional; Legacy requires a completed success.
    let refresh_owner = if fleet {
        RefreshOwner::Hub
    } else {
        let completed_success: bool = f
            .conn
            .test_connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM legacy_refresh_receipt_bindings AS binding
             JOIN outbound_request_receipts AS receipt ON receipt.id=binding.receipt_id
             WHERE binding.output_credential_generation=?1 COLLATE NOCASE
             AND receipt.outcome='success' AND receipt.completed_at_ms IS NOT NULL)",
                [generation(n).to_string()],
                |row| row.get(0),
            )
            .unwrap();
        if completed_success {
            RefreshOwner::Hub
        } else {
            RefreshOwner::External
        }
    };
    let mut pair = SealedCredentialPair {
        account_id: 1,
        slot,
        origin: fleet.then_some(CredentialOrigin::FleetOauth),
        refresh_owner,
        generation: generation(n),
        access_envelope: Vec::new(),
        refresh_envelope: Vec::new(),
        refresh_present: true,
        refresh_fingerprint: None,
        expires_at: if fleet { 900 } else { 0 },
        next_refresh_at: if fleet { 600 } else { 0 },
        client_id: fleet.then(|| "synthetic-client".to_string()),
        region: fleet.then(|| "eu".to_string()),
        migrated: true,
        written_at_ms: T0_MS,
    };
    f.conn
        .reserve_and_seal_credential_pair(
            &mut pair,
            &f.keys,
            b"synthetic-migration-access",
            refresh.as_bytes(),
            &mut f.nonces,
        )
        .expect("reserve and seal a synthetic migrated pair through accepted S/E/K");
    pair
}

fn empty_plan(now_ms: i64) -> CredentialMigrationPlan<'static> {
    CredentialMigrationPlan {
        pairs: &[],
        blocked_slots: &[],
        attempts: &[],
        legacy_fences: &[],
        refresh_fences: &[],
        now_ms,
    }
}

fn drop_c10_for_floor(f: &Fixture) {
    f.conn
        .test_connection()
        .execute_batch(
            "DROP TABLE credential_refresh_attempts;
         DROP TABLE credential_refresh_fences;
         DROP TABLE credential_legacy_fences;
         DROP TABLE credential_pairs;
         DROP TABLE credential_sign_ins;
         DROP TABLE credential_key_usage;
         DROP TABLE credential_format_state;
         PRAGMA user_version=78;",
        )
        .expect("synthetic below-rung fixture has none of the seven C10 tables");
}

fn snapshot(f: &Fixture) -> [u8; 32] {
    use rusqlite::types::ValueRef;
    let db = f.conn.test_connection();
    let mut hash = Sha256::new();
    let names: Vec<String> = db
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for name in names {
        hash.update(name.as_bytes());
        let mut statement = db
            .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
            .unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        let mut row_hashes = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let mut row_hash = Sha256::new();
            for index in 0..columns {
                match row.get_ref(index).unwrap() {
                    ValueRef::Null => row_hash.update([0]),
                    ValueRef::Integer(v) => {
                        row_hash.update([1]);
                        row_hash.update(v.to_le_bytes());
                    }
                    ValueRef::Real(v) => {
                        row_hash.update([2]);
                        row_hash.update(v.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(v) => {
                        row_hash.update([3]);
                        row_hash.update((v.len() as u64).to_le_bytes());
                        row_hash.update(v);
                    }
                    ValueRef::Blob(v) => {
                        row_hash.update([4]);
                        row_hash.update((v.len() as u64).to_le_bytes());
                        row_hash.update(v);
                    }
                }
            }
            row_hashes.push(<[u8; 32]>::from(row_hash.finalize()));
        }
        row_hashes.sort_unstable();
        hash.update((row_hashes.len() as u64).to_le_bytes());
        for row in row_hashes {
            hash.update(row);
        }
    }
    hash.finalize().into()
}

fn assert_readers_invalid(f: &mut Fixture, context: &str) {
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        let before_changes = f.conn.test_connection().total_changes();
        assert!(
            matches!(
                f.conn.read_pre_upgrade_credential_row(slot),
                Err(StoreError::CredentialPairInvalid)
            ),
            "{context}: credential row refuses inconsistent whole source"
        );
        assert_eq!(
            f.conn.test_connection().total_changes(),
            before_changes,
            "{context}: credential row refusal issues no same-value or rolled-back SQL mutation"
        );
        let before_changes = f.conn.test_connection().total_changes();
        assert!(
            matches!(
                f.conn.read_pre_upgrade_refresh_bindings(slot),
                Err(StoreError::CredentialPairInvalid)
            ),
            "{context}: bindings refuse inconsistent whole source"
        );
        assert_eq!(
            f.conn.test_connection().total_changes(),
            before_changes,
            "{context}: binding refusal issues no same-value or rolled-back SQL mutation"
        );
        let before_changes = f.conn.test_connection().total_changes();
        assert!(
            matches!(
                f.conn.read_pre_upgrade_refresh_fences(slot),
                Err(StoreError::CredentialPairInvalid)
            ),
            "{context}: fences refuse inconsistent whole source"
        );
        assert_eq!(
            f.conn.test_connection().total_changes(),
            before_changes,
            "{context}: fence refusal issues no same-value or rolled-back SQL mutation"
        );
    }
}

#[test]
fn m2p056_atomic_conversion() {
    atomic::run_atomic_conversion();
}

#[test]
fn m2p056_cleanup_transitions() {
    cleanup::run_cleanup_transitions();
}

#[test]
fn m2p057_local_readers_are_bounded() {
    readers::run_local_readers_are_bounded();
}

#[test]
fn m2p059_snapshot_admission() {
    snapshot_admission::run_snapshot_admission();
}
