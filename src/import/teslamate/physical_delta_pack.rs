// SPDX-License-Identifier: AGPL-3.0-only

//! Optional Protocol 1.4 changed-set materialization from verified full heads.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params, types::Value};
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::physical_delta_compare::{PhysicalCompareError, PhysicalDeltaComparison};
use crate::{
    db::PendingPhysicalV3Admission,
    hub_pack::{
        PHYSICAL_TABLES_2_2, ProjectionBinding, ProjectionPackError, available_bytes,
        ensure_private_staging_directory, open_verified_physical_pack_2_2,
    },
    manifest_signing::ManifestSigning,
    protocol::{CursorKey, Sha256Digest},
};

const DELTA_SQL: &str = include_str!("physical_delta_pack_v1.sql");
const MAX_ROWS: u64 = 2_000_000;
const MAX_ROOTS: u64 = 10_000;
const MAX_DECODED: u64 = 256 * 1024 * 1024;
const MAX_COMPRESSED: u64 = 16 * 1024 * 1024;
const MAX_PACKS: usize = 64;
const MAX_AGGREGATE_DECODED: u64 = 2 * 1024 * 1024 * 1024;
const MAX_AGGREGATE_COMPRESSED: u64 = 256 * 1024 * 1024;
const CHUNK_TARGET_DECODED: u64 = 16 * 1024 * 1024;
const SCRATCH_LIMIT: u64 = 8 * 1024 * 1024 * 1024;
const FORMAT: &str = "teslatlas-physical-v3-delta-v1";

#[derive(Debug, Error)]
pub(crate) enum PhysicalDeltaPackError {
    #[error("physical delta exceeds Protocol 1.4 limits")]
    Capacity,
    #[error("physical delta comparison: {0}")]
    Compare(#[from] PhysicalCompareError),
    #[error("physical delta source pack: {0}")]
    Pack(#[from] ProjectionPackError),
    #[error("physical delta SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("physical delta file: {0}")]
    Io(#[from] io::Error),
    #[error("physical delta JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("physical delta missing target context")]
    MissingContext,
}

#[derive(Debug, Clone)]
pub(crate) struct PhysicalDeltaPack {
    pub ordinal: u32,
    pub sha256: Sha256Digest,
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
    pub path: PathBuf,
    pub created: bool,
}

pub(crate) struct StagedPhysicalDelta {
    pub vehicle_id: uuid::Uuid,
    pub base_receipt_id: String,
    pub target_receipt_id: String,
    pub base_manifest_signed_sha256: Sha256Digest,
    pub target_manifest_signed_sha256: Sha256Digest,
    pub receipt_json: Vec<u8>,
    pub total_rows: u64,
    pub impacted_roots_count: u64,
    pub packs: Vec<PhysicalDeltaPack>,
    cleanup: bool,
}

impl StagedPhysicalDelta {
    pub(crate) fn retain_catalogued_objects(&mut self) {
        self.cleanup = false;
    }
}

impl Drop for StagedPhysicalDelta {
    fn drop(&mut self) {
        if self.cleanup {
            for pack in &self.packs {
                if pack.created {
                    let _ = fs::remove_file(&pack.path);
                }
            }
        }
    }
}

struct CreatedPacks(Vec<PhysicalDeltaPack>);

impl Drop for CreatedPacks {
    fn drop(&mut self) {
        for pack in &self.0 {
            if pack.created {
                let _ = fs::remove_file(&pack.path);
            }
        }
    }
}

struct DeltaOutput {
    sqlite: tempfile::NamedTempFile,
    connection: Connection,
    evidence_rows: u64,
}

impl DeltaOutput {
    fn new(staging: &Path, metadata: &[(&str, String)]) -> Result<Self, PhysicalDeltaPackError> {
        let sqlite = tempfile::Builder::new()
            .prefix("physical-delta-")
            .suffix(".sqlite")
            .tempfile_in(staging)?;
        let connection = Connection::open(sqlite.path())?;
        connection.execute_batch(
            "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY;",
        )?;
        connection.pragma_update(None, "max_page_count", (MAX_DECODED / 4096) as i64)?;
        connection.execute_batch(DELTA_SQL)?;
        for (key, value) in metadata {
            connection.execute(
                "INSERT INTO delta_metadata(key,value) VALUES (?1,?2)",
                params![key, value],
            )?;
        }
        Ok(Self {
            sqlite,
            connection,
            evidence_rows: 0,
        })
    }

    fn decoded_bytes(&self) -> Result<u64, PhysicalDeltaPackError> {
        let pages: i64 = self
            .connection
            .query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_bytes: i64 = self
            .connection
            .query_row("PRAGMA page_size", [], |r| r.get(0))?;
        u64::try_from(pages)
            .ok()
            .and_then(|count| {
                u64::try_from(page_bytes)
                    .ok()
                    .and_then(|size| count.checked_mul(size))
            })
            .ok_or(PhysicalDeltaPackError::Capacity)
    }

    fn publish(
        self,
        ordinal: u32,
        packs_dir: &Path,
        minimum_free_bytes: u64,
    ) -> Result<PhysicalDeltaPack, PhysicalDeltaPackError> {
        if self.evidence_rows == 0 || self.decoded_bytes()? > MAX_DECODED {
            return Err(PhysicalDeltaPackError::Capacity);
        }
        drop(self.connection);
        let mut pack = publish_pack(self.sqlite.path(), packs_dir, minimum_free_bytes)?;
        pack.ordinal = ordinal;
        Ok(pack)
    }
}

fn rotate_if_full(
    output: &mut DeltaOutput,
    packs: &mut CreatedPacks,
    staging: &Path,
    metadata: &[(&str, String)],
    packs_dir: &Path,
    chunk_target_decoded: u64,
    minimum_free_bytes: u64,
) -> Result<(), PhysicalDeltaPackError> {
    if !output.evidence_rows.is_multiple_of(128) || output.decoded_bytes()? < chunk_target_decoded {
        return Ok(());
    }
    if packs.0.len() >= MAX_PACKS {
        return Err(PhysicalDeltaPackError::Capacity);
    }
    let next = DeltaOutput::new(staging, metadata)?;
    let finished = std::mem::replace(output, next);
    let ordinal = u32::try_from(packs.0.len()).map_err(|_| PhysicalDeltaPackError::Capacity)?;
    packs
        .0
        .push(finished.publish(ordinal, packs_dir, minimum_free_bytes)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_changed_set(
    prior: &PendingPhysicalV3Admission,
    target: &PendingPhysicalV3Admission,
    binding: &ProjectionBinding,
    cursor_key: &CursorKey,
    target_raw_sha256: Sha256Digest,
    packs_dir: &Path,
    minimum_free_bytes: u64,
) -> Result<StagedPhysicalDelta, PhysicalDeltaPackError> {
    prepare_changed_set_with_chunk_target(
        prior,
        target,
        binding,
        cursor_key,
        target_raw_sha256,
        packs_dir,
        minimum_free_bytes,
        CHUNK_TARGET_DECODED,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_changed_set_with_chunk_target(
    prior: &PendingPhysicalV3Admission,
    target: &PendingPhysicalV3Admission,
    binding: &ProjectionBinding,
    cursor_key: &CursorKey,
    target_raw_sha256: Sha256Digest,
    packs_dir: &Path,
    minimum_free_bytes: u64,
    chunk_target_decoded: u64,
) -> Result<StagedPhysicalDelta, PhysicalDeltaPackError> {
    if !(4096..=MAX_DECODED).contains(&chunk_target_decoded) {
        return Err(PhysicalDeltaPackError::Capacity);
    }
    if target.head_sequence != prior.head_sequence + 1 {
        return Err(PhysicalDeltaPackError::MissingContext);
    }
    let comparison = PhysicalDeltaComparison::compare(
        &prior.manifest,
        &target.manifest,
        binding,
        cursor_key,
        packs_dir,
        SCRATCH_LIMIT,
        minimum_free_bytes,
    )?;
    comparison.prepare_wire_scope()?;
    let db = comparison.connection();
    let context_count: i64 =
        db.query_row("SELECT COUNT(*) FROM target_context", [], |r| r.get(0))?;
    let tombstone_count: i64 = db.query_row(
        "SELECT COUNT(*) FROM changed_raw WHERE removed=1",
        [],
        |r| r.get(0),
    )?;
    let affected_count: i64 =
        db.query_row("SELECT COUNT(*) FROM affected_projected", [], |r| r.get(0))?;
    let root_count: i64 = db.query_row("SELECT COUNT(*) FROM impacted_roots", [], |r| r.get(0))?;
    let rows = u64::try_from(
        context_count
            .checked_add(tombstone_count)
            .ok_or(PhysicalDeltaPackError::Capacity)?,
    )
    .map_err(|_| PhysicalDeltaPackError::Capacity)?;
    if rows == 0
        || rows > MAX_ROWS
        || affected_count < 0
        || affected_count as u64 > MAX_ROWS
        || root_count < 0
        || root_count as u64 > MAX_ROOTS
    {
        return Err(PhysicalDeltaPackError::Capacity);
    }
    let signing = ManifestSigning::from_cursor_key(cursor_key);
    let base_manifest = signing.signed_physical_manifest_document(prior)?;
    let target_manifest = signing.signed_physical_manifest_document(target)?;
    let base_sha = Sha256Digest::of_bytes(&base_manifest);
    let target_sha = Sha256Digest::of_bytes(&target_manifest);
    let base = checkpoint(prior, base_sha);
    let next = checkpoint(target, target_sha);
    let source = json!({
        "installation_id": binding.installation_id,
        "account_id": binding.account_id,
        "vehicle_id": binding.vehicle_id,
        "generation": binding.generation,
        "selected_car_id": binding.selected_car_id,
    });
    let months = comparison.affected_months()?;
    let staging = packs_dir.join(".staging");
    ensure_private_staging_directory(&staging)?;
    let required = MAX_AGGREGATE_DECODED
        .checked_add(MAX_DECODED)
        .and_then(|n| n.checked_add(MAX_COMPRESSED))
        .and_then(|n| n.checked_add(minimum_free_bytes))
        .ok_or(PhysicalDeltaPackError::Capacity)?;
    if available_bytes(&staging)? < required {
        return Err(PhysicalDeltaPackError::Capacity);
    }
    let metadata = [
        ("payload_format", FORMAT.to_owned()),
        ("base_manifest_id", prior.snapshot_id.to_string()),
        ("base_receipt_id", prior.receipt_id.clone()),
        ("base_manifest_sha256", base_sha.to_string()),
        ("base_sequence", prior.head_sequence.to_string()),
        ("target_manifest_id", target.snapshot_id.to_string()),
        ("target_receipt_id", target.receipt_id.clone()),
        ("target_manifest_sha256", target_sha.to_string()),
        ("target_sequence", target.head_sequence.to_string()),
        ("target_raw_sha256", target_raw_sha256.to_string()),
        ("installation_id", binding.installation_id.to_string()),
        ("account_id", binding.account_id.to_string()),
        ("vehicle_id", binding.vehicle_id.to_string()),
        ("generation", binding.generation.to_string()),
        ("selected_car_id", binding.selected_car_id.to_string()),
    ];
    let mut output = DeltaOutput::new(&staging, &metadata)?;
    let mut packs = CreatedPacks(Vec::new());
    let mut tombstones =
        db.prepare("SELECT table_name,id FROM changed_raw WHERE removed=1 ORDER BY table_name,id")?;
    let mut tombstone_rows = tombstones.query([])?;
    while let Some(row) = tombstone_rows.next()? {
        output.connection.execute(
            "INSERT INTO tombstones VALUES (?1,?2)",
            params![row.get::<_, String>(0)?, row.get::<_, i64>(1)?],
        )?;
        output.evidence_rows += 1;
        rotate_if_full(
            &mut output,
            &mut packs,
            &staging,
            &metadata,
            packs_dir,
            chunk_target_decoded,
            minimum_free_bytes,
        )?;
    }
    let mut affected_hash = Sha256::new();
    let mut rotate_error = None;
    let affected_result = comparison.visit_affected_projected(|table, id, absent| {
        let effect = if absent { "delete" } else { "recompute" };
        output.connection.execute(
            "INSERT INTO affected_projected_ids VALUES (?1,?2,?3)",
            params![table, id, effect],
        )?;
        affected_hash.update(format!("{table}\t{id}\t{effect}\n").as_bytes());
        output.evidence_rows += 1;
        if let Err(error) = rotate_if_full(
            &mut output,
            &mut packs,
            &staging,
            &metadata,
            packs_dir,
            chunk_target_decoded,
            minimum_free_bytes,
        ) {
            rotate_error = Some(error);
            return Err(PhysicalCompareError::Capacity);
        }
        Ok(())
    });
    if let Some(error) = rotate_error {
        return Err(error);
    }
    affected_result?;
    let mut root_hash = Sha256::new();
    let mut rotate_error = None;
    let root_result = comparison.visit_roots(|root| {
        output.connection.execute(
            "INSERT INTO impacted_roots VALUES (?1,?2,?3,?4,?5)",
            params![
                root.root_type,
                root.id,
                root.base_state,
                root.target_state,
                root.target_child_count
            ],
        )?;
        root_hash.update(
            format!(
                "{}\t{}\t{}\t{}\t{}\n",
                root.root_type,
                root.id,
                root.base_state,
                root.target_state,
                root.target_child_count
            )
            .as_bytes(),
        );
        output.evidence_rows += 1;
        if let Err(error) = rotate_if_full(
            &mut output,
            &mut packs,
            &staging,
            &metadata,
            packs_dir,
            chunk_target_decoded,
            minimum_free_bytes,
        ) {
            rotate_error = Some(error);
            return Err(PhysicalCompareError::Capacity);
        }
        Ok(())
    });
    if let Some(error) = rotate_error {
        return Err(error);
    }
    root_result?;
    copy_target_context(
        &comparison,
        &target.manifest,
        binding,
        packs_dir,
        &staging,
        &metadata,
        &mut output,
        &mut packs,
        chunk_target_decoded,
        minimum_free_bytes,
    )?;
    if output.evidence_rows > 0 {
        if packs.0.len() >= MAX_PACKS {
            return Err(PhysicalDeltaPackError::Capacity);
        }
        let ordinal = u32::try_from(packs.0.len()).map_err(|_| PhysicalDeltaPackError::Capacity)?;
        packs
            .0
            .push(output.publish(ordinal, packs_dir, minimum_free_bytes)?);
    }
    if packs.0.is_empty() {
        return Err(PhysicalDeltaPackError::Capacity);
    }
    let total_compressed = packs
        .0
        .iter()
        .try_fold(0_u64, |sum, pack| sum.checked_add(pack.compressed_bytes))
        .ok_or(PhysicalDeltaPackError::Capacity)?;
    let total_decoded = packs
        .0
        .iter()
        .try_fold(0_u64, |sum, pack| sum.checked_add(pack.uncompressed_bytes))
        .ok_or(PhysicalDeltaPackError::Capacity)?;
    if total_compressed > MAX_AGGREGATE_COMPRESSED || total_decoded > MAX_AGGREGATE_DECODED {
        return Err(PhysicalDeltaPackError::Capacity);
    }
    let chunks = packs
        .0
        .iter()
        .map(|pack| {
            json!({"chunk_index":pack.ordinal,"pack":{
                "object_name":format!("{}.sqlite.zst",pack.sha256),
                "sha256":pack.sha256.to_string(),
                "compressed_bytes":pack.compressed_bytes,
            }})
        })
        .collect::<Vec<_>>();
    let receipt = json!({
        "kind": "physical_changed_set",
        "payload_format": FORMAT,
        "vehicle_id": binding.vehicle_id,
        "source": source,
        "base": base,
        "target": next,
        "target_raw_sha256": target_raw_sha256.to_string(),
        "chunks": chunks,
        "total_compressed_bytes": total_compressed,
        "total_uncompressed_bytes": total_decoded,
        "total_rows": rows,
        "affected_projected_ids_sha256": hex::encode(affected_hash.finalize()),
        "affected_projected_ids_count": affected_count,
        "impacted_roots_sha256": hex::encode(root_hash.finalize()),
        "impacted_roots_count": root_count,
        "affected_months": months,
    });
    let mut staged = StagedPhysicalDelta {
        vehicle_id: binding.vehicle_id,
        base_receipt_id: prior.receipt_id.clone(),
        target_receipt_id: target.receipt_id.clone(),
        base_manifest_signed_sha256: base_sha,
        target_manifest_signed_sha256: target_sha,
        receipt_json: Vec::new(),
        total_rows: rows,
        impacted_roots_count: root_count as u64,
        packs: std::mem::take(&mut packs.0),
        cleanup: true,
    };
    staged.receipt_json = signing.signed_control_document(&receipt)?;
    Ok(staged)
}

fn checkpoint(admission: &PendingPhysicalV3Admission, manifest_sha256: Sha256Digest) -> JsonValue {
    json!({
        "manifest_id": admission.snapshot_id.to_string(),
        "receipt_id": admission.receipt_id,
        "manifest_sha256": manifest_sha256.to_string(),
        "sequence": admission.head_sequence,
        "schema_version": "2.2",
    })
}

fn copy_target_context(
    comparison: &PhysicalDeltaComparison,
    target: &crate::protocol::SyncManifest,
    binding: &ProjectionBinding,
    packs_dir: &Path,
    staging: &Path,
    metadata: &[(&str, String)],
    output: &mut DeltaOutput,
    packs: &mut CreatedPacks,
    chunk_target_decoded: u64,
    minimum_free_bytes: u64,
) -> Result<(), PhysicalDeltaPackError> {
    let db = comparison.connection();
    for pack in &target.chunks {
        let selected: i64 = db.query_row(
            "SELECT COUNT(*) FROM target_context WHERE pack_ordinal=?1",
            [i64::from(pack.ordinal)],
            |r| r.get(0),
        )?;
        if selected == 0 {
            continue;
        }
        let path = packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        let verified = open_verified_physical_pack_2_2(pack, target, binding, &path)?;
        for table in PHYSICAL_TABLES_2_2 {
            let mut context = db.prepare(
                "SELECT id,EXISTS(SELECT 1 FROM changed_raw c
                   WHERE c.table_name=target_context.table_name AND c.id=target_context.id AND c.removed=0)
                 FROM target_context WHERE pack_ordinal=?1 AND table_name=?2 ORDER BY id")?;
            let mut ids = context.query(params![i64::from(pack.ordinal), table])?;
            let mut source = verified
                .connection()
                .prepare(&format!("SELECT * FROM {table} WHERE id=?1"))?;
            let columns = source.column_count();
            let placeholders = std::iter::repeat_n("?", columns)
                .collect::<Vec<_>>()
                .join(",");
            let insert_sql = format!("INSERT INTO {table} VALUES ({placeholders})");
            while let Some(row) = ids.next()? {
                let id: i64 = row.get(0)?;
                let role = if row.get::<_, i64>(1)? == 0 {
                    "context"
                } else {
                    "changed"
                };
                let values = source
                    .query_row([id], |source_row| {
                        (0..columns)
                            .map(|index| source_row.get::<_, Value>(index))
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .optional()?
                    .ok_or(PhysicalDeltaPackError::MissingContext)?;
                output
                    .connection
                    .prepare_cached(&insert_sql)?
                    .execute(rusqlite::params_from_iter(values.iter()))?;
                output.connection.execute(
                    "INSERT INTO row_roles VALUES (?1,?2,?3)",
                    params![table, id, role],
                )?;
                output.evidence_rows += 1;
                rotate_if_full(
                    output,
                    packs,
                    staging,
                    metadata,
                    packs_dir,
                    chunk_target_decoded,
                    minimum_free_bytes,
                )?;
            }
        }
    }
    Ok(())
}

fn publish_pack(
    source: &Path,
    packs_dir: &Path,
    minimum_free_bytes: u64,
) -> Result<PhysicalDeltaPack, PhysicalDeltaPackError> {
    let staging = packs_dir.join(".staging");
    let mut compressed = tempfile::Builder::new()
        .prefix("physical-delta-")
        .suffix(".zst")
        .tempfile_in(&staging)?;
    {
        let source_file = fs::File::open(source)?;
        let mut encoder = zstd::stream::write::Encoder::new(compressed.as_file_mut(), 4)?;
        io::copy(&mut source_file.take(MAX_DECODED + 1), &mut encoder)?;
        encoder.finish()?;
    }
    compressed.as_file().sync_all()?;
    let bytes = fs::metadata(compressed.path())?.len();
    if bytes == 0 || bytes > MAX_COMPRESSED || available_bytes(&staging)? < minimum_free_bytes {
        return Err(PhysicalDeltaPackError::Capacity);
    }
    let mut hash = Sha256::new();
    let mut input = fs::File::open(compressed.path())?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    let sha256 = Sha256Digest::from_bytes(hash.finalize().into());
    let content = packs_dir.join("sha256");
    fs::create_dir_all(&content)?;
    let path = content.join(format!("{sha256}.sqlite.zst"));
    fs::set_permissions(compressed.path(), fs::Permissions::from_mode(0o640))?;
    let created = match fs::hard_link(compressed.path(), &path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            verified_immutable_pack(&path, bytes, sha256, MAX_COMPRESSED)?;
            false
        }
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = fs::File::open(&content).and_then(|directory| directory.sync_all()) {
        if created {
            let _ = fs::remove_file(&path);
        }
        return Err(error.into());
    }
    Ok(PhysicalDeltaPack {
        ordinal: 0,
        sha256,
        compressed_bytes: bytes,
        uncompressed_bytes: fs::metadata(source)?.len(),
        path,
        created,
    })
}

/// Admit an existing content-addressed object before reading it. NONBLOCK
/// prevents a substituted FIFO from blocking open; NOFOLLOW rejects symlinks.
/// The descriptor and final pathname must still describe the admitted inode.
pub(super) fn verified_immutable_pack(
    path: &Path,
    expected_bytes: u64,
    expected_digest: Sha256Digest,
    maximum_bytes: u64,
) -> io::Result<File> {
    if expected_bytes == 0 || expected_bytes > maximum_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content-addressed pack exceeds its byte bound",
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    verify_pack_descriptor(path, file, expected_bytes, expected_digest)
}

fn verify_pack_descriptor(
    path: &Path,
    mut file: File,
    expected_bytes: u64,
    expected_digest: Sha256Digest,
) -> io::Result<File> {
    let before = file.metadata()?;
    if !before.is_file() || before.len() != expected_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content-addressed pack is not an exact-size regular file",
        ));
    }
    verify_pack_digest(&mut file, expected_bytes, expected_digest)?;
    let after = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    if !named.is_file()
        || after.len() != expected_bytes
        || named.len() != expected_bytes
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || after.dev() != named.dev()
        || after.ino() != named.ino()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content-addressed pack changed during verification",
        ));
    }
    Ok(file)
}

fn verify_pack_digest(
    input: &mut impl Read,
    expected_bytes: u64,
    expected_digest: Sha256Digest,
) -> io::Result<()> {
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut remaining = expected_bytes;
    while remaining > 0 {
        let bytes = usize::try_from(remaining.min(buffer.len() as u64)).expect("buffer-sized read");
        input.read_exact(&mut buffer[..bytes])?;
        hash.update(&buffer[..bytes]);
        remaining -= bytes as u64;
    }
    let mut trailing = [0_u8; 1];
    if input.read(&mut trailing)? != 0
        || Sha256Digest::from_bytes(hash.finalize().into()) != expected_digest
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content-addressed pack size or digest conflicts",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_admission_reuses_exact_bytes_and_rejects_sparse_oversize() {
        let temporary = crate::private_tempdir().unwrap();
        let source = temporary.path().join("source.sqlite");
        fs::write(&source, b"synthetic physical delta").unwrap();
        let packs = temporary.path().join("packs");
        fs::create_dir_all(packs.join(".staging")).unwrap();
        let first = publish_pack(&source, &packs, 0).unwrap();
        let duplicate = publish_pack(&source, &packs, 0).unwrap();
        assert!(!duplicate.created);
        assert_eq!(duplicate.sha256, first.sha256);

        OpenOptions::new()
            .write(true)
            .open(&first.path)
            .unwrap()
            .set_len(MAX_COMPRESSED + 1)
            .unwrap();
        let failure = publish_pack(&source, &packs, 0).unwrap_err();
        assert!(failure.to_string().contains("exact-size regular file"));
        assert_eq!(fs::metadata(&first.path).unwrap().len(), MAX_COMPRESSED + 1);
    }

    #[test]
    fn collision_admission_rejects_symlinks_directories_and_replaced_inodes() {
        let temporary = crate::private_tempdir().unwrap();
        let path = temporary.path().join("pack");
        let bytes = b"bounded object";
        let digest = Sha256Digest::of_bytes(bytes);
        fs::write(&path, bytes).unwrap();
        assert!(verified_immutable_pack(&path, bytes.len() as u64, digest, MAX_COMPRESSED).is_ok());
        assert!(
            verified_immutable_pack(
                &path,
                bytes.len() as u64,
                Sha256Digest::of_bytes(b"different bytes"),
                MAX_COMPRESSED,
            )
            .is_err()
        );

        let link = temporary.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(
            verified_immutable_pack(&link, bytes.len() as u64, digest, MAX_COMPRESSED).is_err()
        );
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            verified_immutable_pack(temporary.path(), bytes.len() as u64, digest, MAX_COMPRESSED)
                .is_err()
        );

        let admitted = File::open(&path).unwrap();
        fs::rename(&path, temporary.path().join("preserved-pack")).unwrap();
        fs::write(&path, bytes).unwrap();
        assert!(verify_pack_descriptor(&path, admitted, bytes.len() as u64, digest).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            fs::read(temporary.path().join("preserved-pack")).unwrap(),
            bytes
        );
    }

    #[test]
    fn collision_hash_reads_at_most_expected_bytes_plus_one() {
        struct GrowingReader {
            read_bytes: usize,
            largest_read: usize,
        }
        impl Read for GrowingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.largest_read = self.largest_read.max(buffer.len());
                self.read_bytes += buffer.len();
                buffer.fill(0);
                Ok(buffer.len())
            }
        }
        let expected_bytes = 2 * 64 * 1024 + 7;
        let digest = Sha256Digest::of_bytes(&vec![0; expected_bytes]);
        let mut growing = GrowingReader {
            read_bytes: 0,
            largest_read: 0,
        };
        assert!(verify_pack_digest(&mut growing, expected_bytes as u64, digest).is_err());
        assert_eq!(growing.read_bytes, expected_bytes + 1);
        assert_eq!(growing.largest_read, 64 * 1024);

        let mut truncated = &b"short"[..];
        assert_eq!(
            verify_pack_digest(&mut truncated, 6, Sha256Digest::of_bytes(b"short!"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof,
        );
    }
}
