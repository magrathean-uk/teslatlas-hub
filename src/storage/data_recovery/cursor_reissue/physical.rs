// SPDX-License-Identifier: AGPL-3.0-only

//! Restore re-keys the physical sync state (M06-T16).
//!
//! A physical admission stores its manifest with a terminal cursor the old
//! key signed, and its manifest hash and receipt ID are derived from that
//! manifest, cursor included. Re-signing the cursor therefore renames the
//! receipt, and everything bound to the receipt follows:
//!
//! - pending and retained admissions are re-signed: only the cursor string
//!   changes in the manifest; the manifest hash and receipt are derived again
//!   by the production derivation; snapshot packs and their bytes are kept;
//! - the 1.4 delta transitions are invalidated: their pack bytes carry the old
//!   receipts and signed-manifest hashes, and without them the 1.4 route
//!   answers an adjacent base with its signed `rebase_required`;
//! - the prepared months of a current (pending) head are re-issued for its
//!   renamed receipt, with a rebuilt pack and a receipt the new key signs;
//!   every other prepared month is invalidated, as no route serves it.
//!
//! Pack files are written into the staged tree first; the catalogue changes
//! in the one transaction that also re-signs the generic sync state.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, Transaction, params};

use super::{DataRecoveryError, invalid, issue, manifest_cursor, replace_cursor};
use crate::{
    db::{PendingPhysicalV3Admission, physical_v3_admission_from_manifest},
    protocol::{CursorKey, SyncManifest},
    sync::prepared_map::{
        StoredPreparedMonth, rekey_prepared_month_for_restore,
        restored_prepared_month_is_current,
    },
};

/// The tables whose rows a key signed or that name a physical receipt.
const PHYSICAL_SIGNED_TABLES: [&str; 4] = [
    "pending_physical_v3_admissions",
    "retained_physical_v3_admissions",
    "physical_v3_delta_transitions",
    "prepared_map_months",
];

/// The physical tables whose references the re-keying must keep whole.
const PHYSICAL_REFERENCE_TABLES: [&str; 4] = [
    "pending_physical_v3_packs",
    "retained_physical_v3_packs",
    "physical_v3_delta_transitions",
    "physical_v3_delta_packs",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionTable {
    Pending,
    Retained,
}

/// One admission, before and after its cursor is re-signed.
struct RekeyedAdmission {
    table: AdmissionTable,
    old: PendingPhysicalV3Admission,
    new: PendingPhysicalV3Admission,
    manifest_json: Vec<u8>,
}

/// One prepared month re-issued for its head's renamed receipt.
struct ReissuedMonth {
    stored: StoredPreparedMonth,
    artifact_id: String,
    pack_sha256: String,
    compressed_bytes: i64,
    receipt_json: Vec<u8>,
}

/// Everything the transaction writes, prepared before it begins.
pub(super) struct PhysicalRekey {
    admissions: Vec<RekeyedAdmission>,
    months: Vec<ReissuedMonth>,
}

/// Whether the staged catalogue holds physical state a key signed.
pub(super) fn holds_signed_state(connection: &Connection) -> Result<bool, DataRecoveryError> {
    let exists = PHYSICAL_SIGNED_TABLES
        .iter()
        .map(|table| format!("EXISTS(SELECT 1 FROM {table})"))
        .collect::<Vec<_>>()
        .join(" OR ");
    connection
        .query_row(&format!("SELECT {exists}"), [], |row| row.get(0))
        .map_err(DataRecoveryError::CatalogueIdentity)
}

fn admission_table_name(table: AdmissionTable) -> &'static str {
    match table {
        AdmissionTable::Pending => "pending_physical_v3_admissions",
        AdmissionTable::Retained => "retained_physical_v3_admissions",
    }
}

/// Every stored admission of `table` with its derived identity, checked
/// against the identity its row states.
fn stored_admissions(
    connection: &Connection,
    table: AdmissionTable,
) -> Result<Vec<(PendingPhysicalV3Admission, SyncManifest, Vec<u8>)>, DataRecoveryError> {
    let sql = DataRecoveryError::CatalogueIdentity;
    let rows: Vec<(String, String, i64, String, String, Vec<u8>)> = connection
        .prepare(&format!(
            "SELECT vehicle_id, snapshot_id, selected_car_id, receipt_id, manifest_sha256,
                    manifest_json
               FROM {} ORDER BY vehicle_id, head_sequence",
            admission_table_name(table)
        ))
        .map_err(sql)?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    let mut admissions = Vec::with_capacity(rows.len());
    for (vehicle_id, snapshot_id, selected_car_id, receipt_id, manifest_sha256, bytes) in rows {
        let manifest: SyncManifest = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("a restored physical admission has an unknown shape"))?;
        let identity = physical_v3_admission_from_manifest(&manifest, selected_car_id)?;
        if identity.vehicle_id.to_string() != vehicle_id
            || identity.snapshot_id.to_string() != snapshot_id
            || identity.receipt_id != receipt_id
            || identity.manifest_sha256.to_string() != manifest_sha256
        {
            return Err(invalid(
                "a restored physical admission does not match its stored identity",
            ));
        }
        admissions.push((identity, manifest, bytes));
    }
    Ok(admissions)
}

/// The re-signed form of every pending and retained admission.
fn rekey_admissions(
    connection: &Connection,
    key: &CursorKey,
) -> Result<Vec<RekeyedAdmission>, DataRecoveryError> {
    let mut rekeyed = Vec::new();
    for table in [AdmissionTable::Pending, AdmissionTable::Retained] {
        for (old, manifest, bytes) in stored_admissions(connection, table)? {
            let (claims, cursor) = manifest_cursor(&bytes)?
                .ok_or_else(|| invalid("a restored physical admission carries no cursor"))?;
            if cursor != manifest.terminal_cursor {
                return Err(invalid("a restored physical admission has two cursors"));
            }
            let fresh = issue(key, claims)?;
            let manifest_json = replace_cursor(&bytes, &cursor, &fresh)?;
            let resigned: SyncManifest = serde_json::from_slice(&manifest_json)
                .map_err(DataRecoveryError::Decode)?;
            let new = physical_v3_admission_from_manifest(&resigned, old.selected_car_id)?;
            rekeyed.push(RekeyedAdmission {
                table,
                old,
                new,
                manifest_json,
            });
        }
    }
    Ok(rekeyed)
}

fn stored_prepared_months(
    connection: &Connection,
) -> Result<Vec<StoredPreparedMonth>, DataRecoveryError> {
    let sql = DataRecoveryError::CatalogueIdentity;
    connection
        .prepare(
            "SELECT artifact_id, vehicle_id, month, input_manifest_id, input_receipt_id,
                    input_sequence, source_digest, pack_sha256, compressed_bytes,
                    CASE WHEN length(receipt_json) <= 2097152 THEN receipt_json END
               FROM prepared_map_months ORDER BY artifact_id",
        )
        .map_err(sql)?
        .query_map([], |row| {
            Ok(StoredPreparedMonth {
                artifact_id: row.get(0)?,
                vehicle_id: row.get(1)?,
                month: row.get(2)?,
                input_manifest_id: row.get(3)?,
                input_receipt_id: row.get(4)?,
                input_sequence: row.get(5)?,
                source_digest: row.get(6)?,
                pack_sha256: row.get(7)?,
                compressed_bytes: row.get(8)?,
                receipt_json: row.get(9)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)
}

/// The staged tree's prepared scratch directory, and whether this run made it.
fn scratch_directory(root: &Path) -> Result<(PathBuf, bool), DataRecoveryError> {
    let path = root.join("packs").join(".staging");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok((path, false)),
        Ok(_) => Err(invalid("the staged prepared scratch path is not a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&path)
                .map_err(|source| super::super::io_error("creating prepared scratch", &path, source))?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|source| {
                super::super::io_error("securing prepared scratch", &path, source)
            })?;
            Ok((path, true))
        }
        Err(source) => Err(super::super::io_error("inspecting prepared scratch", &path, source)),
    }
}

/// Re-issues the prepared months of each current head into the staged tree.
/// A month of any other head, or one that is not exactly what the producer
/// writes, is left out and so invalidated.
fn reissue_months(
    root: &Path,
    connection: &Connection,
    key: &CursorKey,
    admissions: &[RekeyedAdmission],
) -> Result<Vec<ReissuedMonth>, DataRecoveryError> {
    let current: BTreeMap<&str, &RekeyedAdmission> = admissions
        .iter()
        .filter(|admission| admission.table == AdmissionTable::Pending)
        .map(|admission| (admission.old.receipt_id.as_str(), admission))
        .collect();
    let stored = stored_prepared_months(connection)?;
    if !stored
        .iter()
        .any(|month| current.contains_key(month.input_receipt_id.as_str()))
    {
        return Ok(Vec::new());
    }
    let packs = root.join("packs");
    let (scratch, created) = scratch_directory(root)?;
    let mut months = Vec::new();
    let result = (|| -> Result<(), DataRecoveryError> {
        for month in stored {
            let Some(head) = current.get(month.input_receipt_id.as_str()) else {
                continue;
            };
            let reissued =
                rekey_prepared_month_for_restore(&packs, &scratch, key, &head.old, &head.new, &month)
                    .map_err(|error| {
                        invalid(format!("cannot re-issue a restored prepared month: {error}"))
                    })?;
            if let Some(reissued) = reissued {
                months.push(ReissuedMonth {
                    stored: month,
                    artifact_id: reissued.artifact_id,
                    pack_sha256: reissued.pack_sha256,
                    compressed_bytes: i64::try_from(reissued.compressed_bytes)
                        .map_err(|_| invalid("a re-issued prepared pack is too large"))?,
                    receipt_json: reissued.receipt_json,
                });
            }
        }
        Ok(())
    })();
    // Scratch files were removed as each month finished; a directory this
    // run made goes too, so the tree publishes as the bundle described it.
    if created {
        let _ = fs::remove_dir(&scratch);
    }
    result?;
    super::super::sync_directory(&packs.join("sha256"))?;
    Ok(months)
}

/// Everything the transaction needs, written into the staged tree first.
pub(super) fn prepare(
    root: &Path,
    connection: &Connection,
    key: &CursorKey,
) -> Result<PhysicalRekey, DataRecoveryError> {
    let admissions = rekey_admissions(connection, key)?;
    let months = reissue_months(root, connection, key, &admissions)?;
    Ok(PhysicalRekey { admissions, months })
}

/// The catalogue half, inside the restore's one re-signing transaction:
/// delta transitions out, prepared months replaced, retained then pending
/// admissions renamed, and the physical references checked whole.
pub(super) fn apply(
    transaction: &Transaction<'_>,
    rekey: &PhysicalRekey,
) -> Result<(), DataRecoveryError> {
    let sql = DataRecoveryError::CatalogueIdentity;
    transaction
        .execute_batch(
            "PRAGMA defer_foreign_keys = ON;
             DELETE FROM physical_v3_delta_packs;
             DELETE FROM physical_v3_delta_transitions;
             DELETE FROM prepared_map_months;",
        )
        .map_err(sql)?;
    for month in &rekey.months {
        transaction
            .execute(
                "INSERT INTO prepared_map_months
                 (artifact_id, vehicle_id, month, input_manifest_id, input_receipt_id,
                  input_sequence, source_digest, pack_sha256, compressed_bytes, receipt_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    month.artifact_id,
                    month.stored.vehicle_id,
                    month.stored.month,
                    month.stored.input_manifest_id,
                    rekey
                        .admissions
                        .iter()
                        .find(|admission| {
                            admission.table == AdmissionTable::Pending
                                && admission.old.receipt_id == month.stored.input_receipt_id
                        })
                        .map(|admission| admission.new.receipt_id.as_str())
                        .ok_or_else(|| invalid("a re-issued prepared month lost its head"))?,
                    month.stored.input_sequence,
                    month.stored.source_digest,
                    month.pack_sha256,
                    month.compressed_bytes,
                    month.receipt_json,
                ],
            )
            .map_err(sql)?;
    }
    for admission in &rekey.admissions {
        let changed = match admission.table {
            AdmissionTable::Retained => {
                transaction
                    .execute(
                        "UPDATE retained_physical_v3_packs SET receipt_id = ?1
                          WHERE receipt_id = ?2",
                        params![admission.new.receipt_id, admission.old.receipt_id],
                    )
                    .map_err(sql)?;
                transaction
                    .execute(
                        "UPDATE retained_physical_v3_admissions
                            SET manifest_json = ?1, manifest_sha256 = ?2, receipt_id = ?3
                          WHERE receipt_id = ?4 AND vehicle_id = ?5 AND snapshot_id = ?6",
                        params![
                            admission.manifest_json,
                            admission.new.manifest_sha256.to_string(),
                            admission.new.receipt_id,
                            admission.old.receipt_id,
                            admission.old.vehicle_id.to_string(),
                            admission.old.snapshot_id.to_string(),
                        ],
                    )
                    .map_err(sql)?
            }
            AdmissionTable::Pending => transaction
                .execute(
                    "UPDATE pending_physical_v3_admissions
                        SET manifest_json = ?1, manifest_sha256 = ?2, receipt_id = ?3
                      WHERE receipt_id = ?4 AND vehicle_id = ?5 AND snapshot_id = ?6",
                    params![
                        admission.manifest_json,
                        admission.new.manifest_sha256.to_string(),
                        admission.new.receipt_id,
                        admission.old.receipt_id,
                        admission.old.vehicle_id.to_string(),
                        admission.old.snapshot_id.to_string(),
                    ],
                )
                .map_err(sql)?,
        };
        if changed != 1 {
            return Err(invalid("a restored physical admission changed while re-keying"));
        }
    }
    for table in PHYSICAL_REFERENCE_TABLES {
        let violations = transaction
            .prepare(&format!("PRAGMA foreign_key_check({table})"))
            .map_err(sql)?
            .query_map([], |_| Ok(()))
            .map_err(sql)?
            .count();
        if violations != 0 {
            return Err(invalid("re-keying left a physical reference without its row"));
        }
    }
    Ok(())
}

/// After the commit: every physical cursor verifies under `key` with the
/// claims its manifest states, every stored identity is the production
/// derivation of its re-signed manifest, no delta transition is left, and
/// every prepared month is a current head's, verified as delivery checks it.
pub(super) fn verify(
    root: &Path,
    connection: &Connection,
    key: &CursorKey,
) -> Result<(), DataRecoveryError> {
    let mut current = Vec::new();
    for table in [AdmissionTable::Pending, AdmissionTable::Retained] {
        for (identity, manifest, bytes) in stored_admissions(connection, table)? {
            let (claims, cursor) = manifest_cursor(&bytes)?
                .ok_or_else(|| invalid("a restored physical admission carries no cursor"))?;
            if cursor != manifest.terminal_cursor
                || !matches!(cursor.verify(key), Ok(found) if found == claims)
            {
                return Err(invalid(
                    "a restored physical cursor is not signed by the new cursor key",
                ));
            }
            if table == AdmissionTable::Pending {
                current.push(identity.receipt_id);
            }
        }
    }
    let transitions: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM physical_v3_delta_transitions)
                  + (SELECT COUNT(*) FROM physical_v3_delta_packs)",
            [],
            |row| row.get(0),
        )
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    if transitions != 0 {
        return Err(invalid("a restored delta transition kept its old signature"));
    }
    let packs = root.join("packs");
    for month in stored_prepared_months(connection)? {
        let verified = current.contains(&month.input_receipt_id)
            && restored_prepared_month_is_current(&packs, key, &month).map_err(|error| {
                invalid(format!("cannot verify a restored prepared month: {error}"))
            })?;
        if !verified {
            return Err(invalid(
                "a restored prepared month is not signed by the new cursor key",
            ));
        }
    }
    Ok(())
}
