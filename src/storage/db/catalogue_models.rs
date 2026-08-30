// SPDX-License-Identifier: AGPL-3.0-only

fn catalogue_quick_check_label(connection: &Connection) -> Result<String, StoreError> {
    let rows: Vec<String> = connection
        .prepare("PRAGMA quick_check")
        .map_err(StoreError::Query)?
        .query_map([], |row| row.get(0))
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)?;
    if rows.as_slice() == ["ok"] {
        Ok("ok".to_owned())
    } else {
        Err(StoreError::Integrity(
            rows.into_iter()
                .next()
                .unwrap_or_else(|| "failed".to_owned()),
        ))
    }
}

impl HubBackupSnapshot<'_> {
    pub(crate) fn copy_bytes(&self) -> Result<u64, StoreError> {
        self.store
            .backup_copy_bytes_with_gate(&self.publication_gate)
    }

    pub(crate) fn copy_to(&self, destination: &Path) -> Result<(), StoreError> {
        self.store
            .backup_to_with_gate(destination, &self.publication_gate)
    }
}

fn validate_private_store_directory(path: &Path) -> Result<(), ()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != SHARED_DATA_DIRECTORY_MODE
    {
        return Err(());
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct RepairReport {
    pub status: String,
    pub sqlite_integrity: String,
    pub quarantined_sessions_preserved: usize,
    pub orphaned_packs_removed: usize,
    pub freed_bytes: u64,
}

/// Bounded startup facts. Unlike `CatalogueInventory`, this intentionally
/// avoids parsing retained manifests and walking pack directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInventory {
    pub journal_mode: String,
    pub vehicles: u64,
    pub raw_observations: u64,
    pub quarantined_sessions: u64,
    pub referenced_packs: u64,
    pub teslamate_legacy_token_rows: u64,
    pub fleet_token_rows: u64,
}

/// Read-only catalogue facts used by `doctor`. Counts and PRAGMA values only;
/// this never hashes packs and never mutates tokens or TeslaMate.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CatalogueInventory {
    pub schema_version: i32,
    pub journal_mode: String,
    pub page_size: i64,
    pub page_count: i64,
    pub freelist_count: i64,
    pub sqlite_page_bytes: u64,
    pub wal_present: bool,
    pub wal_bytes: u64,
    pub synchronous: i64,
    pub foreign_keys_enabled: bool,
    pub vehicles: u64,
    pub raw_observations: u64,
    pub current_observations: u64,
    pub quarantined_sessions: u64,
    pub open_lifecycle_rows: u64,
    pub referenced_packs: u64,
    pub referenced_pack_bytes: u64,
    pub physical_pack_files: u64,
    pub physical_pack_bytes: u64,
    pub teslamate_legacy_token_rows: u64,
    pub fleet_token_rows: u64,
    pub paired_devices: u64,
    pub installation_id: Uuid,
}

fn physical_pack_inventory(packs_dir: &Path) -> Result<(u64, u64), StoreError> {
    let mut identities = HashSet::new();
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    for directory in [
        packs_dir.to_path_buf(),
        packs_dir.join("sha256"),
        packs_dir.join(".staging"),
    ] {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(StoreError::InspectCatalogPack {
                    path: directory,
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| StoreError::InspectCatalogPack {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| StoreError::InspectCatalogPack {
                    path: path.clone(),
                    source,
                })?;
            if !metadata.file_type().is_file()
                || !identities.insert((metadata.dev(), metadata.ino()))
            {
                continue;
            }
            files = files.checked_add(1).ok_or(StoreError::InvalidStoredCount)?;
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or(StoreError::InvalidStoredCount)?;
        }
    }
    Ok((files, bytes))
}

/// Reject a candidate import successor before it can participate in an
/// atomic batch. The caller still performs full typed-SQLite inspection and
/// immutable-binding verification; this helper keeps the wire-level lineage
/// invariants identical to the single-successor path.
fn validate_import_delta_successor_shape(delta: &LineageDelta) -> Result<(), StoreError> {
    delta
        .pack
        .validate(ProtocolLimits::default())
        .map_err(StoreError::Manifest)?;
    if delta.pack.snapshot_id.is_nil()
        || delta.from_sequence >= delta.to_sequence
        || delta.pack_digest != delta.pack.sha256
        || delta.pack.schema != HUB_PROJECTION_SCHEMA_V2
        || delta.pack.format != crate::protocol::PackFormat::HubProjectionSqlite
        || delta.pack.compression != crate::protocol::PackCompression::Zstd
        || delta.pack.sequence
            != (SequenceRange {
                from_exclusive: delta.from_sequence,
                to_inclusive: delta.to_sequence,
            })
        || delta.chain_digest
            != canonical_delta_chain_digest(delta.parent_chain_digest, delta.pack.sha256)
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

/// Insert exactly one already-verified typed delta without advancing the
/// lineage head. The enclosing batch finalizer advances the head only after
/// every pack, source-state replacement, lifecycle promotion, and fingerprint
/// update has succeeded in the same transaction.
fn insert_import_delta_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
    delta: &LineageDelta,
) -> Result<(), StoreError> {
    let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
    let inserted = transaction
        .execute(
            "INSERT INTO sync_deltas(
                vehicle_id, from_sequence, to_sequence, parent_chain_digest,
                chain_digest, pack_digest, pack_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(vehicle_id, from_sequence, to_sequence) DO NOTHING",
            params![
                vehicle_key,
                i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                delta.parent_chain_digest.to_string(),
                delta.chain_digest.to_string(),
                delta.pack_digest.to_string(),
                pack_json,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    if inserted != 1 {
        return Err(StoreError::LineageCatalogConflict);
    }
    HubStore::register_lineage_pack_snapshot(
        transaction,
        vehicle_key,
        &delta.pack,
        delta.to_sequence,
        &pack_json,
    )?;

    let existing_pack: Option<(String, i64, String, i64, i64)> = transaction
        .query_row(
            "SELECT snapshot_id, ordinal, relative_path,
                    compressed_bytes, uncompressed_bytes
             FROM sync_packs WHERE sha256 = ?1",
            params![delta.pack.sha256.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if let Some((snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes)) =
        existing_pack
    {
        if snapshot_id != delta.pack.snapshot_id.to_string()
            || ordinal != i64::from(delta.pack.ordinal)
            || relative_path != delta.pack.relative_path
            || compressed_bytes
                != i64::try_from(delta.pack.compressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?
            || uncompressed_bytes
                != i64::try_from(delta.pack.uncompressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        return Err(StoreError::LineageCatalogConflict);
    }
    let occupied: Option<String> = transaction
        .query_row(
            "SELECT sha256 FROM sync_packs
             WHERE snapshot_id = ?1 AND ordinal = ?2",
            params![
                delta.pack.snapshot_id.to_string(),
                i64::from(delta.pack.ordinal)
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if occupied.is_some() {
        return Err(StoreError::LineageCatalogConflict);
    }
    transaction
        .execute(
            "INSERT INTO sync_packs(
                sha256, snapshot_id, ordinal, relative_path,
                compressed_bytes, uncompressed_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                delta.pack.sha256.to_string(),
                delta.pack.snapshot_id.to_string(),
                i64::from(delta.pack.ordinal),
                delta.pack.relative_path,
                i64::try_from(delta.pack.compressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?,
                i64::try_from(delta.pack.uncompressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    Ok(())
}

fn load_json_rows<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    vehicle_id: &str,
) -> Result<Vec<T>, StoreError> {
    let mut statement = connection.prepare(sql).map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id], |row| row.get::<_, String>(0))
        .map_err(StoreError::Query)?;
    let mut values = Vec::new();
    for row in rows {
        let json = row.map_err(StoreError::Query)?;
        values.push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
    }
    Ok(values)
}
