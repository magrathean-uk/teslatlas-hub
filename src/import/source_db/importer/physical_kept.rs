// SPDX-License-Identifier: AGPL-3.0-only

// A window's data reaches 1.4 clients without another import (M03-T22,
// DEC-137). At import publication the sealed raw physical stage and the
// import's source sessions are kept per vehicle under the packs directory,
// their digests in `hub_metadata`. After a handback the seam is planned from
// the kept source sessions and the new coverage: when the plan restates no
// source row it is applied in one store transaction and the head is
// published from a copy of the kept stage with the new pack plan; when it
// restates one nothing is applied and the head stays refused until the next
// import.

/// The directory under the packs directory that holds the kept stages.
const PHYSICAL_KEPT_DIRECTORY: &str = "physical-kept";

/// The import's source sessions kept beside its raw physical stage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct KeptPhysicalSource {
    binding: ProjectionBinding,
    drives: Vec<ProjectionDrive>,
    charges: Vec<ProjectionCharge>,
}

/// What a handback did with one vehicle's physical 1.4 head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HandbackPhysicalOutcome {
    /// The current head is served: nothing to plan.
    Current,
    /// The head was planned with the new coverage and published.
    Published(crate::physical_publication::PhysicalV3PublicationKind),
    /// The plan restates a source row: nothing was applied, and the head
    /// stays refused until the next import.
    Restates,
    /// The head cannot be planned from what is kept; it stays refused until
    /// the next import. The reason names what failed.
    Withheld(String),
    /// The vehicle has no physical head any more: its kept stage went.
    Pruned,
}

fn physical_kept_dir(store: &HubStore) -> std::path::PathBuf {
    store.packs_dir().join(PHYSICAL_KEPT_DIRECTORY)
}

/// The legacy kept raw stage an earlier build wrote: a private sealed stage
/// under `.staging`, as [`SourceDbStage::open_sealed`] requires. It stays
/// readable until a verified conversion to a kept archive commits.
fn kept_stage_path(store: &HubStore, vehicle_id: Uuid) -> std::path::PathBuf {
    physical_kept_dir(store)
        .join(".staging")
        .join(format!("{vehicle_id}.sqlite"))
}

/// The legacy source sessions beside a legacy raw stage.
fn legacy_kept_source_path(store: &HubStore, vehicle_id: Uuid) -> std::path::PathBuf {
    physical_kept_dir(store).join(format!("{vehicle_id}.source.json"))
}

/// The kept archive of one generation of the vehicle's replay base
/// (DEC-250).
fn kept_archive_name(vehicle_id: Uuid, generation: Uuid) -> String {
    format!("{vehicle_id}.{generation}.kept")
}

fn kept_generation_source_name(vehicle_id: Uuid, generation: Uuid) -> String {
    format!("{vehicle_id}.{generation}.source.json")
}

/// The source sessions file the record names.
fn kept_source_path(
    store: &HubStore,
    vehicle_id: Uuid,
    kept: &crate::db::PhysicalV3Kept,
) -> std::path::PathBuf {
    match &kept.archive {
        Some(archive) => physical_kept_dir(store)
            .join(kept_generation_source_name(vehicle_id, archive.generation)),
        None => legacy_kept_source_path(store, vehicle_id),
    }
}

/// A digest of the import binding a kept archive belongs to.
fn kept_binding_digest(binding: &ProjectionBinding) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"hub kept archive binding v1\n");
    hash.update(binding.installation_id.as_bytes());
    hash.update(binding.account_id.as_bytes());
    hash.update(binding.vehicle_id.as_bytes());
    hash.update(binding.generation.to_be_bytes());
    hash.update(binding.selected_car_id.to_be_bytes());
    hash.finalize().into()
}

fn kept_archive_identity(
    binding: &ProjectionBinding,
) -> crate::source_db_stage::KeptArchiveIdentity {
    crate::source_db_stage::KeptArchiveIdentity {
        vehicle_id: binding.vehicle_id,
        binding: kept_binding_digest(binding),
    }
}

/// A simulated crash at a named point of a keep or conversion (tests catch
/// the panic): `files` after the new pair is durable, `swap` after the
/// record names it, before the superseded files go.
fn kept_crash_point(point: &'static str) {
    #[cfg(test)]
    if crate::source_db_stage::KEPT_ARCHIVE_CRASH_AT.with(|at| at.get() == Some(point)) {
        crate::source_db_stage::KEPT_ARCHIVE_CRASH_AT.with(|at| at.set(None));
        panic!("simulated crash at kept point {point}");
    }
    #[cfg(not(test))]
    let _ = point;
}

fn sync_kept_dir(store: &HubStore) -> Result<(), String> {
    std::fs::File::open(physical_kept_dir(store))
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

/// Writes one generation's kept pair durably: the archive of `stage` and
/// the source sessions `source`, then the directory. Returns the archive
/// record; nothing names the files yet.
fn write_kept_pair(
    store: &HubStore,
    binding: &ProjectionBinding,
    stage: &SourceDbStage,
    source: &[u8],
) -> Result<(crate::db::PhysicalV3KeptArchive, String), String> {
    let vehicle_id = binding.vehicle_id;
    let generation = Uuid::new_v4();
    let receipt = stage
        .write_kept_archive(
            physical_kept_dir(store),
            &kept_archive_name(vehicle_id, generation),
            kept_archive_identity(binding),
        )
        .map_err(|error| error.to_string())?;
    write_private_file(
        &physical_kept_dir(store).join(kept_generation_source_name(vehicle_id, generation)),
        source,
    )?;
    sync_kept_dir(store)?;
    kept_crash_point("files");
    Ok((
        crate::db::PhysicalV3KeptArchive {
            generation,
            format: receipt.format,
            digest: receipt.digest.to_string(),
            bytes: receipt.bytes,
            decoded_bytes: receipt.decoded_bytes,
        },
        receipt.logical.to_string(),
    ))
}

/// Keeps the sealed raw `stage`, as one compact kept archive, and the
/// import's source sessions for the vehicle's next handback, replacing what
/// an earlier import kept. The record goes first, so a crash part-way
/// leaves nothing to plan and never an older import's base; the new pair is
/// durable before one transaction names it; the superseded files go after.
/// A keep that fails forgets the vehicle's kept state; the next import
/// keeps again. Returns the kept stage's recorded digest.
fn keep_physical_source(
    store: &HubStore,
    publication_gate: &PublicationGate,
    binding: &ProjectionBinding,
    stage: &SourceDbStage,
    drives: &[ProjectionDrive],
    charges: &[ProjectionCharge],
) -> Option<String> {
    let vehicle_id = binding.vehicle_id;
    let recorded = (|| -> Result<String, String> {
        store
            .record_physical_v3_kept(publication_gate, vehicle_id, None)
            .map_err(|error| error.to_string())?;
        store
            .record_physical_v3_handback_plan(publication_gate, vehicle_id, None)
            .map_err(|error| error.to_string())?;
        let source = serde_json::to_vec(&KeptPhysicalSource {
            binding: binding.clone(),
            drives: drives.to_vec(),
            charges: charges.to_vec(),
        })
        .map_err(|error| error.to_string())?;
        // Its rows are authenticated by this digest at every handback, and
        // each handback checks the pages of the stage it plans (M08-T19).
        let (archive, logical) = write_kept_pair(store, binding, stage, &source)?;
        let kept = crate::db::PhysicalV3Kept {
            stage: logical,
            source: Sha256Digest::of_bytes(&source).to_string(),
            archive: Some(archive),
        };
        if !store
            .swap_physical_v3_kept(publication_gate, vehicle_id, None, Some(&kept), true)
            .map_err(|error| error.to_string())?
        {
            return Err("the kept record changed while it was replaced".into());
        }
        kept_crash_point("swap");
        remove_superseded_kept_files(store, vehicle_id, Some(&kept));
        Ok(kept.stage)
    })();
    match recorded {
        Ok(stage) => Some(stage),
        Err(reason) => {
            tracing::warn!(
                %vehicle_id,
                %reason,
                "the import's physical stage was not kept; a handback leaves the 1.4 head refused \
                 until the next import"
            );
            if let Err(reason) = forget_physical_kept(store, publication_gate, vehicle_id) {
                tracing::warn!(%vehicle_id, %reason, "the failed keep's record was not forgotten");
            }
            None
        }
    }
}

/// What one file of the kept directory is, by name, for one vehicle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeptFile {
    LegacySource,
    Archive(Uuid),
    Source(Uuid),
    /// An archive or source file of a generation not yet renamed into place.
    Partial(Uuid),
}

fn kept_file(vehicle_id: Uuid, name: &str) -> Option<KeptFile> {
    let rest = name.strip_prefix(&format!("{vehicle_id}."))?;
    if rest == "source.json" {
        return Some(KeptFile::LegacySource);
    }
    let generation = rest.get(..36).and_then(|id| Uuid::parse_str(id).ok())?;
    let tail = rest.get(36..)?;
    match tail {
        ".kept" => Some(KeptFile::Archive(generation)),
        ".source.json" => Some(KeptFile::Source(generation)),
        ".kept.partial" => Some(KeptFile::Partial(generation)),
        _ => tail
            .strip_prefix(".source.")
            .and_then(|temporary| temporary.strip_suffix(".tmp"))
            .filter(|temporary| Uuid::parse_str(temporary).is_ok())
            .map(|_| KeptFile::Partial(generation)),
    }
}

/// Removes the vehicle's kept files the record `kept` does not name (an
/// interrupted keep or conversion, or the superseded pair), under the
/// publication gate the caller holds. Only regular files of the kept shapes
/// go; anything else is left. A capture a pending import publication names
/// is never removed, wherever its adoption left it (issue #60); when the
/// pending publications cannot be read, nothing is removed.
fn remove_superseded_kept_files(
    store: &HubStore,
    vehicle_id: Uuid,
    kept: Option<&crate::db::PhysicalV3Kept>,
) {
    let Ok(holds) = store.import_publication_holds() else {
        return;
    };
    if holds.holds_vehicle(vehicle_id) {
        return;
    }
    let current = kept.map(|kept| kept.archive.as_ref().map(|archive| archive.generation));
    let mut removed = false;
    let directory = physical_kept_dir(store);
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(file) = kept_file(vehicle_id, &name) else {
                continue;
            };
            let named = match (file, current) {
                (KeptFile::LegacySource, Some(None)) => true,
                (KeptFile::Archive(generation) | KeptFile::Source(generation), Some(Some(now))) => {
                    generation == now
                }
                _ => false,
            };
            let held = matches!(
                file,
                KeptFile::Archive(generation) | KeptFile::Source(generation)
                    if holds.holds(vehicle_id, generation)
            );
            if !named
                && !held
                && entry.file_type().is_ok_and(|kind| kind.is_file())
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed = true;
            }
        }
    }
    let legacy = kept_stage_path(store, vehicle_id);
    if current != Some(None)
        && std::fs::symlink_metadata(&legacy).is_ok_and(|metadata| metadata.is_file())
        && std::fs::remove_file(&legacy).is_ok()
    {
        removed = true;
        let _ = std::fs::File::open(directory.join(".staging")).and_then(|dir| dir.sync_all());
    }
    if removed {
        let _ = sync_kept_dir(store);
    }
}

/// The working stage a changed handback plans: the kept archive decoded,
/// bounded, into one owned sealed stage and authenticated before any edit,
/// or for a legacy record the raw stage authenticated and copied. A legacy
/// raw stage is returned too, still open, for its conversion.
fn open_kept_working(
    store: &HubStore,
    vehicle_id: Uuid,
    kept: &crate::db::PhysicalV3Kept,
    binding: &ProjectionBinding,
) -> Result<(crate::source_db_stage::OwnedSourceDbStage, Option<SourceDbStage>), String> {
    if let Some(archive) = &kept.archive {
        let working = SourceDbStage::decode_kept_archive(
            physical_kept_dir(store),
            &kept_archive_name(vehicle_id, archive.generation),
            crate::source_db_stage::KeptArchiveExpected {
                identity: kept_archive_identity(binding),
                logical: &kept.stage,
                digest: &archive.digest,
                bytes: archive.bytes,
                decoded_bytes: archive.decoded_bytes,
            },
        )
        .map_err(|error| format!("the kept archive: {error}"))?;
        return Ok((crate::source_db_stage::OwnedSourceDbStage::new(working), None));
    }
    // The kept stage is authenticated by its recorded logical digest and
    // only copied. The run's one full integrity check is of the planned
    // copy, after its last write, as the publication reads it (M08-T19).
    let kept_stage = SourceDbStage::open_sealed_to_copy(kept_stage_path(store, vehicle_id))
        .map_err(|error| format!("the kept stage: {error}"))?;
    match kept_stage.sealed_logical_digest() {
        Ok(digest) if digest.to_string() == kept.stage => {}
        Ok(_) => return Err("the kept stage differs from its record".into()),
        Err(error) => return Err(format!("the kept stage: {error}")),
    }
    let working = kept_stage
        .copy_sealed_into(physical_kept_dir(store))
        .map_err(|error| format!("a copy of the kept stage: {error}"))?;
    Ok((
        crate::source_db_stage::OwnedSourceDbStage::new(working),
        Some(kept_stage),
    ))
}

/// Converts a legacy kept raw stage, already authenticated against `kept`,
/// to a kept archive (DEC-250): the new pair is written durably, one
/// transaction replaces the record only while it is still `kept`, then the
/// legacy files go. The recorded plan inputs stay: the stage's logical
/// digest is unchanged. A failure leaves the legacy base as it was.
fn try_convert_legacy_kept(
    store: &HubStore,
    publication_gate: &PublicationGate,
    kept: &crate::db::PhysicalV3Kept,
    binding: &ProjectionBinding,
    legacy: &SourceDbStage,
    source: &[u8],
) -> Result<(), String> {
    let vehicle_id = binding.vehicle_id;
    // Files an earlier refused attempt left go before this one writes more.
    remove_unnamed_kept_files(store, vehicle_id);
    let converted = (|| -> Result<(), String> {
        let (archive, logical) = write_kept_pair(store, binding, legacy, source)?;
        let replaced = logical == kept.stage
            && store
                .swap_physical_v3_kept(
                    publication_gate,
                    vehicle_id,
                    Some(kept),
                    Some(&crate::db::PhysicalV3Kept {
                        stage: kept.stage.clone(),
                        source: kept.source.clone(),
                        archive: Some(archive),
                    }),
                    false,
                )
                .map_err(|error| error.to_string())?;
        if !replaced {
            return Err("the kept record changed or the archive's rows differ".into());
        }
        kept_crash_point("swap");
        let now = store
            .physical_v3_kept(vehicle_id)
            .map_err(|error| error.to_string())?;
        remove_superseded_kept_files(store, vehicle_id, now.as_ref());
        Ok(())
    })();
    if converted.is_err() {
        remove_unnamed_kept_files(store, vehicle_id);
    }
    converted
}

/// Removes the vehicle's kept files the surviving catalogue record does not
/// name, under the held gate. A record that cannot be reread, or is gone,
/// keeps every file.
fn remove_unnamed_kept_files(store: &HubStore, vehicle_id: Uuid) {
    if let Ok(Some(record)) = store.physical_v3_kept(vehicle_id) {
        remove_superseded_kept_files(store, vehicle_id, Some(&record));
    }
}

fn convert_legacy_kept(
    store: &HubStore,
    publication_gate: &PublicationGate,
    kept: &crate::db::PhysicalV3Kept,
    binding: &ProjectionBinding,
    legacy: &SourceDbStage,
    source: &[u8],
) {
    let vehicle_id = binding.vehicle_id;
    match try_convert_legacy_kept(store, publication_gate, kept, binding, legacy, source) {
        Ok(()) => tracing::info!(%vehicle_id, "the kept raw stage was converted to a kept archive"),
        Err(reason) => tracing::warn!(
            %vehicle_id,
            %reason,
            "the kept raw stage stays as it is; its conversion is retried at the next handback"
        ),
    }
}

/// Converts every live legacy kept raw base to a kept archive before a
/// backup snapshot is taken. The caller holds the publication gate. Each base
/// is authenticated (source sessions and stage against their record) and goes
/// through the same verified conversion as a handback's; a failure (space,
/// corruption, a changed record) is returned with the vehicle and the reason,
/// and leaves that base, its record and the files as they were.
pub(crate) fn convert_legacy_kept_bases(
    store: &HubStore,
    publication_gate: &PublicationGate,
) -> Result<(), (Uuid, String)> {
    let vehicles = store
        .physical_v3_kept_vehicles()
        .map_err(|error| (Uuid::nil(), error.to_string()))?;
    for vehicle_id in vehicles {
        let convert = || -> Result<(), String> {
            let Some(kept) = store.physical_v3_kept(vehicle_id).map_err(|e| e.to_string())? else {
                return Ok(());
            };
            if kept.archive.is_some() {
                return Ok(());
            }
            let source_bytes = std::fs::read(legacy_kept_source_path(store, vehicle_id))
                .map_err(|error| format!("the kept source sessions: {error}"))?;
            if Sha256Digest::of_bytes(&source_bytes).to_string() != kept.source {
                return Err("the kept source sessions differ from their record".into());
            }
            let source: KeptPhysicalSource = serde_json::from_slice(&source_bytes)
                .map_err(|error| format!("the kept source sessions: {error}"))?;
            if source.binding.vehicle_id != vehicle_id {
                return Err("the kept source sessions name another vehicle".into());
            }
            let legacy = SourceDbStage::open_sealed_to_copy(kept_stage_path(store, vehicle_id))
                .map_err(|error| format!("the kept stage: {error}"))?;
            match legacy.sealed_logical_digest() {
                Ok(digest) if digest.to_string() == kept.stage => {}
                Ok(_) => return Err("the kept stage differs from its record".into()),
                Err(error) => return Err(format!("the kept stage: {error}")),
            }
            try_convert_legacy_kept(
                store,
                publication_gate,
                &kept,
                &source.binding,
                &legacy,
                &source_bytes,
            )
        };
        convert().map_err(|reason| (vehicle_id, reason))?;
        tracing::info!(%vehicle_id, "a legacy kept raw stage was converted before a backup");
    }
    Ok(())
}

/// Writes `bytes` to `path` through a private temporary file and a rename.
fn write_private_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    written.map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        error.to_string()
    })
}

/// Forgets what is kept for the vehicle: the record, then the files. The files
/// go only against the record the catalogue actually holds afterwards (read
/// again), so a failed removal never unlinks what a surviving record names; a
/// failure to remove or to read again is returned and leaves every file.
fn forget_physical_kept(
    store: &HubStore,
    publication_gate: &PublicationGate,
    vehicle_id: Uuid,
) -> Result<(), String> {
    #[cfg(test)]
    let injected = FAIL_KEPT_REMOVAL.with(|fail| fail.replace(false));
    #[cfg(not(test))]
    let injected = false;
    let removed = if injected {
        Err("the kept record removal failed (injected)".to_string())
    } else {
        store
            .record_physical_v3_kept(publication_gate, vehicle_id, None)
            .map_err(|error| error.to_string())
    };
    let plan = store
        .record_physical_v3_handback_plan(publication_gate, vehicle_id, None)
        .map_err(|error| error.to_string());
    let surviving = store.physical_v3_kept(vehicle_id).map_err(|error| error.to_string())?;
    remove_superseded_kept_files(store, vehicle_id, surviving.as_ref());
    removed?;
    plan
}

#[cfg(test)]
thread_local! {
    /// Fails the next kept-record removal of `forget_physical_kept` (tests).
    pub(super) static FAIL_KEPT_REMOVAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Fails a handback right after its store transaction commits (tests).
    static FAIL_AFTER_HANDBACK_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Plans and publishes, after a handback, the physical 1.4 head of every
/// vehicle with a kept stage whose current head is refused (M03-T22). The
/// caller holds the publication gate. A vehicle that cannot be planned keeps
/// its refusal and the next vehicle goes on.
pub(crate) fn publish_physical_after_handback(
    store: &HubStore,
    cursor_key: &CursorKey,
    publication_gate: &PublicationGate,
) -> Result<Vec<(Uuid, HandbackPhysicalOutcome)>, SourceDbImportError> {
    lower_handback_thread_priority();
    // Issue #60: an interrupted import's publication is finished first, from
    // its capture, so no handback plans an older replay base in its place.
    for (vehicle_id, outcome) in resume_import_publications(store, cursor_key, publication_gate)? {
        tracing::info!(%vehicle_id, ?outcome, "import publication resumed before the handback");
    }
    let mut outcomes = Vec::new();
    for vehicle_id in store.physical_v3_kept_vehicles()? {
        let outcome =
            publish_vehicle_after_handback(store, cursor_key, publication_gate, vehicle_id)?;
        match &outcome {
            HandbackPhysicalOutcome::Restates => tracing::info!(
                %vehicle_id,
                "the window's plan restates an imported row; the 1.4 head stays refused until \
                 the next import"
            ),
            HandbackPhysicalOutcome::Withheld(reason) => tracing::warn!(
                %vehicle_id,
                %reason,
                "the 1.4 head was not planned after the handback; it stays refused until the \
                 next import"
            ),
            _ => {}
        }
        outcomes.push((vehicle_id, outcome));
    }
    Ok(outcomes)
}

fn publish_vehicle_after_handback(
    store: &HubStore,
    cursor_key: &CursorKey,
    publication_gate: &PublicationGate,
    vehicle_id: Uuid,
) -> Result<HandbackPhysicalOutcome, SourceDbImportError> {
    let Some(kept) = store.physical_v3_kept(vehicle_id)? else {
        return Ok(HandbackPhysicalOutcome::Pruned);
    };
    // Issue #60: while an import's publication is pending its capture may
    // already be the kept base, and the restart that finishes it needs every
    // file, so nothing is pruned or planned until it is resolved.
    if store.import_publication_holds()?.pending(vehicle_id) {
        return Ok(HandbackPhysicalOutcome::Withheld(
            "an import's publication is pending; the next start or window close finishes it"
                .into(),
        ));
    }
    if store.collection_coverage_unreconciled()? {
        return Ok(HandbackPhysicalOutcome::Withheld(
            "a window's handback has not reached the store's coverage yet".into(),
        ));
    }
    match store.physical_v3_current_head_served(vehicle_id)? {
        None => {
            return match forget_physical_kept(store, publication_gate, vehicle_id) {
                Ok(()) => Ok(HandbackPhysicalOutcome::Pruned),
                Err(reason) => Ok(HandbackPhysicalOutcome::Withheld(format!(
                    "the kept record was not forgotten: {reason}"
                ))),
            };
        }
        Some(true) => return Ok(HandbackPhysicalOutcome::Current),
        Some(false) => {}
    }
    // Issue #60: after a committed import only its own adopted base may be
    // planned, and the head is stamped with that import's revision.
    let revision = match store.physical_v3_import_revision(vehicle_id)? {
        None => None,
        Some(revision)
            if revision.kept.is_some()
                && revision.kept == kept.archive.as_ref().map(|archive| archive.generation)
                && store.import_publication_intent(vehicle_id)?.is_none() =>
        {
            Some(revision.run_id)
        }
        Some(_) => {
            return Ok(HandbackPhysicalOutcome::Withheld(
                "the kept base is not the committed import's; the next import publishes the head"
                    .into(),
            ));
        }
    };
    // An interrupted keep or conversion left files the record does not name.
    remove_superseded_kept_files(store, vehicle_id, Some(&kept));
    let withheld = |reason: String| Ok(HandbackPhysicalOutcome::Withheld(reason));
    let source_bytes = match std::fs::read(kept_source_path(store, vehicle_id, &kept)) {
        Ok(bytes) if Sha256Digest::of_bytes(&bytes).to_string() == kept.source => bytes,
        Ok(_) => return withheld("the kept source sessions differ from their record".into()),
        Err(error) => return withheld(format!("the kept source sessions: {error}")),
    };
    let source: KeptPhysicalSource = match serde_json::from_slice(&source_bytes) {
        Ok(source) => source,
        Err(error) => return withheld(format!("the kept source sessions: {error}")),
    };
    if source.binding.vehicle_id != vehicle_id {
        return withheld("the kept source sessions name another vehicle".into());
    }
    let (run, committed) = store.import_seam_for_handback(
        publication_gate,
        vehicle_id,
        source.binding.selected_car_id,
        &source.drives,
        &source.charges,
    )?;
    if !committed {
        return Ok(HandbackPhysicalOutcome::Restates);
    }
    #[cfg(test)]
    if FAIL_AFTER_HANDBACK_COMMIT.with(|fail| fail.replace(false)) {
        return Err(StoreError::ImportGenerationConflict.into());
    }
    if let Some(recorded) =
        serve_unchanged_head(store, publication_gate, vehicle_id, &kept, &source.binding, &run.ahead.packs)?
    {
        if !recorded.prepared {
            prepare_month_of_served_head(
                store,
                cursor_key,
                publication_gate,
                &kept,
                &source.binding,
                &run.ahead.packs,
                recorded,
            );
        }
        return Ok(HandbackPhysicalOutcome::Published(
            crate::physical_publication::PhysicalV3PublicationKind::Unchanged,
        ));
    }
    let (working, legacy) = match open_kept_working(store, vehicle_id, &kept, &source.binding) {
        Ok(opened) => opened,
        Err(reason) => return withheld(reason),
    };
    let binding = source.binding.clone();
    let outcome = match publish_planned_physical_route_at_revision(
        store,
        cursor_key,
        source.binding,
        Some(working),
        publication_gate,
        Some(&run.ahead.packs),
        None,
        Some(&kept.stage),
        revision,
    ) {
        Ok(Some(publication)) => Ok(HandbackPhysicalOutcome::Published(publication.kind)),
        Ok(None) => withheld("the window's plan cannot be held by the physical schema".into()),
        // The planned copy failed its check or could not be read: this
        // vehicle keeps its refusal, as for a damaged kept stage.
        Err(SourceDbImportError::PhysicalPublication(
            crate::physical_publication::SourceDbPhysicalPublicationError::Stage(error),
        )) => withheld(format!("the planned copy of the kept stage: {error}")),
        Err(error) => Err(error),
    };
    if let Some(legacy) = legacy {
        convert_legacy_kept(store, publication_gate, &kept, &binding, &legacy, &source_bytes);
    }
    outcome
}

/// The early answer of a handback (M08-T19): when the vehicle's current head
/// was planned from inputs whose digest the inputs now have, its plan would
/// make the same stage again, so the same snapshot, and the full path would
/// end as `Unchanged` after recording the head with the new coverage. This
/// records it so, without opening, copying or planning the kept stage or
/// reading a pack, and returns the record. A head without recorded inputs
/// (a store an earlier build last planned) is planned in full, which
/// records them.
fn serve_unchanged_head(
    store: &HubStore,
    publication_gate: &PublicationGate,
    vehicle_id: Uuid,
    kept: &crate::db::PhysicalV3Kept,
    binding: &ProjectionBinding,
    packs: &crate::import::source_db::seam::SeamPackPlan,
) -> Result<Option<crate::db::PhysicalV3HandbackPlan>, SourceDbImportError> {
    let Some(recorded) = store.physical_v3_handback_plan(vehicle_id)? else {
        return Ok(None);
    };
    // Only a planned head is recorded, and only a planned head may be
    // served again: a vehicle without the seam publishes its raw stage.
    if !store.import_seam_present(vehicle_id)? {
        return Ok(None);
    }
    let Some(current) = store.physical_v3_current_snapshot_id(vehicle_id)? else {
        return Ok(None);
    };
    if current.to_string() != recorded.snapshot_id {
        return Ok(None);
    }
    let stored = match stored_rows_digest(&store.import_seam_stored_rows(vehicle_id)?) {
        Ok(stored) => stored,
        Err(_) => return Ok(None),
    };
    let Some(now_ms) = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
    else {
        return Ok(None);
    };
    let inputs = handback_plan_inputs(&kept.stage, binding, packs, recorded.span, &stored, now_ms);
    if inputs != recorded.inputs {
        return Ok(None);
    }
    store.record_physical_v3_seam_planned_with_coverage(
        publication_gate,
        vehicle_id,
        current,
        &crate::db::coverage_digest(packs.coverage().intervals()),
    )?;
    tracing::info!(
        %vehicle_id,
        snapshot_id = %current,
        "the handback's plan inputs are those of the current 1.4 head; it is served under the \
         new coverage without planning it again"
    );
    Ok(Some(recorded))
}

/// The optional prepared month of a head the early answer served, when the
/// preparation recorded with its inputs had failed (M08-T19): the kept
/// stage is authenticated, copied and planned as the full path does, its
/// planned copy checked and confirmed to be the served head's stage, and
/// only the month is prepared from it; nothing is published and no pack of
/// the head is read. A failure keeps the record unprepared for the next
/// handback; the head stays served either way.
#[allow(clippy::too_many_arguments)]
fn prepare_month_of_served_head(
    store: &HubStore,
    cursor_key: &CursorKey,
    publication_gate: &PublicationGate,
    kept: &crate::db::PhysicalV3Kept,
    binding: &ProjectionBinding,
    packs: &crate::import::source_db::seam::SeamPackPlan,
    mut recorded: crate::db::PhysicalV3HandbackPlan,
) {
    let vehicle_id = binding.vehicle_id;
    let prepared = (|| -> Result<(), String> {
        let (mut working, legacy) = open_kept_working(store, vehicle_id, kept, binding)?;
        drop(legacy);
        let result = (|| -> Result<(), String> {
            plan_physical_stage(store, binding, working.stage_mut(), packs)
                .map_err(|error| format!("the plan: {error:?}"))?;
            let digest = working
                .stage_mut()
                .sealed_content_digest()
                .map_err(|error| format!("the planned copy of the kept stage: {error}"))?;
            let snapshot_id =
                crate::physical_publication::physical_v3_snapshot_id(digest, binding);
            if snapshot_id.to_string() != recorded.snapshot_id {
                return Err("the plan is not the served head's".into());
            }
            let admission = store
                .pending_physical_v3_control_admission_for_vehicle(vehicle_id)
                .map_err(|error| error.to_string())?
                .filter(|admission| admission.snapshot_id == snapshot_id)
                .ok_or_else(|| "the head is not served".to_owned())?;
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|value| i64::try_from(value.as_millis()).ok())
                .ok_or_else(|| "the clock".to_owned())?;
            crate::sync::prepared_map::publish_selected_month(
                store,
                cursor_key,
                working.stage_mut(),
                &admission,
                now_ms,
            )
            .map_err(|error| error.to_string())
        })();
        let cleanup = working.take().discard();
        result?;
        cleanup.map_err(|error| error.to_string())
    })();
    match prepared {
        Ok(()) => {
            recorded.prepared = true;
            if let Err(error) =
                store.record_physical_v3_handback_plan(publication_gate, vehicle_id, Some(&recorded))
            {
                tracing::warn!(%vehicle_id, %error, "the prepared month was not recorded");
            }
        }
        Err(reason) => tracing::warn!(
            %vehicle_id,
            %reason,
            "optional prepared month is unavailable; local map generation remains authoritative"
        ),
    }
}

/// The thread the service runs a handback's publication on.
const HANDBACK_PUBLICATION_THREAD: &str = "hub-handback-publication";

/// Lowers the priority of the service's handback thread for the rest of its
/// life (M08-T19): it is that thread's only work. On Linux a thread's nice
/// value is set through its thread id; macOS sets a thread's QoS class only
/// through a C call this crate's forbid(unsafe_code) excludes, so there the
/// thread keeps its priority. Any other thread, such as a bounded
/// observation calling the handback inline, is left as it is.
fn lower_handback_thread_priority() {
    let handback_thread = std::thread::current().name() == Some(HANDBACK_PUBLICATION_THREAD);
    #[cfg(not(target_os = "linux"))]
    let _ = handback_thread;
    #[cfg(target_os = "linux")]
    if handback_thread {
        let thread = std::fs::read_link("/proc/thread-self")
            .ok()
            .and_then(|path| path.file_name()?.to_str()?.parse::<i32>().ok())
            .and_then(rustix::process::Pid::from_raw);
        if let Some(thread) = thread
            && let Err(error) = rustix::process::setpriority_process(Some(thread), 19)
        {
            tracing::debug!(%error, "the handback thread keeps its priority");
        }
    }
}

// The import publication intent and its restart (issue #60).
include!("import_publication.rs");
