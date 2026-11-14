// SPDX-License-Identifier: AGPL-3.0-only

//! The streamed plaintext directory format (M06-T15, REV-CX-M06-T10 finding
//! 4). A plaintext backup above the listed bound is written with a manifest
//! (`backup-v5.json`) that names only the catalogue member and declares the
//! member count and bytes. Packs are named by their own digest and no-ops are
//! checked against the catalogue, so creation, verification and restore walk
//! the tree one entry at a time and never hold a member list.

use super::*;

/// How many members the staged payload holds, counted one entry at a time.
pub(super) fn count_payload_members(payload: &Path) -> Result<u64, DataRecoveryError> {
    let noops = current_schema_22_noop_pairs(payload)?.len() as u64;
    let packs = payload.join("packs").join("sha256");
    let mut count = 1_u64 + noops;
    for entry in
        fs::read_dir(&packs).map_err(|source| io_error("reading backup pack directory", &packs, source))?
    {
        entry.map_err(|source| io_error("reading backup pack entry", &packs, source))?;
        count += 1;
    }
    Ok(count)
}

/// Finish a staged plaintext backup above the listed bound in the streamed
/// directory format: the manifest names the catalogue member and declares
/// the member count and bytes; every pack, no-op and the catalogue are
/// inventoried, synced and checked by walking the tree, never as a list.
pub(super) fn publish_streamed_directory(
    staging: StagingDirectory,
    destination: &Path,
    payload: &Path,
    installation_id: Uuid,
) -> Result<DataRecoveryReport, DataRecoveryError> {
    let inventory = stream_payload_inventory(payload)?;
    let manifest = BackupManifest {
        kind: STREAMED_DIRECTORY_KIND.to_owned(),
        generation: Uuid::new_v4(),
        created_at_ms: current_epoch_ms()?,
        build: BUILD_VERSION.to_owned(),
        hub_schema: SCHEMA_VERSION,
        protocol: expected_protocol(),
        installation_id,
        scope: BACKUP_SCOPE.to_owned(),
        excluded_host_state: excluded_host_state_strings(),
        members: vec![inventory.catalogue.clone()],
        member_count: Some(inventory.count()?),
        payload_bytes: Some(inventory.bytes),
    };
    validate_manifest(&manifest)?;
    sync_payload_tree(payload, inventory.noops > 0)?;
    let manifest_bytes = canonical_json(&manifest)?;
    write_private_file(&staging.path.join(STREAMED_DIRECTORY_MANIFEST_NAME), &manifest_bytes)?;
    let marker = CompletionMarker {
        kind: BackupFormat::StreamedDirectory.completion_kind().to_owned(),
        generation: manifest.generation,
        manifest_sha256: sha256_bytes_hex(&manifest_bytes),
    };
    let marker_bytes = canonical_json(&marker)?;
    write_private_file(&staging.path.join(COMPLETION_NAME), &marker_bytes)?;
    let (sealed_manifest, sealed_manifest_bytes, sealed_marker) =
        read_backup_envelope(&staging.path)?;
    if sealed_manifest != manifest
        || sealed_manifest_bytes != manifest_bytes
        || sealed_marker != marker
    {
        return Err(invalid(
            "published data-backup envelope differs from the staged metadata",
        ));
    }
    validate_backup_tree(
        &staging.path,
        &sealed_manifest,
        &sealed_manifest_bytes,
        &sealed_marker,
    )?;
    sync_directory(&staging.path)?;
    let report = report("data_backup_created", destination, &manifest)?;
    staging.publish(destination)?;
    Ok(report)
}

/// What one walk of a payload established: the catalogue member, the pack
/// and no-op counts and the total bytes.
pub(super) struct PayloadInventory {
    pub(super) catalogue: BackupMember,
    pub(super) packs: u64,
    pub(super) noops: u64,
    pub(super) bytes: u64,
}

impl PayloadInventory {
    fn count(&self) -> Result<u64, DataRecoveryError> {
        self.packs
            .checked_add(self.noops)
            .and_then(|count| count.checked_add(1))
            .ok_or(DataRecoveryError::CapacityOverflow)
    }
}

/// Walk a staged payload one entry at a time: the catalogue, each pack
/// (canonical name, private regular file, content digest equal to its name)
/// and each current no-op; nothing per member is kept.
pub(super) fn stream_payload_inventory(payload: &Path) -> Result<PayloadInventory, DataRecoveryError> {
    let catalogue = inventory_file(&payload.join("hub.sqlite"), CATALOGUE_MEMBER.to_owned())?;
    let mut bytes = catalogue.size;
    let (packs, pack_bytes) = walk_pack_directory(&payload.join("packs").join("sha256"))?;
    bytes = checked_capacity_add(bytes, pack_bytes)?;
    let expected_noops = current_schema_22_noop_pairs(payload)?;
    validate_schema_22_noop_directory(payload, &expected_noops)?;
    let noop_directory = payload.join("packs").join("noop");
    for name in expected_noops.keys() {
        let metadata = require_regular_file(&noop_directory.join(name), "backup schema 2.2 no-op")?;
        bytes = checked_capacity_add(bytes, metadata.len())?;
    }
    Ok(PayloadInventory {
        catalogue,
        packs,
        noops: expected_noops.len() as u64,
        bytes,
    })
}

/// Read a pack directory one entry at a time: every entry must be a private
/// regular file named by its own content digest. Returns the count and bytes.
pub(super) fn walk_pack_directory(directory: &Path) -> Result<(u64, u64), DataRecoveryError> {
    let metadata = require_real_directory(directory, "SHA-256 pack directory")?;
    if permission_mode(&metadata) != PRIVATE_DIRECTORY_MODE {
        return Err(invalid("SHA-256 pack directory must have mode 0700"));
    }
    let entries = fs::read_dir(directory)
        .map_err(|source| io_error("reading pack directory", directory, source))?;
    let (mut count, mut bytes) = (0_u64, 0_u64);
    for entry in entries {
        let entry = entry.map_err(|source| io_error("reading pack entry", directory, source))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("pack directory contains a non-UTF-8 name"))?;
        if is_ignorable_finder_file(&entry.path(), &name) {
            continue;
        }
        let digest = name
            .strip_suffix(".sqlite.zst")
            .filter(|_| canonical_pack_filename(&name))
            .ok_or_else(|| invalid(format!("unexpected pack member: {name}")))?;
        let path = entry.path();
        let metadata = require_regular_file(&path, "pack member")?;
        if permission_mode(&metadata) != PRIVATE_FILE_MODE || sha256_file_hex(&path)? != digest {
            return Err(invalid(format!(
                "pack member differs from its name: {PACK_DIRECTORY}/{name}"
            )));
        }
        count += 1;
        bytes = checked_capacity_add(bytes, metadata.len())?;
    }
    Ok((count, bytes))
}

/// Sync every file of a payload and then its directories, walking the tree.
pub(super) fn sync_payload_tree(payload: &Path, has_noops: bool) -> Result<(), DataRecoveryError> {
    sync_file(&payload.join("hub.sqlite"))?;
    let mut directories = vec![payload.join("packs").join("sha256")];
    if has_noops {
        directories.push(payload.join("packs").join("noop"));
    }
    for directory in &directories {
        for entry in fs::read_dir(directory)
            .map_err(|source| io_error("reading payload directory", directory, source))?
        {
            let entry = entry.map_err(|source| io_error("reading payload entry", directory, source))?;
            sync_file(&entry.path())?;
        }
        sync_directory(directory)?;
    }
    sync_directory(&payload.join("packs"))?;
    sync_directory(payload)
}

/// Copy a streamed directory backup's members into `staging`, one entry at a
/// time: the catalogue against its manifest member, each pack against the
/// digest its name declares and each current no-op against its own digest
/// (its content is checked against the catalogue afterwards).
pub(super) fn copy_streamed_directory(
    source: &Path,
    staging: &Path,
    manifest: &BackupManifest,
) -> Result<StreamedMembers, DataRecoveryError> {
    let catalogue = manifest
        .members
        .first()
        .cloned()
        .ok_or_else(|| invalid("a streamed directory manifest names its catalogue"))?;
    let data = source.join(DATA_DIRECTORY);
    copy_verified_member(&data.join("hub.sqlite"), &staging.join("hub.sqlite"), &catalogue)?;
    let packs = data.join("packs").join("sha256");
    let mut pack_count = 0_u64;
    for entry in
        fs::read_dir(&packs).map_err(|error| io_error("reading backup pack directory", &packs, error))?
    {
        let entry = entry.map_err(|error| io_error("reading backup pack entry", &packs, error))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("backup pack filename is not UTF-8"))?;
        if is_ignorable_finder_file(&entry.path(), &name) {
            continue;
        }
        let digest = name
            .strip_suffix(".sqlite.zst")
            .filter(|_| canonical_pack_filename(&name))
            .ok_or_else(|| invalid(format!("unexpected backup pack member: {name}")))?;
        let metadata = require_regular_file(&entry.path(), "source backup member")?;
        let member = BackupMember {
            path: format!("{PACK_DIRECTORY}/{name}"),
            size: metadata.len(),
            mode: PRIVATE_FILE_MODE,
            sha256: digest.to_owned(),
        };
        copy_verified_member(
            &entry.path(),
            &staging.join("packs").join("sha256").join(&name),
            &member,
        )?;
        pack_count += 1;
    }
    let expected_noops = current_schema_22_noop_pairs(&data)?;
    if !expected_noops.is_empty() {
        create_private_directory(&staging.join("packs").join("noop"))?;
    }
    for name in expected_noops.keys() {
        let from = data.join("packs").join("noop").join(name);
        let member = inventory_file(&from, format!("{SCHEMA_22_NOOP_DIRECTORY}/{name}"))?;
        copy_verified_member(&from, &staging.join("packs").join("noop").join(name), &member)?;
    }
    let streamed = StreamedMembers {
        catalogue,
        packs: pack_count,
        noops: expected_noops.len() as u64,
    };
    if Some(streamed.packs + streamed.noops + 1) != manifest.member_count {
        return Err(invalid(
            "the backup's members do not match the count its manifest declares",
        ));
    }
    Ok(streamed)
}

/// The payload of a streamed directory backup, checked by walking it
/// (M06-T15): the catalogue against the manifest's member, every pack as a
/// private file whose digest is its name, the no-ops exactly the current
/// catalogue's, and the walked count and bytes against the declared ones.
pub(super) fn validate_streamed_directory_payload(
    data: &Path,
    manifest: &BackupManifest,
) -> Result<(), DataRecoveryError> {
    let [catalogue] = manifest.members.as_slice() else {
        return Err(invalid("a streamed directory manifest names its catalogue member"));
    };
    let expected_noops = current_schema_22_noop_pairs(data)?;
    let packs = data.join("packs");
    if expected_noops.is_empty() {
        require_directory_entries(&packs, ["sha256"], "data-backup packs directory")?;
    } else {
        require_directory_entries(&packs, ["noop", "sha256"], "data-backup packs directory")?;
    }
    validate_schema_22_noop_directory(data, &expected_noops)?;
    let path = data.join("hub.sqlite");
    let metadata = require_regular_file(&path, "data-backup member")?;
    if permission_mode(&metadata) != catalogue.mode
        || metadata.len() != catalogue.size
        || sha256_file_hex(&path)? != catalogue.sha256
    {
        return Err(invalid(format!(
            "data-backup member changed: {CATALOGUE_MEMBER}"
        )));
    }
    let (pack_count, pack_bytes) = walk_pack_directory(&packs.join("sha256"))?;
    let mut bytes = checked_capacity_add(catalogue.size, pack_bytes)?;
    for name in expected_noops.keys() {
        let metadata = require_regular_file(&packs.join("noop").join(name), "data-backup member")?;
        bytes = checked_capacity_add(bytes, metadata.len())?;
    }
    let count = pack_count
        .checked_add(expected_noops.len() as u64 + 1)
        .ok_or(DataRecoveryError::CapacityOverflow)?;
    if Some(count) != manifest.member_count || Some(bytes) != manifest.payload_bytes {
        return Err(invalid(
            "data-backup members do not match the count and bytes its manifest declares",
        ));
    }
    Ok(())
}
