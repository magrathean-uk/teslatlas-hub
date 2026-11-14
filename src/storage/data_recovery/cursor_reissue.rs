// SPDX-License-Identifier: AGPL-3.0-only

//! Restore regenerates the cursor key and re-signs the restored sync state
//! (M06-T09; the frozen Backups decision of 8 October 2026).
//!
//! A bundle never carries the cursor key, but its catalogue holds terminal
//! cursors the old key signed. Restore therefore writes a fresh key into the
//! staged tree and re-issues every stored terminal cursor under it with the
//! same claims, before the staged tree is published by one rename. The claims
//! are rebuilt from each row's own identity fields, never decoded from the old
//! cursor. Only the cursor string changes: history rows, packs, digests and
//! sequences stay byte-identical, and an old cursor never verifies again.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use uuid::Uuid;

use super::{
    DataRecoveryError, current_schema_22_noop_pairs, invalid, sha256_bytes_hex, sync_directory,
    sync_file, validate_schema_22_noop_directory,
};
use crate::{
    protocol::{
        CursorClaims, CursorKey, LineageDelta, LineageManifestV2, OpaqueCursor, PROTOCOL_V1,
        SyncManifest,
    },
    updates_delivery::SignedNoOpState,
};

/// Whether the staged catalogue holds state a cursor key signed.
fn holds_signed_sync_state(connection: &Connection) -> Result<bool, DataRecoveryError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_manifests)
                 OR EXISTS(SELECT 1 FROM sync_heads)
                 OR EXISTS(SELECT 1 FROM sync_retired_lineages)",
            [],
            |row| row.get(0),
        )
        .map_err(DataRecoveryError::CatalogueIdentity)
}

/// The claims a stored manifest's terminal cursor must carry, and that
/// cursor. `None` for a delta row, which carries no cursor.
fn manifest_cursor(bytes: &[u8]) -> Result<Option<(CursorClaims, OpaqueCursor)>, DataRecoveryError> {
    if let Ok(manifest) = serde_json::from_slice::<SyncManifest>(bytes) {
        return Ok(Some((
            CursorClaims {
                protocol: manifest.protocol,
                schema: manifest.schema,
                installation_id: manifest.installation_id,
                account_id: manifest.account_id,
                vehicle_id: manifest.vehicle_id,
                generation: manifest.generation,
                sequence: manifest.head_sequence,
            },
            manifest.terminal_cursor,
        )));
    }
    if let Ok(lineage) = serde_json::from_slice::<LineageManifestV2>(bytes) {
        return Ok(Some((lineage_claims(&lineage, lineage.head_sequence), lineage.terminal_cursor)));
    }
    if serde_json::from_slice::<LineageDelta>(bytes).is_ok() {
        return Ok(None);
    }
    Err(invalid("restored sync manifest has an unknown shape"))
}

fn lineage_claims(lineage: &LineageManifestV2, sequence: u64) -> CursorClaims {
    CursorClaims {
        protocol: PROTOCOL_V1,
        schema: lineage.schema,
        installation_id: lineage.installation_id,
        account_id: lineage.account_id,
        vehicle_id: lineage.vehicle_id,
        generation: lineage.generation,
        sequence,
    }
}

/// The claims of a head's cursor: its base manifest's identity at the head's
/// own sequence.
fn head_claims(base_manifest: &[u8], sequence: u64) -> Result<CursorClaims, DataRecoveryError> {
    if let Ok(manifest) = serde_json::from_slice::<SyncManifest>(base_manifest) {
        return Ok(CursorClaims {
            protocol: manifest.protocol,
            schema: manifest.schema,
            installation_id: manifest.installation_id,
            account_id: manifest.account_id,
            vehicle_id: manifest.vehicle_id,
            generation: manifest.generation,
            sequence,
        });
    }
    if let Ok(lineage) = serde_json::from_slice::<LineageManifestV2>(base_manifest) {
        return Ok(lineage_claims(&lineage, sequence));
    }
    Err(invalid("restored sync head has no readable base manifest"))
}

fn issue(key: &CursorKey, claims: CursorClaims) -> Result<OpaqueCursor, DataRecoveryError> {
    OpaqueCursor::issue(key, claims)
        .map_err(|error| invalid(format!("cannot re-sign a restored cursor: {error}")))
}

/// Replaces the one JSON string holding `old` with `new`. Cursors have a fixed
/// length, so every other byte keeps its place.
fn replace_cursor(
    bytes: &[u8],
    old: &OpaqueCursor,
    new: &OpaqueCursor,
) -> Result<Vec<u8>, DataRecoveryError> {
    let old = format!("\"{}\"", old.as_str());
    let new = format!("\"{}\"", new.as_str());
    if old.len() != new.len() {
        return Err(invalid("re-signed cursor length differs"));
    }
    let needle = old.as_bytes();
    let mut found = bytes
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(at, _)| at);
    let (Some(at), None) = (found.next(), found.next()) else {
        return Err(invalid("restored manifest does not hold its cursor exactly once"));
    };
    let mut replaced = bytes.to_vec();
    replaced[at..at + needle.len()].copy_from_slice(new.as_bytes());
    Ok(replaced)
}

/// Rewrites every manifest, retired-lineage and head cursor in one
/// transaction. Returns each re-signed snapshot cursor by snapshot id.
fn reissue_catalogue(
    connection: &mut Connection,
    key: &CursorKey,
) -> Result<BTreeMap<String, OpaqueCursor>, DataRecoveryError> {
    let sql = DataRecoveryError::CatalogueIdentity;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    let mut snapshot_cursors = BTreeMap::new();

    let manifests: Vec<(String, Vec<u8>)> = transaction
        .prepare("SELECT snapshot_id, manifest_json FROM sync_manifests")
        .map_err(sql)?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    for (snapshot_id, bytes) in manifests {
        let Some((claims, old)) = manifest_cursor(&bytes)? else {
            continue;
        };
        let new = issue(key, claims)?;
        let replaced = replace_cursor(&bytes, &old, &new)?;
        transaction
            .execute(
                "UPDATE sync_manifests SET manifest_json = ?1 WHERE snapshot_id = ?2",
                params![replaced, snapshot_id],
            )
            .map_err(sql)?;
        snapshot_cursors.insert(snapshot_id, new);
    }

    let retired: Vec<(i64, Vec<u8>)> = transaction
        .prepare("SELECT rowid, manifest_json FROM sync_retired_lineages")
        .map_err(sql)?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    for (rowid, bytes) in retired {
        let lineage: LineageManifestV2 = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("restored retired lineage has an unknown shape"))?;
        let new = issue(key, lineage_claims(&lineage, lineage.head_sequence))?;
        let replaced = replace_cursor(&bytes, &lineage.terminal_cursor, &new)?;
        transaction
            .execute(
                "UPDATE sync_retired_lineages SET manifest_json = ?1 WHERE rowid = ?2",
                params![replaced, rowid],
            )
            .map_err(sql)?;
    }

    let heads: Vec<(String, i64, Option<Vec<u8>>)> = transaction
        .prepare(
            "SELECT heads.vehicle_id, heads.head_sequence, manifests.manifest_json
               FROM sync_heads AS heads
               LEFT JOIN sync_manifests AS manifests
                 ON manifests.snapshot_id = heads.base_snapshot_id",
        )
        .map_err(sql)?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    for (vehicle_id, sequence, base) in heads {
        let base = base.ok_or_else(|| invalid("restored sync head has no base manifest"))?;
        let sequence =
            u64::try_from(sequence).map_err(|_| invalid("restored sync head sequence is negative"))?;
        let new = issue(key, head_claims(&base, sequence)?)?;
        // Heads hold the cursor as its JSON string, as the writers store it.
        let encoded = serde_json::to_string(&new).map_err(DataRecoveryError::Encode)?;
        transaction
            .execute(
                "UPDATE sync_heads SET terminal_cursor = ?1 WHERE vehicle_id = ?2",
                params![encoded, vehicle_id],
            )
            .map_err(sql)?;
    }
    transaction.commit().map_err(sql)?;
    Ok(snapshot_cursors)
}

/// Rewrites each schema 2.2 no-op body with its manifest's re-signed cursor
/// (temporary file and rename, inside the staged tree).
fn reissue_noops(
    root: &Path,
    snapshot_cursors: &BTreeMap<String, OpaqueCursor>,
) -> Result<(), DataRecoveryError> {
    let directory = root.join("packs").join("noop");
    for name in current_schema_22_noop_pairs(root)?.into_keys() {
        let path = directory.join(&name);
        let bytes = fs::read(&path)
            .map_err(|source| super::io_error("reading a restored no-op", &path, source))?;
        let mut noop: SignedNoOpState =
            serde_json::from_slice(&bytes).map_err(DataRecoveryError::Decode)?;
        let cursor = snapshot_cursors
            .get(&noop.snapshot_id.to_string())
            .ok_or_else(|| invalid("restored no-op has no re-signed manifest"))?;
        noop.terminal_cursor = cursor.clone();
        if let Some(witness) = noop.source_witness.as_mut() {
            witness.terminal_cursor_sha256 = sha256_bytes_hex(cursor.as_str().as_bytes());
        }
        let rewritten = serde_json::to_vec(&noop).map_err(DataRecoveryError::Encode)?;
        let mode = fs::symlink_metadata(&path)
            .map_err(|source| super::io_error("inspecting a restored no-op", &path, source))?
            .permissions()
            .mode()
            & 0o7777;
        let temporary = directory.join(format!(".{name}.{}.tmp", Uuid::new_v4()));
        let write = || -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&temporary)?;
            file.set_permissions(fs::Permissions::from_mode(mode))?;
            file.write_all(&rewritten)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
        };
        if let Err(source) = write() {
            let _ = fs::remove_file(&temporary);
            return Err(super::io_error("re-signing a restored no-op", &path, source));
        }
    }
    if directory.exists() {
        sync_directory(&directory)?;
    }
    Ok(())
}

/// Every stored cursor verifies under `key` with the claims its row states.
fn verify_catalogue(connection: &Connection, key: &CursorKey) -> Result<(), DataRecoveryError> {
    let sql = DataRecoveryError::CatalogueIdentity;
    let check = |claims: CursorClaims, cursor: &OpaqueCursor| -> Result<(), DataRecoveryError> {
        match cursor.verify(key) {
            Ok(found) if found == claims => Ok(()),
            _ => Err(invalid("a restored cursor is not signed by the new cursor key")),
        }
    };
    let rows: Vec<Vec<u8>> = connection
        .prepare(
            "SELECT manifest_json FROM sync_manifests
             UNION ALL SELECT manifest_json FROM sync_retired_lineages",
        )
        .map_err(sql)?
        .query_map([], |row| row.get(0))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    for bytes in rows {
        if let Some((claims, cursor)) = manifest_cursor(&bytes)? {
            check(claims, &cursor)?;
        }
    }
    let heads: Vec<(i64, String, Option<Vec<u8>>)> = connection
        .prepare(
            "SELECT heads.head_sequence, heads.terminal_cursor, manifests.manifest_json
               FROM sync_heads AS heads
               LEFT JOIN sync_manifests AS manifests
                 ON manifests.snapshot_id = heads.base_snapshot_id",
        )
        .map_err(sql)?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    for (sequence, cursor, base) in heads {
        let base = base.ok_or_else(|| invalid("restored sync head has no base manifest"))?;
        let sequence =
            u64::try_from(sequence).map_err(|_| invalid("restored sync head sequence is negative"))?;
        let cursor: OpaqueCursor =
            serde_json::from_str(&cursor).map_err(DataRecoveryError::Decode)?;
        check(head_claims(&base, sequence)?, &cursor)?;
    }
    Ok(())
}

/// Regenerates the cursor key in the staged tree `root` and re-signs its sync
/// state, when the catalogue holds any. Runs after the tree was verified
/// against the bundle and sealed, and before it is published: an interrupted
/// run leaves only the staging directory, never a published tree whose
/// signed rows lack their key.
pub(super) fn regenerate_cursor_key(root: &Path) -> Result<bool, DataRecoveryError> {
    let catalogue = root.join("hub.sqlite");
    let mut connection = Connection::open_with_flags(
        &catalogue,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(DataRecoveryError::CatalogueIdentity)?;
    if !holds_signed_sync_state(&connection)? {
        return Ok(false);
    }
    // The staged tree carries no key from the bundle; one already present
    // would mean the tree is not what was verified.
    let key_path = crate::legacy_owner_credentials::cursor_key_path(root);
    if fs::symlink_metadata(root.join("secrets")).is_ok() || fs::symlink_metadata(&key_path).is_ok()
    {
        return Err(invalid("the staged restore already holds a secrets directory"));
    }
    let key = crate::legacy_owner_credentials::load_or_create_cursor_key(root)?;
    // Overwritten cursor bytes are zeroed, not left in free pages.
    connection
        .execute_batch("PRAGMA secure_delete = ON;")
        .map_err(DataRecoveryError::CatalogueIdentity)?;
    let snapshot_cursors = reissue_catalogue(&mut connection, &key)?;
    verify_catalogue(&connection, &key)?;
    drop(connection);
    sync_file(&catalogue)?;
    reissue_noops(root, &snapshot_cursors)?;
    // The no-op bodies still pair with their (re-signed) manifests.
    validate_schema_22_noop_directory(root, &current_schema_22_noop_pairs(root)?)?;
    Ok(true)
}
