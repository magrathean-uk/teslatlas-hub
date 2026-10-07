// SPDX-License-Identifier: AGPL-3.0-only

//! Guarded publication of one complete sealed PhysicalV3 source stage.

use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    db::{
        HubStore, PendingPhysicalV3Admission, PhysicalV3PublicationState, PublicationGate,
        StoreError,
    },
    hub_pack::{ProjectionBinding, ProjectionPackError, ProjectionPackWriter},
    protocol::{CursorKey, ProtocolLimits, SequenceRange, Sha256Digest},
    teslamate_physical_fragments::{
        TeslaMatePhysicalFragmentError, TeslaMatePhysicalFragmentLimits,
        write_staged_physical_updates_snapshot_v3_with_limits,
    },
    teslamate_stage::{TeslaMateStage, TeslaMateStageError, TeslaMateStageFormat},
};

const MAX_IJSON_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalV3PublicationKind {
    FirstHead,
    Unchanged,
    Rotation,
    ResumedRotation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalV3Publication {
    pub kind: PhysicalV3PublicationKind,
    pub admission: PendingPhysicalV3Admission,
}

/// Consume one complete private stage and make its exact schema-2.2 snapshot
/// public. The stage is discarded after every outcome. A retry of the same
/// stage derives the same snapshot ID and either returns the public head or
/// finishes a rotation that committed before its activation became visible.
pub async fn publish_sealed_physical_v3_stage(
    store: &HubStore,
    cursor_key: &CursorKey,
    binding: ProjectionBinding,
    stage: TeslaMateStage,
) -> Result<PhysicalV3Publication, TeslaMatePhysicalPublicationError> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TeslaMatePhysicalPublicationError::Clock)?
        .as_millis();
    let now_ms = i64::try_from(now_ms).map_err(|_| TeslaMatePhysicalPublicationError::Clock)?;
    publish_sealed_physical_v3_stage_at(store, cursor_key, binding, stage, now_ms).await
}

async fn publish_sealed_physical_v3_stage_at(
    store: &HubStore,
    cursor_key: &CursorKey,
    binding: ProjectionBinding,
    stage: TeslaMateStage,
    now_ms: i64,
) -> Result<PhysicalV3Publication, TeslaMatePhysicalPublicationError> {
    let owned = crate::teslamate_stage::OwnedTeslaMateStage::new(stage);
    let gate = store.acquire_publication_gate().await?;
    publish_sealed_physical_v3_stage_with_gate_at(
        store,
        cursor_key,
        binding,
        owned.take(),
        &gate,
        now_ms,
    )
}

/// The normal importer already holds the publication gate from source capture
/// through both catalogues. Never release it or acquire it recursively here.
pub(crate) fn publish_sealed_physical_v3_stage_with_gate_at(
    store: &HubStore,
    cursor_key: &CursorKey,
    binding: ProjectionBinding,
    stage: TeslaMateStage,
    gate: &PublicationGate,
    now_ms: i64,
) -> Result<PhysicalV3Publication, TeslaMatePhysicalPublicationError> {
    let result = publish_sealed_physical_v3_stage_inner(
        store,
        cursor_key,
        binding,
        &stage,
        gate,
        now_ms,
        TeslaMatePhysicalFragmentLimits::default(),
    );
    if let Ok(publication) = &result
        && let Err(error) = crate::import::teslamate::prepared_map::publish_selected_month(
            store,
            cursor_key,
            &stage,
            &publication.admission,
            now_ms,
        )
    {
        tracing::warn!(%error, vehicle_id = %publication.admission.vehicle_id,
                "optional prepared month is unavailable; local map generation remains authoritative");
    }
    use crate::runtime::development_event_log as dev;
    let mut event = dev::Event::new(
        dev::Kind::Publication,
        if result.is_ok() {
            dev::Outcome::Complete
        } else {
            dev::Outcome::Failed
        },
    );
    if let Ok(publication) = &result {
        event.target_sequence = Some(publication.admission.head_sequence);
        event.chunks = publication.admission.manifest.chunk_count as u64;
        event.rows = publication.admission.manifest.total_rows;
    }
    dev::record(event);
    let cleanup = stage.discard();
    match (result, cleanup) {
        (Ok(publication), Ok(())) => Ok(publication),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(TeslaMatePhysicalPublicationError::StageCleanup(error)),
    }
}

#[derive(Debug)]
enum IntendedPublication {
    First,
    Rotate(PendingPhysicalV3Admission),
}

#[allow(clippy::too_many_arguments)]
fn publish_sealed_physical_v3_stage_inner(
    store: &HubStore,
    cursor_key: &CursorKey,
    binding: ProjectionBinding,
    stage: &TeslaMateStage,
    publication_gate: &PublicationGate,
    now_ms: i64,
    fragment_limits: TeslaMatePhysicalFragmentLimits,
) -> Result<PhysicalV3Publication, TeslaMatePhysicalPublicationError> {
    if stage.format()? != TeslaMateStageFormat::PhysicalV3 {
        return Err(TeslaMatePhysicalPublicationError::WrongStageFormat);
    }
    let stage_digest = stage.sealed_content_digest()?;
    let snapshot_id = physical_v3_snapshot_id(stage_digest, &binding);
    let state = store.physical_v3_publication_state_for_vehicle_at(binding.vehicle_id, now_ms)?;
    // Recover the already committed, fully verified successor before deciding
    // what to do with a fresh source snapshot. The source may have advanced
    // while the Hub was stopped; that must not strand the persisted head.
    let state = if let PhysicalV3PublicationState::Blocked { current, retained } = state {
        if binding.installation_id != current.installation_id
            || binding.account_id != current.account_id
            || binding.generation != current.manifest.generation
            || binding.selected_car_id != current.selected_car_id
        {
            return Err(crate::db::StoreError::PhysicalV3AdmissionInvalid.into());
        }
        let recovered = store.activate_pending_physical_v3_rotation_at(
            publication_gate,
            current.vehicle_id,
            &retained.admission.receipt_id,
            now_ms,
        )?;
        if recovered.snapshot_id == snapshot_id {
            return Ok(PhysicalV3Publication {
                kind: PhysicalV3PublicationKind::ResumedRotation,
                admission: recovered,
            });
        }
        PhysicalV3PublicationState::Public(recovered)
    } else {
        state
    };
    let (head_sequence, intended) = match state {
        PhysicalV3PublicationState::Empty => (1, IntendedPublication::First),
        PhysicalV3PublicationState::Public(current) if current.snapshot_id == snapshot_id => {
            if binding.installation_id != current.installation_id
                || binding.account_id != current.account_id
                || binding.generation != current.manifest.generation
                || binding.selected_car_id != current.selected_car_id
            {
                return Err(crate::db::StoreError::PhysicalV3AdmissionInvalid.into());
            }
            // The sealed full-table digest and binding select the same head,
            // and the state lookup has verified every persisted pack. Reuse
            // that exact receipt without another full-capacity reservation,
            // SQLite reconstruction or compression pass.
            return Ok(PhysicalV3Publication {
                kind: PhysicalV3PublicationKind::Unchanged,
                admission: current,
            });
        }
        PhysicalV3PublicationState::Public(current) => {
            let next = current
                .head_sequence
                .checked_add(1)
                .filter(|sequence| *sequence <= MAX_IJSON_INTEGER)
                .ok_or(TeslaMatePhysicalPublicationError::SequenceExhausted)?;
            (next, IntendedPublication::Rotate(current))
        }
        PhysicalV3PublicationState::Blocked { .. } => {
            return Err(TeslaMatePhysicalPublicationError::DifferentBlockedRotation);
        }
    };

    let stage_limits = stage.stats()?.limits;
    let writer = ProjectionPackWriter::with_limits(
        store.packs_dir(),
        ProtocolLimits::hub_sync_v1_1_3_schema_2_2(),
    )
    .with_minimum_free_bytes(stage_limits.minimum_free_bytes);
    writer.ensure_full_snapshot_capacity_for_capture(
        stage_limits.max_stage_bytes,
        stage_limits.minimum_free_bytes,
    )?;
    let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        stage,
        &writer,
        binding,
        snapshot_id,
        SequenceRange {
            from_exclusive: head_sequence,
            to_inclusive: head_sequence,
        },
        cursor_key,
        fragment_limits,
    )?;

    match intended {
        IntendedPublication::First => {
            let admission =
                store.stage_pending_physical_v3_admission(publication_gate, candidate)?;
            Ok(PhysicalV3Publication {
                kind: PhysicalV3PublicationKind::FirstHead,
                admission,
            })
        }
        IntendedPublication::Rotate(prior) => {
            let delta = match crate::db::physical_v3_admission_from_manifest(
                &candidate.manifest,
                candidate.binding.selected_car_id,
            ) {
                Ok(target) => {
                    match crate::import::teslamate::physical_delta_pack::prepare_changed_set(
                        &prior,
                        &target,
                        &candidate.binding,
                        cursor_key,
                        stage_digest,
                        store.packs_dir(),
                        stage_limits.minimum_free_bytes,
                    ) {
                        Ok(delta) => Some(delta),
                        Err(error) => {
                            tracing::warn!(%error, vehicle_id = %candidate.binding.vehicle_id,
                            "bounded physical changed set unavailable; signed full replacement remains available");
                            None
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, vehicle_id = %candidate.binding.vehicle_id,
                        "physical changed set candidate identity unavailable");
                    None
                }
            };
            let mut event = dev_publication_choice(delta.is_some(), prior.head_sequence);
            event.target_sequence = Some(head_sequence);
            crate::runtime::development_event_log::record(event);
            let admission = store.rotate_and_activate_physical_v3_admission_with_delta_at(
                publication_gate,
                candidate,
                delta,
                now_ms,
            )?;
            Ok(PhysicalV3Publication {
                kind: PhysicalV3PublicationKind::Rotation,
                admission,
            })
        }
    }
}

fn physical_v3_snapshot_id(stage_digest: Sha256Digest, binding: &ProjectionBinding) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"teslatlas-hub/hub-sync-v1/1.3.0/physical-v3-snapshot/v1\0");
    digest.update(stage_digest.as_bytes());
    digest.update(binding.installation_id.as_bytes());
    digest.update(binding.account_id.as_bytes());
    digest.update(binding.vehicle_id.as_bytes());
    digest.update(binding.generation.to_be_bytes());
    digest.update(binding.selected_car_id.to_be_bytes());
    Uuid::new_v5(&Uuid::NAMESPACE_URL, &digest.finalize())
}

#[derive(Debug, Error)]
pub enum TeslaMatePhysicalPublicationError {
    #[error("physical V3 publication requires a sealed physical-v3 stage")]
    WrongStageFormat,
    #[error("physical V3 publication sequence is exhausted")]
    SequenceExhausted,
    #[error("a different physical V3 rotation is already blocked awaiting activation")]
    DifferentBlockedRotation,
    #[error("the deterministic physical V3 candidate does not match its stored head")]
    DeterministicCandidateMismatch,
    #[error("the system clock cannot produce a valid publication timestamp")]
    Clock,
    #[error("cannot discard consumed physical V3 stage: {0}")]
    StageCleanup(TeslaMateStageError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Stage(#[from] TeslaMateStageError),
    #[error(transparent)]
    Fragment(#[from] TeslaMatePhysicalFragmentError),
    #[error(transparent)]
    Pack(#[from] ProjectionPackError),
}

#[cfg(test)]
#[path = "physical_publication/tests.rs"]
mod tests;

fn dev_publication_choice(delta: bool, base: u64) -> crate::runtime::development_event_log::Event {
    use crate::runtime::development_event_log as dev;
    let mut event = dev::Event::new(
        dev::Kind::Publication,
        if delta {
            dev::Outcome::ChangedSet
        } else {
            dev::Outcome::FullReplacement
        },
    );
    event.base_sequence = Some(base);
    event
}
