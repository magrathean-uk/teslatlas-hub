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

/// The kept raw stage: a private sealed stage under `.staging`, as
/// [`SourceDbStage::open_sealed`] requires.
fn kept_stage_path(store: &HubStore, vehicle_id: Uuid) -> std::path::PathBuf {
    physical_kept_dir(store)
        .join(".staging")
        .join(format!("{vehicle_id}.sqlite"))
}

fn kept_source_path(store: &HubStore, vehicle_id: Uuid) -> std::path::PathBuf {
    physical_kept_dir(store).join(format!("{vehicle_id}.source.json"))
}

/// Keeps the sealed raw `stage` and the import's source sessions for the
/// vehicle's next handback, replacing what an earlier import kept. A keep
/// that fails forgets the vehicle's kept state, so a handback never plans
/// from an older import; the next import keeps again. Returns the kept
/// stage's recorded digest.
fn keep_physical_source(
    store: &HubStore,
    publication_gate: &PublicationGate,
    binding: &ProjectionBinding,
    stage: &SourceDbStage,
    drives: &[ProjectionDrive],
    charges: &[ProjectionCharge],
) -> Option<String> {
    let vehicle_id = binding.vehicle_id;
    let kept = (|| -> Result<crate::db::PhysicalV3Kept, String> {
        // The record goes first: a crash part-way leaves nothing to plan.
        store
            .record_physical_v3_kept(publication_gate, vehicle_id, None)
            .map_err(|error| error.to_string())?;
        store
            .record_physical_v3_handback_plan(publication_gate, vehicle_id, None)
            .map_err(|error| error.to_string())?;
        let copy = stage
            .copy_sealed_into(physical_kept_dir(store))
            .map_err(|error| error.to_string())?;
        let digest = copy.sealed_content_digest();
        let copied = copy.path().to_path_buf();
        drop(copy);
        let digest = match digest {
            Ok(digest) => digest,
            Err(error) => {
                let _ = std::fs::remove_file(&copied);
                return Err(error.to_string());
            }
        };
        std::fs::rename(&copied, kept_stage_path(store, vehicle_id)).map_err(|error| {
            let _ = std::fs::remove_file(&copied);
            error.to_string()
        })?;
        let source = serde_json::to_vec(&KeptPhysicalSource {
            binding: binding.clone(),
            drives: drives.to_vec(),
            charges: charges.to_vec(),
        })
        .map_err(|error| error.to_string())?;
        write_private_file(&kept_source_path(store, vehicle_id), &source)?;
        Ok(crate::db::PhysicalV3Kept {
            stage: digest.to_string(),
            source: Sha256Digest::of_bytes(&source).to_string(),
        })
    })();
    let recorded = kept.and_then(|kept| {
        store
            .record_physical_v3_kept(publication_gate, vehicle_id, Some(&kept))
            .map(|()| kept.stage)
            .map_err(|error| error.to_string())
    });
    match recorded {
        Ok(stage) => Some(stage),
        Err(reason) => {
        tracing::warn!(
            %vehicle_id,
            %reason,
            "the import's physical stage was not kept; a handback leaves the 1.4 head refused \
             until the next import"
        );
        forget_physical_kept(store, publication_gate, vehicle_id);
            None
        }
    }
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

/// Forgets what is kept for the vehicle: the record, then the files.
fn forget_physical_kept(store: &HubStore, publication_gate: &PublicationGate, vehicle_id: Uuid) {
    let _ = store.record_physical_v3_kept(publication_gate, vehicle_id, None);
    let _ = store.record_physical_v3_handback_plan(publication_gate, vehicle_id, None);
    let _ = std::fs::remove_file(kept_stage_path(store, vehicle_id));
    let _ = std::fs::remove_file(kept_source_path(store, vehicle_id));
}

#[cfg(test)]
thread_local! {
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
    if store.collection_coverage_unreconciled()? {
        return Ok(HandbackPhysicalOutcome::Withheld(
            "a window's handback has not reached the store's coverage yet".into(),
        ));
    }
    match store.physical_v3_current_head_served(vehicle_id)? {
        None => {
            forget_physical_kept(store, publication_gate, vehicle_id);
            return Ok(HandbackPhysicalOutcome::Pruned);
        }
        Some(true) => return Ok(HandbackPhysicalOutcome::Current),
        Some(false) => {}
    }
    let withheld = |reason: String| Ok(HandbackPhysicalOutcome::Withheld(reason));
    let source = match std::fs::read(kept_source_path(store, vehicle_id)) {
        Ok(bytes) if Sha256Digest::of_bytes(&bytes).to_string() == kept.source => bytes,
        Ok(_) => return withheld("the kept source sessions differ from their record".into()),
        Err(error) => return withheld(format!("the kept source sessions: {error}")),
    };
    let source: KeptPhysicalSource = match serde_json::from_slice(&source) {
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
    if serve_unchanged_head(store, publication_gate, vehicle_id, &kept, &source.binding, &run.ahead.packs)? {
        return Ok(HandbackPhysicalOutcome::Published(
            crate::physical_publication::PhysicalV3PublicationKind::Unchanged,
        ));
    }
    // The one full check of the kept stage in this handback (M08-T19):
    // its copy and the content digests after it rely on it.
    let kept_stage = match SourceDbStage::open_sealed(kept_stage_path(store, vehicle_id)) {
        Ok(stage) => stage,
        Err(error) => return withheld(format!("the kept stage: {error}")),
    };
    match kept_stage.sealed_content_digest() {
        Ok(digest) if digest.to_string() == kept.stage => {}
        Ok(_) => return withheld("the kept stage differs from its record".into()),
        Err(error) => return withheld(format!("the kept stage: {error}")),
    }
    let working = match kept_stage.copy_sealed_into(physical_kept_dir(store)) {
        Ok(stage) => crate::source_db_stage::OwnedSourceDbStage::new(stage),
        Err(error) => return withheld(format!("a copy of the kept stage: {error}")),
    };
    drop(kept_stage);
    match publish_planned_physical_route(
        store,
        cursor_key,
        source.binding,
        Some(working),
        publication_gate,
        Some(&run.ahead.packs),
        None,
        Some(&kept.stage),
    )? {
        Some(publication) => Ok(HandbackPhysicalOutcome::Published(publication.kind)),
        None => withheld("the window's plan cannot be held by the physical schema".into()),
    }
}

/// The early answer of a handback (M08-T19): when the vehicle's current head
/// was planned from inputs whose digest the inputs now have, its plan would
/// make the same stage again, so the same snapshot, and the full path would
/// end as `Unchanged` after recording the head with the new coverage. This
/// records it so, without opening, copying or planning the kept stage or
/// reading a pack, and says so. A head without recorded inputs (a store an
/// earlier build last planned) is planned in full, which records them.
fn serve_unchanged_head(
    store: &HubStore,
    publication_gate: &PublicationGate,
    vehicle_id: Uuid,
    kept: &crate::db::PhysicalV3Kept,
    binding: &ProjectionBinding,
    packs: &crate::import::source_db::seam::SeamPackPlan,
) -> Result<bool, SourceDbImportError> {
    let Some(recorded) = store.physical_v3_handback_plan(vehicle_id)? else {
        return Ok(false);
    };
    // Only a planned head is recorded, and only a planned head may be
    // served again: a vehicle without the seam publishes its raw stage.
    if !store.import_seam_present(vehicle_id)? {
        return Ok(false);
    }
    let Some(current) = store.physical_v3_current_snapshot_id(vehicle_id)? else {
        return Ok(false);
    };
    if current.to_string() != recorded.snapshot_id {
        return Ok(false);
    }
    let stored = match stored_rows_digest(&store.import_seam_stored_rows(vehicle_id)?) {
        Ok(stored) => stored,
        Err(_) => return Ok(false),
    };
    let Some(now_ms) = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
    else {
        return Ok(false);
    };
    let inputs = handback_plan_inputs(&kept.stage, binding, packs, recorded.span, &stored, now_ms);
    if inputs != recorded.inputs {
        return Ok(false);
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
    Ok(true)
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
