// SPDX-License-Identifier: AGPL-3.0-only

fn schema_version(connection: &Connection) -> Result<i32, StoreError> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(StoreError::Query)
}

fn read_only_count(connection: &Connection, sql: &'static str) -> Result<u64, StoreError> {
    let count: i64 = connection
        .query_row(sql, [], |row| row.get(0))
        .map_err(StoreError::Query)?;
    u64::try_from(count).map_err(|_| StoreError::InvalidStoredCount)
}

fn referenced_pack_rows_at(
    connection: &Connection,
    retired_expiry_cutoff_ms: i64,
) -> Result<Vec<(String, String, i64)>, StoreError> {
    let mut rows = {
        let mut statement = connection
            .prepare("SELECT sha256, relative_path, compressed_bytes FROM sync_packs")
            .map_err(StoreError::Query)?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?
    };
    rows.extend(pending_physical_v3_pack_rows(connection)?);
    rows.extend(retained_physical_v3_pack_rows(
        connection,
        retired_expiry_cutoff_ms,
    )?);
    rows.extend(prepared_map_pack_rows(connection, retired_expiry_cutoff_ms)?);
    rows.extend(physical_delta_pack_rows(connection, retired_expiry_cutoff_ms)?);
    let retired_rows = {
        let mut statement = connection
            .prepare(
                "SELECT lineage.vehicle_id, lineage.head_digest, lineage.manifest_json,
                        packs.pack_digest, packs.relative_path, packs.compressed_bytes
                   FROM sync_retired_lineage_packs AS packs
                   JOIN sync_retired_lineages AS lineage
                     ON lineage.vehicle_id = packs.vehicle_id
                    AND lineage.head_digest = packs.head_digest
                  WHERE lineage.expires_at_ms > ?1
                  ORDER BY packs.pack_digest, lineage.vehicle_id, lineage.head_digest",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map(params![retired_expiry_cutoff_ms], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?
    };
    for (vehicle_id, head_digest, manifest_json, pack_digest, relative_path, compressed_bytes) in
        retired_rows
    {
        validate_retired_lineage_pack_binding(
            &vehicle_id,
            &head_digest,
            &manifest_json,
            &pack_digest,
            &relative_path,
            compressed_bytes,
        )?;
        rows.push((pack_digest, relative_path, compressed_bytes));
    }
    rows.sort_unstable();
    let mut deduplicated: Vec<(String, String, i64)> = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(existing) = deduplicated.last()
            && existing.0 == row.0
        {
            if existing != &row {
                return Err(StoreError::LineageCatalogConflict);
            }
            continue;
        }
        deduplicated.push(row);
    }
    Ok(deduplicated)
}

/// Delete the retention rows that expired before `cutoff_ms`. Shared by repair
/// and storage reclamation; each runs it after its file removal, so the rows
/// stay retryable when a removal fails.
fn delete_expired_pack_retention_rows(
    transaction: &Transaction<'_>,
    cutoff_ms: i64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "DELETE FROM sync_retired_lineages WHERE expires_at_ms <= ?1",
            params![cutoff_ms],
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "DELETE FROM prepared_map_months AS p
                 WHERE NOT EXISTS (
                     SELECT 1 FROM pending_physical_v3_admissions AS h
                     WHERE h.vehicle_id = p.vehicle_id
                       AND h.receipt_id = p.input_receipt_id
                       AND h.serve_state = 'public_first'
                 ) AND NOT EXISTS (
                     SELECT 1 FROM retained_physical_v3_admissions AS r
                     WHERE r.vehicle_id = p.vehicle_id
                       AND r.receipt_id = p.input_receipt_id
                       AND r.expires_at_ms > ?1
                 )",
            params![cutoff_ms],
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "DELETE FROM retained_physical_v3_admissions WHERE expires_at_ms <= ?1",
            params![cutoff_ms],
        )
        .map_err(StoreError::LineageCatalog)?;
    Ok(())
}

/// One class of pack files in a storage census: how many files and bytes it
/// holds, and how many of them no reference, download, recovery path or
/// backup pin needs. Counts and sizes only, never a place or a name.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, PartialEq, Eq)]
pub struct StorageCensusClass {
    pub files: u64,
    pub bytes: u64,
    pub reclaimable_files: u64,
    pub reclaimable_bytes: u64,
}

impl StorageCensusClass {
    fn add(&mut self, bytes: u64, reclaimable: bool) {
        self.files += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        if reclaimable {
            self.reclaimable_files += 1;
            self.reclaimable_bytes = self.reclaimable_bytes.saturating_add(bytes);
        }
    }
}

/// The pack storage of a store by class (M08-T29): pack files under `packs`
/// and `packs/sha256`, and the import stages kept under `packs/physical-kept`.
/// A pack referenced by more than one class is counted once, in the first
/// class of this order. The current heads are the served physical heads and
/// the lineage head; the import bases are each vehicle's kept import stage
/// and its source sessions, which every hand-back republishes from. A stage
/// copy left in the kept stages' staging folder by an interrupted hand-back
/// or keep is an orphan.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, PartialEq, Eq)]
pub struct StorageCensus {
    pub current_heads: StorageCensusClass,
    pub import_bases: StorageCensusClass,
    pub retained_prior_heads: StorageCensusClass,
    pub deltas: StorageCensusClass,
    pub prepared_maps: StorageCensusClass,
    pub orphans: StorageCensusClass,
    pub total: StorageCensusClass,
}

/// The receipt of one reclamation: the census before and after, and the bytes
/// actually removed, measured at each removal.
#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
pub struct StorageReclaimReport {
    pub before: StorageCensus,
    pub reclaimed_files: u64,
    pub reclaimed_bytes: u64,
    pub after: StorageCensus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackStorageClass {
    CurrentHead,
    ImportBase,
    RetainedPriorHead,
    Delta,
    PreparedMap,
}

/// A pack-tree file, held by the opened directory that holds it and its
/// plain name there, so reclamation unlinks the entry it counted and never
/// resolves the path again.
struct PackStorageFile {
    directory: std::rc::Rc<std::os::fd::OwnedFd>,
    name: String,
    path: PathBuf,
    bytes: u64,
    class: Option<PackStorageClass>,
    needed: bool,
}

fn is_pack_object_name(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Open one directory of the pack tree without following a link: `parent` is
/// an already opened directory and `name` a plain entry of it, or with no
/// parent `path` is the packs root. A missing directory is `None`; a link or
/// any other non-directory is refused, so the census never reads, and
/// reclamation never unlinks, outside the store.
fn open_pack_tree_directory(
    parent: Option<&std::os::fd::OwnedFd>,
    name: &str,
    path: &Path,
) -> Result<Option<std::rc::Rc<std::os::fd::OwnedFd>>, StoreError> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let opened = match parent {
        Some(parent) => openat(parent, name, flags, Mode::empty()),
        None => open(path, flags, Mode::empty()),
    };
    match opened {
        Ok(descriptor) => Ok(Some(std::rc::Rc::new(descriptor))),
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(StoreError::InspectCatalogPack {
            path: path.to_path_buf(),
            source: error.into(),
        }),
    }
}

/// The regular files of an opened pack-tree directory, by UTF-8 name, with
/// their sizes. Links and other entries are left out, as is an entry that
/// vanished while it was read.
fn pack_tree_regular_files(
    directory: &std::os::fd::OwnedFd,
    path: &Path,
) -> Result<Vec<(String, u64)>, StoreError> {
    let inspect = |source: Errno| StoreError::InspectCatalogPack {
        path: path.to_path_buf(),
        source: source.into(),
    };
    let mut files = Vec::new();
    for entry in Dir::read_from(directory).map_err(inspect)? {
        let entry = entry.map_err(inspect)?;
        let Ok(name) = std::str::from_utf8(entry.file_name().to_bytes()) else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        let stat = match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => continue,
            Err(source) => return Err(inspect(source)),
        };
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            continue;
        }
        files.push((name.to_owned(), u64::try_from(stat.st_size).unwrap_or(0)));
    }
    Ok(files)
}

/// Every digest any catalogue row names, current or expired, by class.
fn pack_storage_classes(
    connection: &Connection,
) -> Result<HashMap<String, PackStorageClass>, StoreError> {
    let mut classes = HashMap::new();
    for (class, sql) in [
        (PackStorageClass::CurrentHead, "SELECT sha256 FROM pending_physical_v3_packs"),
        (PackStorageClass::CurrentHead, "SELECT sha256 FROM sync_packs"),
        (PackStorageClass::RetainedPriorHead, "SELECT sha256 FROM retained_physical_v3_packs"),
        (PackStorageClass::RetainedPriorHead, "SELECT pack_digest FROM sync_retired_lineage_packs"),
        (PackStorageClass::Delta, "SELECT sha256 FROM physical_v3_delta_packs"),
        (PackStorageClass::PreparedMap, "SELECT pack_sha256 FROM prepared_map_months"),
    ] {
        let digests = connection
            .prepare(sql)
            .map_err(StoreError::Query)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        for digest in digests {
            classes.entry(digest).or_insert(class);
        }
    }
    Ok(classes)
}

/// The pack files under the two pack directories, with what the catalogue
/// says of each. A file is needed when repair's reference set holds it (the
/// current heads, import bases, unexpired retention plus the delete grace that
/// covers a download in progress, and a blocked rotation's predecessor) or a
/// live backup pins it. Only regular files with a content-addressed name are
/// counted; the packs' own staging files are repair's.
fn pack_storage_files(
    connection: &Connection,
    packs_dir: &Path,
    now_ms: i64,
) -> Result<Vec<PackStorageFile>, StoreError> {
    if now_ms < 0 {
        return Err(StoreError::LineageCatalogConflict);
    }
    let cutoff_ms = now_ms.saturating_sub(RETIRED_LINEAGE_PACK_DELETE_GRACE_MS);
    let mut needed = referenced_pack_rows_at(connection, cutoff_ms)?
        .into_iter()
        .map(|(digest, _, _)| digest)
        .collect::<HashSet<_>>();
    needed.extend(crate::data_recovery::live_backup_pack_pins(connection, now_ms)?);
    let classes = pack_storage_classes(connection)?;
    let mut files = Vec::new();
    let Some(root) = open_pack_tree_directory(None, "", packs_dir)? else {
        return Ok(files);
    };
    let objects = packs_dir.join("sha256");
    let directories = [
        Some((root.clone(), packs_dir.to_path_buf())),
        open_pack_tree_directory(Some(&root), "sha256", &objects)?
            .map(|directory| (directory, objects)),
    ];
    for (directory, path) in directories.into_iter().flatten() {
        for (name, bytes) in pack_tree_regular_files(&directory, &path)? {
            let Some(digest) = name
                .strip_suffix(".sqlite.zst")
                .filter(|digest| is_pack_object_name(digest))
                .map(str::to_owned)
            else {
                continue;
            };
            files.push(PackStorageFile {
                directory: directory.clone(),
                path: path.join(&name),
                name,
                bytes,
                class: classes.get(&digest).copied(),
                needed: needed.contains(&digest),
            });
        }
    }
    files.extend(kept_stage_files(connection, &root, packs_dir)?);
    Ok(files)
}

/// The folder under the packs directory that holds the kept import stages
/// (the importer's `PHYSICAL_KEPT_DIRECTORY`).
const KEPT_STAGES_DIRECTORY: &str = "physical-kept";

/// The kept import stages (`.staging/<vehicle>.sqlite`) and their source
/// sessions (`<vehicle>.source.json`). A file named for a vehicle of this
/// store is an import base and needed; any other stage file with a
/// content-free UUID name is a copy no record names, which only a run that
/// holds the publication gate writes, so it is an orphan once the gate is
/// held. Files of any other shape are not counted.
fn kept_stage_files(
    connection: &Connection,
    root: &std::os::fd::OwnedFd,
    packs_dir: &Path,
) -> Result<Vec<PackStorageFile>, StoreError> {
    let vehicles = connection
        .prepare("SELECT vehicle_id FROM vehicles")
        .map_err(StoreError::Query)?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(StoreError::Query)?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(StoreError::Query)?;
    let mut files = Vec::new();
    let kept_path = packs_dir.join(KEPT_STAGES_DIRECTORY);
    let Some(kept) = open_pack_tree_directory(Some(root), KEPT_STAGES_DIRECTORY, &kept_path)?
    else {
        return Ok(files);
    };
    let staging_path = kept_path.join(".staging");
    let directories = [
        open_pack_tree_directory(Some(&kept), ".staging", &staging_path)?
            .map(|directory| (directory, staging_path, ".sqlite")),
        Some((kept, kept_path, ".source.json")),
    ];
    for (directory, path, suffix) in directories.into_iter().flatten() {
        for (name, bytes) in pack_tree_regular_files(&directory, &path)? {
            let Some(stem) = name
                .strip_suffix(suffix)
                .filter(|stem| stem.len() == 36 && Uuid::parse_str(stem).is_ok())
                .map(str::to_owned)
            else {
                continue;
            };
            let base = vehicles.contains(&stem);
            files.push(PackStorageFile {
                directory: directory.clone(),
                path: path.join(&name),
                name,
                bytes,
                class: base.then_some(PackStorageClass::ImportBase),
                needed: base,
            });
        }
    }
    Ok(files)
}

fn storage_census_of(files: &[PackStorageFile]) -> StorageCensus {
    let mut census = StorageCensus::default();
    for file in files {
        let reclaimable = !file.needed;
        let class = match file.class {
            Some(PackStorageClass::CurrentHead) => &mut census.current_heads,
            Some(PackStorageClass::ImportBase) => &mut census.import_bases,
            Some(PackStorageClass::RetainedPriorHead) => &mut census.retained_prior_heads,
            Some(PackStorageClass::Delta) => &mut census.deltas,
            Some(PackStorageClass::PreparedMap) => &mut census.prepared_maps,
            None => &mut census.orphans,
        };
        class.add(file.bytes, reclaimable);
        census.total.add(file.bytes, reclaimable);
    }
    census
}

impl HubStore {
    /// Count the pack files by class, and the bytes no reference, download,
    /// recovery path or backup pin needs. Read-only; it verifies no pack body.
    pub fn storage_census(&self) -> Result<StorageCensus, StoreError> {
        self.storage_census_at(retired_lineage_clock_ms()?)
    }

    fn storage_census_at(&self, now_ms: i64) -> Result<StorageCensus, StoreError> {
        let connection = self.open_read_only_connection()?;
        Ok(storage_census_of(&pack_storage_files(
            &connection,
            self.packs_dir(),
            now_ms,
        )?))
    }

    /// Remove exactly the pack files the census counts as reclaimable, then
    /// the expired retention rows. It runs under the publication gate, never
    /// at start, and verifies no pack body: what it keeps is decided by the
    /// catalogue's references alone. Each removal stands on its own and the
    /// rows go last, so a run that stops part way is finished by the next.
    pub fn reclaim_storage(&self) -> Result<StorageReclaimReport, StoreError> {
        self.reclaim_storage_at(retired_lineage_clock_ms()?)
    }

    fn reclaim_storage_at(&self, now_ms: i64) -> Result<StorageReclaimReport, StoreError> {
        let _publication_gate = self.try_acquire_publication_gate()?;
        let mut connection = self.open()?;
        let files = pack_storage_files(&connection, self.packs_dir(), now_ms)?;
        let before = storage_census_of(&files);
        let (mut reclaimed_files, mut reclaimed_bytes) = (0_u64, 0_u64);
        for file in files.iter().filter(|file| !file.needed) {
            let bytes = match statat(&*file.directory, file.name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {
                    u64::try_from(stat.st_size).unwrap_or(0)
                }
                Ok(_) | Err(Errno::NOENT) => continue,
                Err(source) => {
                    return Err(StoreError::InspectCatalogPack {
                        path: file.path.clone(),
                        source: source.into(),
                    });
                }
            };
            match unlinkat(&*file.directory, file.name.as_str(), AtFlags::empty()) {
                Ok(()) => {
                    reclaimed_files += 1;
                    reclaimed_bytes = reclaimed_bytes.saturating_add(bytes);
                }
                Err(Errno::NOENT) => {}
                Err(source) => {
                    return Err(StoreError::CleanupUnpublishedPack {
                        path: file.path.clone(),
                        source: source.into(),
                    });
                }
            }
        }
        let transaction = connection.transaction().map_err(StoreError::Query)?;
        delete_expired_pack_retention_rows(
            &transaction,
            now_ms.saturating_sub(RETIRED_LINEAGE_PACK_DELETE_GRACE_MS),
        )?;
        transaction.commit().map_err(StoreError::LineageCatalog)?;
        let after = storage_census_of(&pack_storage_files(&connection, self.packs_dir(), now_ms)?);
        Ok(StorageReclaimReport {
            before,
            reclaimed_files,
            reclaimed_bytes,
            after,
        })
    }
}

fn physical_delta_pack_rows(
    connection: &Connection,
    expiry_cutoff_ms: i64,
) -> Result<Vec<(String, String, i64)>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT packs.sha256,packs.relative_path,packs.compressed_bytes
               FROM physical_v3_delta_packs AS packs
               JOIN physical_v3_delta_transitions AS delta
                 ON delta.target_receipt_id=packs.target_receipt_id
               JOIN pending_physical_v3_admissions AS target
                 ON target.receipt_id=delta.target_receipt_id
               JOIN retained_physical_v3_admissions AS base
                 ON base.receipt_id=delta.base_receipt_id
              WHERE base.expires_at_ms > ?1",
        )
        .map_err(StoreError::Query)?;
    statement
        .query_map([expiry_cutoff_ms], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })
        .map_err(StoreError::Query)?
        .map(|row| {
            let (digest, path, bytes) = row.map_err(StoreError::Query)?;
            let parsed = digest
                .parse::<Sha256Digest>()
                .map_err(|_| StoreError::LineageCatalogConflict)?;
            if path != format!("/v1/packs/sha256/{parsed}.sqlite.zst")
                || !(1..=16 * 1024 * 1024).contains(&bytes)
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            Ok((digest, path, bytes))
        })
        .collect()
}

fn prepared_map_pack_rows(
    connection: &Connection,
    expiry_cutoff_ms: i64,
) -> Result<Vec<(String, String, i64)>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT p.pack_sha256, p.compressed_bytes FROM prepared_map_months AS p
             WHERE EXISTS (
                 SELECT 1 FROM pending_physical_v3_admissions AS h
                 WHERE h.vehicle_id = p.vehicle_id
                   AND h.receipt_id = p.input_receipt_id
                   AND h.serve_state = 'public_first'
             ) OR EXISTS (
                 SELECT 1 FROM retained_physical_v3_admissions AS r
                 WHERE r.vehicle_id = p.vehicle_id
                   AND r.receipt_id = p.input_receipt_id
                   AND r.expires_at_ms > ?1
             )",
        )
        .map_err(StoreError::Query)?;
    statement
        .query_map([expiry_cutoff_ms], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(StoreError::Query)?
        .map(|row| {
            let (digest, bytes) = row.map_err(StoreError::Query)?;
            digest
                .parse::<Sha256Digest>()
                .map_err(|_| StoreError::LineageCatalogConflict)?;
            if !(1..=16 * 1024 * 1024).contains(&bytes) {
                return Err(StoreError::LineageCatalogConflict);
            }
            let path = format!("/v1/packs/sha256/{digest}.sqlite.zst");
            Ok((digest, path, bytes))
        })
        .collect()
}

fn retained_physical_v3_pack_rows(
    connection: &Connection,
    expiry_cutoff_ms: i64,
) -> Result<Vec<(String, String, i64)>, StoreError> {
    let admissions = connection
        .prepare(
            "SELECT vehicle_id, snapshot_id, installation_id, account_id,
                    generation, selected_car_id, profile, head_sequence,
                    chunk_count, manifest_sha256, ordered_chunks_sha256,
                    receipt_id, retained_at_ms, expires_at_ms, manifest_json
               FROM retained_physical_v3_admissions
              WHERE expires_at_ms > ?1 ORDER BY vehicle_id",
        )
        .map_err(StoreError::Query)?
        .query_map(params![expiry_cutoff_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, i64>(13)?,
                row.get::<_, Vec<u8>>(14)?,
            ))
        })
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)?;
    let mut rows = Vec::new();
    for (
        vehicle_id,
        snapshot_id,
        installation_id,
        account_id,
        generation,
        selected_car_id,
        profile,
        head_sequence,
        chunk_count,
        manifest_sha256,
        ordered_chunks_sha256,
        receipt_id,
        retained_at_ms,
        expires_at_ms,
        manifest_json,
    ) in admissions
    {
        let manifest: SyncManifest = serde_json::from_slice(&manifest_json)
            .map_err(StoreError::DeserializeManifest)?;
        validate_pending_physical_v3_manifest(&manifest)?;
        let computed = physical_v3_admission_from_manifest(&manifest, selected_car_id)?;
        if profile != HUB_SYNC_V1_1_3_PROFILE
            || vehicle_id != computed.vehicle_id.to_string()
            || snapshot_id != computed.snapshot_id.to_string()
            || installation_id != computed.installation_id.to_string()
            || account_id != computed.account_id.to_string()
            || u64::try_from(generation).ok() != Some(computed.manifest.generation)
            || u64::try_from(head_sequence).ok() != Some(computed.head_sequence)
            || u32::try_from(chunk_count).ok() != Some(computed.chunk_count)
            || manifest_sha256 != computed.manifest_sha256.to_string()
            || ordered_chunks_sha256 != computed.ordered_chunks_sha256.to_string()
            || receipt_id != computed.receipt_id
            || retained_at_ms < 0
            || expires_at_ms <= retained_at_ms
        {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        let stored = connection
            .prepare(
                "SELECT ordinal, sha256, relative_path,
                        compressed_bytes, uncompressed_bytes
                   FROM retained_physical_v3_packs
                  WHERE receipt_id = ?1 ORDER BY ordinal",
            )
            .map_err(StoreError::Query)?
            .query_map([receipt_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        if stored.len() != manifest.chunks.len() {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        for (stored, pack) in stored.into_iter().zip(&manifest.chunks) {
            let expected = (
                i64::from(pack.ordinal),
                pack.sha256.to_string(),
                pack.relative_path.clone(),
                i64::try_from(pack.compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
                i64::try_from(pack.uncompressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?,
            );
            if stored != expected {
                return Err(StoreError::PhysicalV3AdmissionConflict);
            }
            rows.push((expected.1, expected.2, expected.3));
        }
    }
    Ok(rows)
}

fn pending_physical_v3_pack_rows(
    connection: &Connection,
) -> Result<Vec<(String, String, i64)>, StoreError> {
    let admissions = connection
        .prepare(
            "SELECT vehicle_id, snapshot_id, installation_id, account_id,
                    selected_car_id, profile,
                    head_sequence, chunk_count, manifest_sha256,
                    ordered_chunks_sha256, receipt_id, manifest_json
               FROM pending_physical_v3_admissions ORDER BY vehicle_id",
        )
        .map_err(StoreError::Query)?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, Vec<u8>>(11)?,
            ))
        })
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)?;
    let mut rows = Vec::new();
    for (
        vehicle_id,
        snapshot_id,
        installation_id,
        account_id,
        selected_car_id,
        profile,
        head_sequence,
        chunk_count,
        manifest_sha256,
        ordered_chunks_sha256,
        receipt_id,
        manifest_json,
    ) in admissions
    {
        let manifest: SyncManifest = serde_json::from_slice(&manifest_json)
            .map_err(StoreError::DeserializeManifest)?;
        validate_pending_physical_v3_manifest(&manifest)?;
        let computed = physical_v3_admission_from_manifest(&manifest, selected_car_id)?;
        if profile != HUB_SYNC_V1_1_3_PROFILE
            || vehicle_id != computed.vehicle_id.to_string()
            || snapshot_id != computed.snapshot_id.to_string()
            || installation_id != computed.installation_id.to_string()
            || account_id != computed.account_id.to_string()
            || u64::try_from(head_sequence).ok() != Some(computed.head_sequence)
            || u32::try_from(chunk_count).ok() != Some(computed.chunk_count)
            || manifest_sha256 != computed.manifest_sha256.to_string()
            || ordered_chunks_sha256 != computed.ordered_chunks_sha256.to_string()
            || receipt_id != computed.receipt_id
        {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        let stored = connection
            .prepare(
                "SELECT ordinal, sha256, relative_path,
                        compressed_bytes, uncompressed_bytes
                   FROM pending_physical_v3_packs
                  WHERE snapshot_id = ?1 ORDER BY ordinal",
            )
            .map_err(StoreError::Query)?
            .query_map([snapshot_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        if stored.len() != manifest.chunks.len() {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        for (stored, pack) in stored.into_iter().zip(&manifest.chunks) {
            let expected = (
                i64::from(pack.ordinal),
                pack.sha256.to_string(),
                pack.relative_path.clone(),
                i64::try_from(pack.compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
                i64::try_from(pack.uncompressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?,
            );
            if stored != expected {
                return Err(StoreError::PhysicalV3AdmissionConflict);
            }
            rows.push((expected.1, expected.2, expected.3));
        }
    }
    Ok(rows)
}

fn validate_retired_lineage_pack_binding(
    vehicle_id: &str,
    head_digest: &str,
    manifest_json: &[u8],
    pack_digest: &str,
    relative_path: &str,
    compressed_bytes: i64,
) -> Result<(), StoreError> {
    let digest = pack_digest
        .parse::<Sha256Digest>()
        .map_err(|_| StoreError::LineageCatalogConflict)?;
    let manifest: LineageManifestV2 =
        serde_json::from_slice(manifest_json).map_err(StoreError::DeserializeManifest)?;
    manifest
        .validate_with_limits(ProtocolLimits::default())
        .map_err(StoreError::Manifest)?;
    let descriptor = manifest
        .base
        .packs
        .iter()
        .chain(manifest.deltas.iter().map(|delta| &delta.pack))
        .find(|pack| pack.sha256 == digest)
        .ok_or(StoreError::LineageCatalogConflict)?;
    if vehicle_id != manifest.vehicle_id.to_string()
        || head_digest != manifest.head_digest.to_string()
        || relative_path != descriptor.relative_path
        || compressed_bytes
            != i64::try_from(descriptor.compressed_bytes)
                .map_err(|_| StoreError::PackSizeTooLarge)?
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

fn outbound_request_clock_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StoreError::OutboundRequestClock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StoreError::OutboundRequestClockOverflow)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn retired_lineage_clock_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StoreError::RetiredLineageClock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StoreError::RetiredLineageClockOverflow)
}

/// Delete the receipts that are past retention.
///
/// The receipts of the legacy credential unit's own completed refreshes that
/// lead from the last externally written pair to the stored pair are kept for
/// as long as that pair is stored (C5 section 3.3, the Owner-wire credential
/// identity). A binding is deleted with its receipt (`ON DELETE CASCADE`), and
/// the identity is read by following the completed bindings back from the
/// stored generation to the generation no binding produced, so a binding
/// pruned from the middle of that chain would make the identity jump to the
/// refresh that followed it, and C3's configuration fingerprint would change
/// about four times a day without the owner doing anything. Only the chain
/// behind the stored pair is held: once an external write stores another pair
/// the old chain is unreachable and expires with the rest. The chain grows by
/// one receipt for each refresh of the stored pair (about four a day for
/// Tesla's token lifetime), so it takes decades to use the shared budget.
fn prune_expired_outbound_request_receipts(
    transaction: &Transaction<'_>,
) -> Result<(), StoreError> {
    let cutoff_ms = outbound_request_clock_ms()?.saturating_sub(OUTBOUND_REQUEST_RETENTION_MS);
    transaction
        .execute(
            &format!(
                concat!(
                    "DELETE FROM outbound_request_receipts
              WHERE outcome <> 'started'
                AND completed_at_ms < ?1
                AND id NOT IN (
",
                    "{}",
                    "
                )"
                ),
                "WITH RECURSIVE credential_chain(slot, generation) AS (
                    SELECT slot, generation FROM credential_pairs
                     WHERE account_id = 1 AND slot = 'owner_api'
                    UNION
                    SELECT a.slot, a.input_generation
                      FROM credential_refresh_attempts a
                      JOIN credential_chain c
                        ON c.slot = a.slot AND c.generation = a.output_generation
                      JOIN outbound_request_receipts r ON r.id = a.receipt_id
                     WHERE a.slot = 'owner_api' AND r.outcome = 'success'
                )
                SELECT a.receipt_id FROM credential_refresh_attempts a
                  JOIN credential_chain c
                    ON c.slot = a.slot AND c.generation = a.output_generation
                  JOIN outbound_request_receipts r ON r.id = a.receipt_id
                 WHERE a.slot = 'owner_api' AND r.outcome = 'success'"
            ),
            params![cutoff_ms],
        )
        .map_err(StoreError::OutboundRequestReceipt)?;
    Ok(())
}

fn ensure_outbound_request_capacity(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    prune_expired_outbound_request_receipts(transaction)?;
    let count = outbound_request_capacity_consumers(transaction)?;
    if count >= MAX_OUTBOUND_REQUEST_RECEIPTS {
        return Err(StoreError::OutboundRequestAuditCapacityExhausted);
    }
    Ok(())
}

/// Every receipt consumes the same bounded audit budget.
fn outbound_request_capacity_consumers(transaction: &Transaction<'_>) -> Result<i64, StoreError> {
    transaction
        .query_row(
            "SELECT COUNT(*) FROM outbound_request_receipts",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::OutboundRequestReceipt)
}

fn prune_expired_stream_session_receipts(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    let cutoff_ms = outbound_request_clock_ms()?.saturating_sub(OUTBOUND_REQUEST_RETENTION_MS);
    transaction
        .execute(
            "DELETE FROM stream_session_receipts
             WHERE outcome <> 'started' AND completed_at_ms < ?1",
            params![cutoff_ms],
        )
        .map_err(StoreError::StreamSessionReceipt)?;
    Ok(())
}

fn ensure_stream_session_capacity(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    prune_expired_stream_session_receipts(transaction)?;
    let count: i64 = transaction
        .query_row("SELECT COUNT(*) FROM stream_session_receipts", [], |row| {
            row.get(0)
        })
        .map_err(StoreError::StreamSessionReceipt)?;
    if count >= MAX_OUTBOUND_REQUEST_RECEIPTS {
        return Err(StoreError::StreamSessionAuditCapacityExhausted);
    }
    Ok(())
}

fn invalid_outbound_request_receipt_value(index: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid outbound request receipt",
        )),
    )
}

fn receipt_from_row(row: &rusqlite::Row<'_>) -> Result<OutboundRequestReceipt, rusqlite::Error> {
    let correlation: String = row.get(1)?;
    let correlation_id =
        Uuid::parse_str(&correlation).map_err(|_| invalid_outbound_request_receipt_value(1))?;
    let transport: String = row.get(6)?;
    let operation: String = row.get(7)?;
    let safety_class: String = row.get(8)?;
    let precondition: String = row.get(9)?;
    let outcome: String = row.get(10)?;
    let http_status = row
        .get::<_, Option<i64>>(11)?
        .map(|value| u16::try_from(value).map_err(|_| invalid_outbound_request_receipt_value(11)))
        .transpose()?;
    let retry_after_seconds = row
        .get::<_, Option<i64>>(12)?
        .map(|value| u64::try_from(value).map_err(|_| invalid_outbound_request_receipt_value(12)))
        .transpose()?;
    Ok(OutboundRequestReceipt {
        id: OutboundRequestReceiptId(row.get(0)?),
        correlation_id,
        started_at_ms: row.get(2)?,
        completed_at_ms: row.get(3)?,
        duration_ms: row.get(4)?,
        vehicle_tesla_id: row.get(5)?,
        transport: OutboundRequestTransport::parse(&transport)
            .ok_or_else(|| invalid_outbound_request_receipt_value(6))?,
        operation: OutboundRequestOperation::parse(&operation)
            .ok_or_else(|| invalid_outbound_request_receipt_value(7))?,
        safety_class: OutboundRequestSafetyClass::parse(&safety_class)
            .ok_or_else(|| invalid_outbound_request_receipt_value(8))?,
        precondition: OutboundRequestPrecondition::parse(&precondition)
            .ok_or_else(|| invalid_outbound_request_receipt_value(9))?,
        outcome: if outcome == "started" {
            None
        } else {
            Some(
                OutboundRequestOutcome::parse(&outcome)
                    .ok_or_else(|| invalid_outbound_request_receipt_value(10))?,
            )
        },
        http_status,
        retry_after_seconds,
    })
}

fn cleanup_abandoned_import_generations(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute("DELETE FROM import_generations", [])
        .map_err(StoreError::ImportGeneration)?;
    Ok(())
}

fn require_positive_db(value: i64, field: &'static str) -> Result<(), StoreError> {
    if value <= 0 {
        Err(StoreError::InvalidLifecycleCarId)
    } else {
        let _ = field;
        Ok(())
    }
}
