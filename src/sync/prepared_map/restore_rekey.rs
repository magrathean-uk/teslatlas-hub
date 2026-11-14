// SPDX-License-Identifier: AGPL-3.0-only

//! Restore re-issues a prepared month for its re-keyed source head (M06-T16).
//! Additive to `prepared_map`: it reuses the producer's own image writer,
//! receipt signer and semantic check, and edits none of them.

use super::*;

/// One catalogued prepared month, as restore reads it (M06-T16).
#[derive(Debug, Clone)]
pub(crate) struct StoredPreparedMonth {
    pub(crate) artifact_id: String,
    pub(crate) vehicle_id: String,
    pub(crate) month: String,
    pub(crate) input_manifest_id: String,
    pub(crate) input_receipt_id: String,
    pub(crate) input_sequence: i64,
    pub(crate) source_digest: String,
    pub(crate) pack_sha256: String,
    pub(crate) compressed_bytes: i64,
    /// `None` when the stored receipt exceeds the control response ceiling.
    pub(crate) receipt_json: Option<Vec<u8>>,
}

impl StoredPreparedMonth {
    fn cached(&self) -> CachedMonth {
        CachedMonth {
            vehicle: self.vehicle_id.clone(),
            source_receipt: self.input_receipt_id.clone(),
            month: self.month.clone(),
            manifest: self.input_manifest_id.clone(),
            sequence: self.input_sequence,
            source_digest: self.source_digest.clone(),
            digest: self.pack_sha256.clone(),
            compressed_bytes: self.compressed_bytes,
            receipt: self.receipt_json.clone(),
        }
    }
}

/// A prepared month re-issued for its source head's renamed receipt.
#[derive(Debug, Clone)]
pub(crate) struct RekeyedPreparedMonth {
    pub(crate) artifact_id: String,
    pub(crate) pack_sha256: String,
    pub(crate) compressed_bytes: u64,
    pub(crate) receipt_json: Vec<u8>,
}

/// Restore (M06-T16) renames a physical head's receipt when it re-signs the
/// head's cursor under a fresh key. A prepared month binds that receipt in its
/// artefact ID, its signed receipt and its pack metadata, which the App
/// requires to equal the receipt, so the month is re-issued for the renamed
/// head: the same month, window, drive and tile counts, tiles and generation
/// time; a pack rebuilt from its own decoded rows; a receipt the new key
/// signs. The new pack is written into `packs_dir` (content-addressed, no
/// clobber) and the scratch files into `scratch`.
///
/// `Ok(None)` when the stored month is not exactly what the producer writes
/// for `old` (its signature aside, which only the old key could check), or
/// its pack is not the object it names: restore then drops the month, as
/// pruning would, and the next import prepares it again.
pub(crate) fn rekey_prepared_month_for_restore(
    packs_dir: &Path,
    scratch: &Path,
    cursor_key: &CursorKey,
    old: &PendingPhysicalV3Admission,
    new: &PendingPhysicalV3Admission,
    stored: &StoredPreparedMonth,
) -> PreparedResult<Option<RekeyedPreparedMonth>> {
    if old.vehicle_id != new.vehicle_id
        || old.snapshot_id != new.snapshot_id
        || old.head_sequence != new.head_sequence
        || old.selected_car_id != new.selected_car_id
    {
        return Err(invalid("a re-keyed prepared source changed its head"));
    }
    let Some(receipt) = &stored.receipt_json else {
        return Ok(None);
    };
    if stored.vehicle_id != old.vehicle_id.to_string()
        || stored.input_receipt_id != old.receipt_id
        || stored.input_manifest_id != old.snapshot_id.to_string()
        || u64::try_from(stored.input_sequence).ok() != Some(old.head_sequence)
    {
        return Ok(None);
    }
    let Ok(mut document) = serde_json::from_slice::<serde_json::Value>(receipt) else {
        return Ok(None);
    };
    if serde_json::to_vec(&document)? != *receipt {
        return Ok(None);
    }
    let (Some(from_ms), Some(to_ms)) = (
        document["window"]["from_ms"].as_i64(),
        document["window"]["to_ms"].as_i64(),
    ) else {
        return Ok(None);
    };
    let Ok(Some(month)) = previous_complete_month(to_ms) else {
        return Ok(None);
    };
    if month.name != stored.month
        || month.from_ms != from_ms
        || month.to_ms != to_ms
        || artifact_id(old, &month.name, from_ms, to_ms) != stored.artifact_id
    {
        return Ok(None);
    }
    let counts = (|| {
        let span = document["dirty_spans"]["map_months"].as_array()?.first()?;
        Some((
            usize::try_from(span["drive_count"].as_u64()?).ok()?,
            usize::try_from(span["tile_count"].as_u64()?).ok()?,
            document["pack"]["uncompressed_bytes"].as_u64()?,
            document["generation"]["generated_at_ms"].as_i64()?,
        ))
    })();
    let Some((drive_count, tile_count, uncompressed_bytes, generated_at_ms)) = counts else {
        return Ok(None);
    };
    if drive_count > 10_000
        || tile_count > 4096
        || uncompressed_bytes == 0
        || uncompressed_bytes > MAX_SQLITE_BYTES
        || generated_at_ms < 0
    {
        return Ok(None);
    }
    let Ok(compressed_bytes) = u64::try_from(stored.compressed_bytes) else {
        return Ok(None);
    };
    let Ok(digest) = stored.pack_sha256.parse::<Sha256Digest>() else {
        return Ok(None);
    };
    if compressed_bytes == 0
        || compressed_bytes > MAX_PACK_BYTES
        || digest.to_string() != stored.pack_sha256
    {
        return Ok(None);
    }
    // The stored receipt must be the producer's receipt for `old` in every
    // member but its signature, which the old key alone could check.
    let mut expected: serde_json::Value = serde_json::from_slice(&signed_receipt(
        cursor_key,
        old,
        &stored.artifact_id,
        &month,
        drive_count,
        tile_count,
        &stored.pack_sha256,
        compressed_bytes,
        uncompressed_bytes,
        generated_at_ms,
    )?)?;
    let found_signature = document
        .as_object_mut()
        .and_then(|object| object.remove("signature"));
    if let Some(object) = expected.as_object_mut() {
        object.remove("signature");
    }
    if document != expected || !found_signature.is_some_and(|signature| signature.is_object()) {
        return Ok(None);
    }

    let source_path = packs_dir
        .join("sha256")
        .join(format!("{digest}.sqlite.zst"));
    let Ok(mut source) =
        verified_immutable_pack(&source_path, compressed_bytes, digest, MAX_PACK_BYTES)
    else {
        return Ok(None);
    };
    std::io::Seek::seek(&mut source, std::io::SeekFrom::Start(0))?;
    let decoded = NamedTempFile::new_in(scratch)?;
    let Ok(decoder) = zstd::stream::Decoder::new(source) else {
        return Ok(None);
    };
    let copied = copy(&mut decoder.take(uncompressed_bytes + 1), &mut decoded.as_file());
    if copied.ok() != Some(uncompressed_bytes) {
        return Ok(None);
    }
    let rows = (|| -> rusqlite::Result<Option<Vec<(u32, u32, u32, Vec<u8>)>>> {
        let pack = Connection::open_with_flags(
            decoded.path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let metadata: (String, String, String, String, i64, String, i64, i64) = pack.query_row(
            "SELECT artifact_id, vehicle_id, input_manifest_id, input_receipt_id,
                    input_sequence, algorithm_version, window_from_ms, window_to_ms
               FROM prepared_metadata",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )?;
        if metadata
            != (
                stored.artifact_id.clone(),
                stored.vehicle_id.clone(),
                stored.input_manifest_id.clone(),
                stored.input_receipt_id.clone(),
                stored.input_sequence,
                ALGORITHM_VERSION.to_owned(),
                month.from_ms,
                month.to_ms,
            )
        {
            return Ok(None);
        }
        let months: Vec<(String, i64, i64, String, i64, i64)> = pack
            .prepare(
                "SELECT month, from_ms, to_ms, resolution, drive_count, tile_count
                   FROM map_months",
            )?
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        let resolution = if tile_count == 0 { "readyEmpty" } else { "readyData" };
        if months
            != [(
                month.name.clone(),
                month.from_ms,
                month.to_ms,
                resolution.to_owned(),
                i64::try_from(drive_count).unwrap_or(-1),
                i64::try_from(tile_count).unwrap_or(-1),
            )]
        {
            return Ok(None);
        }
        let tiles: Vec<(u32, u32, u32, Vec<u8>)> = pack
            .prepare(
                "SELECT zoom, tile_x, tile_y, segments FROM map_tiles
                  ORDER BY month, zoom, tile_x, tile_y",
            )?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
            .collect::<Result<_, _>>()?;
        Ok((tiles.len() == tile_count).then_some(tiles))
    })();
    let Ok(Some(tiles)) = rows else {
        return Ok(None);
    };
    drop(decoded);

    let id = artifact_id(new, &month.name, month.from_ms, month.to_ms);
    let image = NamedTempFile::new_in(scratch)?;
    let rebuilt_bytes = write_sqlite(&image, &id, new, &month, drive_count, &tiles)?;
    let (pack_sha256, rebuilt_compressed) =
        rekeyed_compressed_pack(packs_dir, scratch, &image, rebuilt_bytes)?;
    let receipt_json = signed_receipt(
        cursor_key,
        new,
        &id,
        &month,
        drive_count,
        tile_count,
        &pack_sha256,
        rebuilt_compressed,
        rebuilt_bytes,
        generated_at_ms,
    )?;
    Ok(Some(RekeyedPreparedMonth {
        artifact_id: id,
        pack_sha256,
        compressed_bytes: rebuilt_compressed,
        receipt_json,
    }))
}

/// Compress one rebuilt prepared image into `packs_dir` without a store, as
/// publication does, and sync it and its directory.
fn rekeyed_compressed_pack(
    packs_dir: &Path,
    scratch: &Path,
    sqlite: &NamedTempFile,
    uncompressed_bytes: u64,
) -> PreparedResult<(String, u64)> {
    let mut compressed = NamedTempFile::new_in(scratch)?;
    let mut input = File::open(sqlite.path())?;
    let mut encoder = zstd::stream::Encoder::new(&mut compressed, 3)?;
    encoder.set_pledged_src_size(Some(uncompressed_bytes))?;
    copy(&mut input, &mut encoder)?;
    encoder.finish()?;
    compressed.flush()?;
    compressed.as_file().sync_all()?;
    let bytes = compressed.as_file().metadata()?.len();
    if bytes == 0 || bytes > MAX_PACK_BYTES {
        return Err(invalid("prepared compressed pack exceeds contract"));
    }
    let digest = Sha256Digest::of_bytes(&fs::read(compressed.path())?);
    let directory = packs_dir.join("sha256");
    let destination = directory.join(format!("{digest}.sqlite.zst"));
    let installed = match compressed.persist_noclobber(&destination) {
        Ok(file) => file,
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            verified_immutable_pack(&destination, bytes, digest, MAX_PACK_BYTES)?
        }
        Err(error) => return Err(error.error.into()),
    };
    installed.sync_all()?;
    File::open(&directory)?.sync_all()?;
    Ok((digest.to_string(), bytes))
}

/// Whether a catalogued prepared month verifies under `cursor_key` exactly as
/// the delivery routes check it, and its pack is the object it names.
pub(crate) fn restored_prepared_month_is_current(
    packs_dir: &Path,
    cursor_key: &CursorKey,
    stored: &StoredPreparedMonth,
) -> PreparedResult<bool> {
    if !current_semantic_receipt(cursor_key, &stored.artifact_id, &stored.cached())? {
        return Ok(false);
    }
    let (Ok(bytes), Ok(digest)) = (
        u64::try_from(stored.compressed_bytes),
        stored.pack_sha256.parse::<Sha256Digest>(),
    ) else {
        return Ok(false);
    };
    let path = packs_dir
        .join("sha256")
        .join(format!("{digest}.sqlite.zst"));
    Ok(verified_immutable_pack(&path, bytes, digest, MAX_PACK_BYTES).is_ok())
}

/// The physical stage of `super::tests::month_stage` for the previous complete month
/// at `now_ms`, with that month's name and window (M06-T16 restore tests).
#[cfg(test)]
pub(crate) fn month_stage_fixture(
    root: &Path,
    now_ms: i64,
    changed: bool,
) -> (SourceDbStage, String, i64, i64) {
    let month = previous_complete_month(now_ms)
        .expect("fixture month")
        .expect("fixture clock has a previous complete month");
    let stage = super::tests::month_stage(root, &month, changed, false);
    (stage, month.name, month.from_ms, month.to_ms)
}
