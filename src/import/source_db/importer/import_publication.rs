// SPDX-License-Identifier: AGPL-3.0-only

// Coherent publication after an interrupted import (issue #60). Before an
// import's first catalogue commit it writes a private capture under the
// packs directory (the raw physical stage as a kept archive, the source
// sessions and the schema-2.2 source facts) and records an intent naming it.
// The commit makes the import the vehicle's import revision, which refuses
// every earlier physical head. Publication then adopts the capture as the
// kept replay base, publishes schema 2.2 and the physical head stamped with
// the revision, and clears the intent. A restart finishes a committed intent
// from the capture alone, under the publication gate; an uncommitted one is
// dropped, and a capture that cannot be used is refused, with the physical
// head kept refused until the next import.

/// The directory under the packs directory that holds import captures.
const IMPORT_PUBLICATION_DIRECTORY: &str = "import-publication";

fn import_publication_dir(store: &HubStore) -> std::path::PathBuf {
    store.packs_dir().join(IMPORT_PUBLICATION_DIRECTORY)
}

fn import_publication_updates_name(vehicle_id: Uuid, generation: Uuid) -> String {
    format!("{vehicle_id}.{generation}.updates.json")
}

/// The schema-2.2 source facts of one import, in a form that reads back.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CapturedUpdatesV2_2 {
    postgres_snapshot_sha256: String,
    observed_migration_version: i64,
    observed_migration_count: usize,
    minimum_supported_migration_version: i64,
    maximum_validated_migration_version: i64,
    pinned_source_revision: String,
    pinned_migration_set_sha256: String,
    fingerprint: String,
    global_settings: crate::source_db_projection::SourceDbSettingsPhysicalV2_2,
    car: crate::source_db_projection::SourceDbCarPhysicalV2_2,
    car_settings: crate::source_db_projection::SourceDbCarSettingsPhysicalV2_2,
    updates: Vec<crate::source_db_projection::SourceDbUpdatePhysicalV2_2>,
}

impl CapturedUpdatesV2_2 {
    fn of(source: &DirectUpdatesSourceV2_2) -> Self {
        Self {
            postgres_snapshot_sha256: source.postgres_snapshot_sha256.clone(),
            observed_migration_version: source.schema.observed_migration_version,
            observed_migration_count: source.schema.observed_migration_count,
            minimum_supported_migration_version: source.schema.minimum_supported_migration_version,
            maximum_validated_migration_version: source.schema.maximum_validated_migration_version,
            pinned_source_revision: source.schema.pinned_source_revision.to_owned(),
            pinned_migration_set_sha256: source.schema.pinned_migration_set_sha256.to_owned(),
            fingerprint: source.schema.fingerprint.clone(),
            global_settings: source.global_settings.clone(),
            car: source.car.clone(),
            car_settings: source.car_settings.clone(),
            updates: source.updates.clone(),
        }
    }

    /// The captured facts, only under the reviewed source this build pins.
    fn into_source(self) -> Result<DirectUpdatesSourceV2_2, String> {
        if self.pinned_source_revision != crate::source_db_schema::SOURCE_DB_V4_SOURCE_REVISION
            || self.pinned_migration_set_sha256
                != crate::source_db_schema::SOURCE_DB_V4_MIGRATION_SET_SHA256
        {
            return Err("the capture was made under another reviewed source".into());
        }
        Ok(DirectUpdatesSourceV2_2 {
            postgres_snapshot_sha256: self.postgres_snapshot_sha256,
            schema: crate::source_db_reader::SourceDbSchemaInfo {
                observed_migration_version: self.observed_migration_version,
                observed_migration_count: self.observed_migration_count,
                minimum_supported_migration_version: self.minimum_supported_migration_version,
                maximum_validated_migration_version: self.maximum_validated_migration_version,
                pinned_source_revision: crate::source_db_schema::SOURCE_DB_V4_SOURCE_REVISION,
                pinned_migration_set_sha256:
                    crate::source_db_schema::SOURCE_DB_V4_MIGRATION_SET_SHA256,
                fingerprint: self.fingerprint,
            },
            global_settings: self.global_settings,
            car: self.car,
            car_settings: self.car_settings,
            updates: self.updates,
        })
    }
}

/// A simulated crash at a named boundary of an import's publication (tests
/// catch the panic): `captured` after the intent is recorded, `committed`
/// after the lineage commit, `schema_22` after schema 2.2, `adopted` after
/// the kept record names the capture, `published` after the physical head.
fn import_publication_crash_point(point: &'static str) {
    #[cfg(test)]
    if IMPORT_PUBLICATION_CRASH_AT.with(|at| at.get() == Some(point)) {
        IMPORT_PUBLICATION_CRASH_AT.with(|at| at.set(None));
        panic!("simulated crash at import publication point {point}");
    }
    #[cfg(not(test))]
    let _ = point;
}

#[cfg(test)]
thread_local! {
    pub(super) static IMPORT_PUBLICATION_CRASH_AT: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

fn sync_directory(path: &std::path::Path) -> Result<(), String> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

fn ensure_private_dir(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// The vehicle and generation a capture file belongs to, by name.
fn capture_file_owner(name: &str) -> Option<(Uuid, Uuid)> {
    let vehicle = Uuid::parse_str(name.get(..36)?).ok()?;
    if name.get(36..37)? != "." {
        return None;
    }
    let generation = Uuid::parse_str(name.get(37..73)?).ok()?;
    name.get(73..74).filter(|dot| *dot == ".")?;
    Some((vehicle, generation))
}

/// Removes the capture files no pending publication holds (an interrupted
/// capture, or one whose intent was resolved), then what an interrupted
/// capture left in the capture staging folder. The caller holds the gate.
fn sweep_import_publication_files(store: &HubStore) {
    let Ok(holds) = store.import_publication_holds() else {
        return;
    };
    let directory = import_publication_dir(store);
    if let Ok(entries) = std::fs::read_dir(&directory) {
        let mut removed = false;
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some((vehicle_id, generation)) = capture_file_owner(&name) else {
                continue;
            };
            if holds.holds(vehicle_id, generation) {
                continue;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_file())
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed = true;
            }
        }
        if removed {
            let _ = sync_directory(&directory);
        }
    }
    sweep_capture_staging(&directory.join(".staging"));
}

/// Removes the compacted candidates (`<uuid>.sqlite` and SQLite's side
/// files) an interrupted capture left in the capture staging folder, which
/// no intent ever names. A capture writes its candidate only while it holds
/// the publication gate and removes it before the capture returns, and every
/// sweep runs under that gate, so no live run's candidate is found here.
/// Other names are left.
fn sweep_capture_staging(staging: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(staging) else {
        return;
    };
    let mut removed = false;
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let database = ["-journal", "-wal", "-shm"]
            .iter()
            .find_map(|side| name.strip_suffix(side))
            .unwrap_or(&name);
        let candidate = database
            .strip_suffix(".sqlite")
            .is_some_and(|id| id.len() == 36 && Uuid::parse_str(id).is_ok());
        if candidate
            && entry.file_type().is_ok_and(|kind| kind.is_file())
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed = true;
        }
    }
    if removed {
        let _ = sync_directory(staging);
    }
}

/// Writes the import's capture durably and records its intent, before the
/// import's first commit (issue #60). A failure leaves nothing named and the
/// import must not commit.
#[allow(clippy::too_many_arguments)]
fn capture_import_publication(
    store: &HubStore,
    publication_gate: &PublicationGate,
    run_id: Uuid,
    binding: &ProjectionBinding,
    stage: &SourceDbStage,
    drives: &[ProjectionDrive],
    charges: &[ProjectionCharge],
    updates: &DirectUpdatesSourceV2_2,
) -> Result<crate::db::ImportPublicationIntent, SourceDbImportError> {
    let vehicle_id = binding.vehicle_id;
    sweep_import_publication_files(store);
    let directory = import_publication_dir(store);
    let generation = Uuid::new_v4();
    let written = (|| -> Result<crate::db::ImportPublicationIntent, String> {
        let source = serde_json::to_vec(&KeptPhysicalSource {
            binding: binding.clone(),
            drives: drives.to_vec(),
            charges: charges.to_vec(),
        })
        .map_err(|error| error.to_string())?;
        let updates_bytes =
            serde_json::to_vec(&CapturedUpdatesV2_2::of(updates)).map_err(|error| error.to_string())?;
        let receipt = stage
            .write_kept_archive(
                &directory,
                &kept_archive_name(vehicle_id, generation),
                kept_archive_identity(binding),
            )
            .map_err(|error| error.to_string())?;
        write_private_file(
            &directory.join(kept_generation_source_name(vehicle_id, generation)),
            &source,
        )?;
        write_private_file(
            &directory.join(import_publication_updates_name(vehicle_id, generation)),
            &updates_bytes,
        )?;
        sync_directory(&directory)?;
        let intent = crate::db::ImportPublicationIntent {
            run_id,
            binding: binding.clone(),
            kept: crate::db::PhysicalV3Kept {
                stage: receipt.logical.to_string(),
                source: Sha256Digest::of_bytes(&source).to_string(),
                archive: Some(crate::db::PhysicalV3KeptArchive {
                    generation,
                    format: receipt.format,
                    digest: receipt.digest.to_string(),
                    bytes: receipt.bytes,
                    decoded_bytes: receipt.decoded_bytes,
                }),
            },
            updates: Sha256Digest::of_bytes(&updates_bytes).to_string(),
            committed: false,
        };
        store
            .record_import_publication_intent(publication_gate, &intent)
            .map_err(|error| error.to_string())?;
        Ok(intent)
    })();
    match written {
        Ok(intent) => {
            import_publication_crash_point("captured");
            Ok(intent)
        }
        Err(reason) => {
            sweep_import_publication_files(store);
            Err(SourceDbImportError::PublicationCapture { vehicle_id, reason })
        }
    }
}

/// Makes a committed import's capture the vehicle's kept replay base: its
/// pair is moved into the kept directory (a pair moved before an interrupted
/// swap is found there), then one transaction names it and notes it on the
/// import revision. Returns the kept stage's recorded digest.
fn adopt_import_publication_capture(
    store: &HubStore,
    publication_gate: &PublicationGate,
    intent: &crate::db::ImportPublicationIntent,
) -> Result<String, String> {
    let vehicle_id = intent.binding.vehicle_id;
    let archive = intent
        .kept
        .archive
        .as_ref()
        .ok_or_else(|| "the capture names no kept archive".to_owned())?;
    let current = store
        .physical_v3_kept(vehicle_id)
        .map_err(|error| error.to_string())?;
    if current.as_ref() != Some(&intent.kept) {
        let from = import_publication_dir(store);
        let to = physical_kept_dir(store);
        ensure_private_dir(&to)?;
        for name in [
            kept_archive_name(vehicle_id, archive.generation),
            kept_generation_source_name(vehicle_id, archive.generation),
        ] {
            let source = from.join(&name);
            let target = to.join(&name);
            match std::fs::symlink_metadata(&source) {
                Ok(metadata) if metadata.is_file() => {
                    std::fs::rename(&source, &target).map_err(|error| error.to_string())?;
                }
                Ok(_) => return Err(format!("the capture file {name} is not a regular file")),
                Err(_) if std::fs::symlink_metadata(&target).is_ok_and(|meta| meta.is_file()) => {}
                Err(_) => return Err(format!("the capture file {name} is missing")),
            }
        }
        sync_directory(&to)?;
        sync_directory(&from)?;
        import_publication_crash_point("moved");
    }
    if !store
        .adopt_import_publication_kept(
            publication_gate,
            vehicle_id,
            current.as_ref(),
            &intent.kept,
            intent.run_id,
        )
        .map_err(|error| error.to_string())?
    {
        return Err("the kept record changed while the capture was adopted".into());
    }
    import_publication_crash_point("adopted");
    remove_superseded_kept_files(store, vehicle_id, Some(&intent.kept));
    Ok(intent.kept.stage.clone())
}

/// Publishes the physical head of a committed import, stamped with its
/// revision, then clears its intent: a withheld head clears it too, leaving
/// every earlier head refused.
fn publish_import_physical(
    store: &HubStore,
    cursor_key: &CursorKey,
    publication_gate: &PublicationGate,
    intent: &crate::db::ImportPublicationIntent,
    stage: crate::source_db_stage::OwnedSourceDbStage,
    seam: Option<&crate::import::source_db::seam::SeamPackPlan>,
    adopted: Option<&str>,
) -> Result<Option<crate::physical_publication::PhysicalV3Publication>, SourceDbImportError> {
    let publication = publish_planned_physical_route_at_revision(
        store,
        cursor_key,
        intent.binding.clone(),
        Some(stage),
        publication_gate,
        seam,
        None,
        adopted,
        Some(intent.run_id),
    )?;
    import_publication_crash_point("published");
    store.clear_import_publication_intent(
        publication_gate,
        intent.binding.vehicle_id,
        Some(intent.run_id),
    )?;
    sweep_import_publication_files(store);
    Ok(publication)
}

/// The physical half of a committed import's publication in its own run:
/// the capture is adopted, then the head is published from the stage the
/// import holds. A failed adoption leaves the intent and the capture where
/// they are, wherever the files were moved to, and the publication
/// unresolved: the next start, window close or import adopts it again
/// before it resolves the publication.
fn finish_import_physical(
    store: &HubStore,
    cursor_key: &CursorKey,
    publication_gate: &PublicationGate,
    intent: &crate::db::ImportPublicationIntent,
    stage: Option<crate::source_db_stage::OwnedSourceDbStage>,
    seam: Option<&crate::import::source_db::seam::SeamPackPlan>,
) -> Result<Option<crate::physical_publication::PhysicalV3Publication>, SourceDbImportError> {
    let vehicle_id = intent.binding.vehicle_id;
    let adopted = match adopt_import_publication_capture(store, publication_gate, intent) {
        Ok(stage) => Some(stage),
        Err(reason) => {
            tracing::warn!(
                %vehicle_id,
                %reason,
                "the import's capture was not kept; the 1.4 head stays refused until the \
                 publication is finished from the capture"
            );
            return Err(SourceDbImportError::PublicationAdoption { vehicle_id, reason });
        }
    };
    match stage {
        Some(stage) => publish_import_physical(
            store,
            cursor_key,
            publication_gate,
            intent,
            stage,
            seam,
            adopted.as_deref(),
        ),
        None => {
            store.clear_import_publication_intent(publication_gate, vehicle_id, Some(intent.run_id))?;
            sweep_import_publication_files(store);
            Ok(None)
        }
    }
}

/// What a restart did with one vehicle's recorded import publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportPublicationOutcome {
    /// The import never committed: its intent and capture were dropped and
    /// the heads it would have replaced stand.
    Abandoned,
    /// Schema 2.2 and the physical head were finished from the capture;
    /// `None` when the physical head was withheld (it stays refused).
    Finished(Option<crate::physical_publication::PhysicalV3PublicationKind>),
    /// The capture cannot finish the publication: the physical head stays
    /// refused until the next import of the vehicle. The reason names what
    /// failed.
    Refused(String),
}

/// Finishes, under the publication gate the caller holds, every import
/// publication an interrupted run left (issue #60): from its capture alone,
/// never the source. A refusal of one vehicle does not stop the next.
pub(crate) fn resume_import_publications(
    store: &HubStore,
    cursor_key: &CursorKey,
    publication_gate: &PublicationGate,
) -> Result<Vec<(Uuid, ImportPublicationOutcome)>, SourceDbImportError> {
    let mut outcomes = Vec::new();
    for (vehicle_id, intent) in store.import_publication_intents()? {
        let outcome = match intent {
            None => ImportPublicationOutcome::Refused(
                "the import's publication intent does not read back".into(),
            ),
            Some(intent) => resume_import_publication(store, cursor_key, publication_gate, intent)?,
        };
        match &outcome {
            ImportPublicationOutcome::Refused(reason) => tracing::warn!(
                %vehicle_id,
                %reason,
                "an interrupted import's publication cannot be finished from its capture; the \
                 1.4 head stays refused until the next import"
            ),
            outcome => tracing::info!(%vehicle_id, ?outcome, "an interrupted import's publication"),
        }
        outcomes.push((vehicle_id, outcome));
    }
    sweep_import_publication_files(store);
    Ok(outcomes)
}

fn resume_import_publication(
    store: &HubStore,
    cursor_key: &CursorKey,
    publication_gate: &PublicationGate,
    intent: crate::db::ImportPublicationIntent,
) -> Result<ImportPublicationOutcome, SourceDbImportError> {
    let vehicle_id = intent.binding.vehicle_id;
    let current = store
        .physical_v3_import_revision(vehicle_id)?
        .is_some_and(|revision| revision.run_id == intent.run_id);
    if !intent.committed || !current {
        store.clear_import_publication_intent(publication_gate, vehicle_id, Some(intent.run_id))?;
        return Ok(ImportPublicationOutcome::Abandoned);
    }
    let refused = |reason: String| Ok(ImportPublicationOutcome::Refused(reason));
    let Some(archive) = intent.kept.archive.as_ref() else {
        return refused("the capture names no kept archive".into());
    };
    // The capture's files, wherever an earlier attempt left them.
    let read_capture = |name: String| -> Result<Vec<u8>, String> {
        let captured = import_publication_dir(store).join(&name);
        let kept = physical_kept_dir(store).join(&name);
        std::fs::read(&captured)
            .or_else(|_| std::fs::read(&kept))
            .map_err(|error| format!("the capture file {name}: {error}"))
    };
    let updates = match read_capture(import_publication_updates_name(vehicle_id, archive.generation)) {
        Ok(bytes) if Sha256Digest::of_bytes(&bytes).to_string() == intent.updates => bytes,
        Ok(_) => return refused("the captured schema-2.2 facts differ from their record".into()),
        Err(reason) => return refused(reason),
    };
    let updates = match serde_json::from_slice::<CapturedUpdatesV2_2>(&updates)
        .map_err(|error| error.to_string())
        .and_then(CapturedUpdatesV2_2::into_source)
    {
        Ok(updates) => updates,
        Err(reason) => return refused(format!("the captured schema-2.2 facts: {reason}")),
    };
    let source = match read_capture(kept_generation_source_name(vehicle_id, archive.generation)) {
        Ok(bytes) if Sha256Digest::of_bytes(&bytes).to_string() == intent.kept.source => bytes,
        Ok(_) => return refused("the captured source sessions differ from their record".into()),
        Err(reason) => return refused(reason),
    };
    let source = match serde_json::from_slice::<KeptPhysicalSource>(&source) {
        Ok(source) if source.binding == intent.binding => source,
        Ok(_) => return refused("the captured source sessions name another binding".into()),
        Err(error) => return refused(format!("the captured source sessions: {error}")),
    };

    // Schema 2.2, as the import's own run publishes it after its commit.
    let Some((base_snapshot_id, head_sequence, _)) = store.v2_head(vehicle_id)? else {
        return refused("the committed import's lineage is gone".into());
    };
    let head_sequence =
        u64::try_from(head_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
    let expected = match production_updates_head(store, vehicle_id) {
        Ok(expected) => expected,
        Err(error) => return refused(format!("the schema-2.2 head: {error}")),
    };
    if let Err(error) = publish_production_updates_schema_22_with_gate(
        store,
        cursor_key,
        &intent.binding,
        updates,
        publication_gate,
        &expected,
        Some((base_snapshot_id, head_sequence)),
    ) {
        return refused(format!("the schema-2.2 publication: {error}"));
    }
    import_publication_crash_point("schema_22");

    // The seam the committed lineage proved, planned again from the same
    // source sessions: the commit already applied it, so nothing restates.
    let seam = if store.import_seam_present(vehicle_id)? {
        let (run, applied) = store.import_seam_for_handback(
            publication_gate,
            vehicle_id,
            intent.binding.selected_car_id,
            &source.drives,
            &source.charges,
        )?;
        if !applied {
            return refused("the import's seam plan now restates an imported row".into());
        }
        Some(run.ahead.packs)
    } else {
        None
    };
    let adopted = match adopt_import_publication_capture(store, publication_gate, &intent) {
        Ok(stage) => stage,
        Err(reason) => return refused(format!("the capture was not adopted: {reason}")),
    };
    let (working, _) = match open_kept_working(store, vehicle_id, &intent.kept, &intent.binding) {
        Ok(opened) => opened,
        Err(reason) => return refused(reason),
    };
    match publish_import_physical(
        store,
        cursor_key,
        publication_gate,
        &intent,
        working,
        seam.as_ref(),
        Some(&adopted),
    ) {
        Ok(publication) => Ok(ImportPublicationOutcome::Finished(
            publication.map(|publication| publication.kind),
        )),
        Err(SourceDbImportError::PhysicalPublication(
            crate::physical_publication::SourceDbPhysicalPublicationError::Stage(error),
        )) => refused(format!("the captured stage: {error}")),
        Err(error) => Err(error),
    }
}
