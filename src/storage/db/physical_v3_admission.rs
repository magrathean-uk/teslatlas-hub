// SPDX-License-Identifier: AGPL-3.0-only

const HUB_SYNC_V1_1_3_PROFILE: &str = "hub-sync-v1@1.3.0";
const PHYSICAL_V3_RECEIPT_PREFIX: &str = "pv3_";

impl HubStore {
    /// Admit one fixture-built physical snapshot through the same private,
    /// durable production path used by a future source capture. This surface
    /// is absent from default product builds and exists only for the explicit
    /// interop example feature.
    #[cfg(feature = "interop-fixture")]
    #[doc(hidden)]
    pub fn stage_interop_physical_v3_admission(
        &self,
        candidate: crate::sync::physical_fragments::StagedPhysicalProjectionV3,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let publication_gate = self.try_acquire_publication_gate()?;
        self.stage_pending_physical_v3_admission(&publication_gate, candidate)
    }

    /// Rotate and explicitly activate one fixture-built successor through the
    /// production storage path. The blocked-state check prevents this feature
    /// only helper from hiding an accidental public successor before the
    /// retained checkpoint has been supplied to activation.
    #[cfg(feature = "interop-fixture")]
    #[doc(hidden)]
    pub fn rotate_and_activate_interop_physical_v3_admission_at(
        &self,
        candidate: crate::sync::physical_fragments::StagedPhysicalProjectionV3,
        retained_receipt_id: &str,
        retained_at_ms: i64,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let publication_gate = self.try_acquire_publication_gate()?;
        let rotated = self.rotate_pending_physical_v3_admission_at(
            &publication_gate,
            candidate,
            retained_at_ms,
        )?;
        match self.unserved_physical_v3_control_admission_for_vehicle(rotated.vehicle_id) {
            Err(StoreError::PhysicalV3SecondHeadUnsupported(vehicle_id))
                if vehicle_id == rotated.vehicle_id => {}
            _ => return Err(StoreError::PhysicalV3AdmissionConflict),
        }
        let active = self.activate_pending_physical_v3_rotation_at(
            &publication_gate,
            rotated.vehicle_id,
            retained_receipt_id,
            retained_at_ms,
        )?;
        if self
            .unserved_physical_v3_control_admission_for_vehicle(active.vehicle_id)?
            .as_ref()
            != Some(&active)
        {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        Ok(active)
    }

    /// Stage one complete physical schema-2.2 snapshot behind a private,
    /// durable marker. The generic current-manifest and pack-serving catalogue
    /// remains untouched. The caller must hold this store's publication gate
    /// across both candidate creation and this call.
    pub(crate) fn stage_pending_physical_v3_admission(
        &self,
        publication_gate: &PublicationGate,
        mut candidate: crate::sync::physical_fragments::StagedPhysicalProjectionV3,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let result = self.stage_pending_physical_v3_admission_inner(&candidate);
        candidate.retain_catalogued_objects();
        match result {
            Ok(admission) => Ok(admission),
            Err(error) => {
                let mut cleanup_error = None;
                for chunk in &candidate.chunks {
                    if chunk.ownership()
                        == crate::hub_pack::ProjectionPackOwnership::Created
                        && let Err(source) = self.remove_unretained_pack(
                            publication_gate,
                            chunk.metadata.sha256,
                            &chunk.path,
                        ) && cleanup_error.is_none()
                        {
                            cleanup_error = Some(source);
                        }
                }
                Err(cleanup_error.unwrap_or(error))
            }
        }
    }

    fn stage_pending_physical_v3_admission_inner(
        &self,
        candidate: &crate::sync::physical_fragments::StagedPhysicalProjectionV3,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let admission = self.validate_physical_v3_candidate(candidate)?;
        if let Some(existing) =
            self.pending_physical_v3_admission_for_vehicle(admission.vehicle_id)?
        {
            return if existing == admission {
                Ok(existing)
            } else {
                Err(StoreError::PhysicalV3SecondHeadUnsupported(
                    admission.vehicle_id,
                ))
            };
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT snapshot_id FROM pending_physical_v3_admissions WHERE vehicle_id = ?1",
                [admission.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if existing.is_some() {
            return Err(StoreError::PhysicalV3SecondHeadUnsupported(
                admission.vehicle_id,
            ));
        }
        insert_pending_physical_v3_admission(&transaction, &admission, "public_first")?;
        self.commit_physical_v3_admission(transaction, &admission)?;
        Ok(admission)
    }

    /// Fixture/recovery support for the historical two-commit rotation. Normal
    /// production uses the atomic public replacement method below.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn rotate_pending_physical_v3_admission_at(
        &self,
        publication_gate: &PublicationGate,
        candidate: crate::sync::physical_fragments::StagedPhysicalProjectionV3,
        retained_at_ms: i64,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        self.rotate_pending_physical_v3_admission_with_delta_at(
            publication_gate, candidate, None, retained_at_ms,
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn rotate_pending_physical_v3_admission_with_delta_at(
        &self,
        publication_gate: &PublicationGate,
        candidate: crate::sync::physical_fragments::StagedPhysicalProjectionV3,
        delta: Option<crate::import::source_db::physical_delta_pack::StagedPhysicalDelta>,
        retained_at_ms: i64,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        self.rotate_physical_v3_admission_at(publication_gate, candidate, delta, retained_at_ms, false)
    }

    /// Production has retained-prior rebase support, so replacement and public
    /// activation commit together. A crash cannot persist an unservable head
    /// whose activation later depends on the predecessor's retention deadline.
    pub(crate) fn rotate_and_activate_physical_v3_admission_with_delta_at(
        &self,
        publication_gate: &PublicationGate,
        candidate: crate::sync::physical_fragments::StagedPhysicalProjectionV3,
        delta: Option<crate::import::source_db::physical_delta_pack::StagedPhysicalDelta>,
        retained_at_ms: i64,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        self.rotate_physical_v3_admission_at(publication_gate, candidate, delta, retained_at_ms, true)
    }

    fn rotate_physical_v3_admission_at(
        &self,
        publication_gate: &PublicationGate,
        mut candidate: crate::sync::physical_fragments::StagedPhysicalProjectionV3,
        mut delta: Option<crate::import::source_db::physical_delta_pack::StagedPhysicalDelta>,
        retained_at_ms: i64,
        public: bool,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let serve_state = if public { "public_first" } else { "blocked_rotation" };
        let result = self.rotate_pending_physical_v3_admission_inner(
            &candidate, delta.as_ref(), retained_at_ms, serve_state,
        );
        // A failed commit acknowledgement can still leave both objects
        // catalogued. Give both candidates the same reference-aware cleanup.
        candidate.retain_catalogued_objects();
        if let Some(delta) = delta.as_mut() {
            delta.retain_catalogued_objects();
        }
        match result {
            Ok(admission) => Ok(admission),
            Err(error) => {
                let written_chunks = candidate
                    .chunks
                    .iter()
                    .filter(|chunk| chunk.may_remove_unpublished_file())
                    .map(|chunk| (chunk.metadata.sha256, chunk.path.as_path()));
                let written_deltas = delta
                    .iter()
                    .flat_map(|delta| &delta.packs)
                    .filter(|pack| pack.created)
                    .map(|pack| (pack.sha256, pack.path.as_path()));
                let mut cleanup_error = None;
                for (digest, path) in written_chunks.chain(written_deltas) {
                    if let Err(source) = self.remove_unretained_pack(publication_gate, digest, path)
                        && cleanup_error.is_none()
                    {
                        cleanup_error = Some(source);
                    }
                }
                Err(cleanup_error.unwrap_or(error))
            }
        }
    }

    fn rotate_pending_physical_v3_admission_inner(
        &self,
        candidate: &crate::sync::physical_fragments::StagedPhysicalProjectionV3,
        delta: Option<&crate::import::source_db::physical_delta_pack::StagedPhysicalDelta>,
        retained_at_ms: i64,
        serve_state: &'static str,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let expires_at_ms = retained_at_ms
            .checked_add(RETIRED_LINEAGE_PACK_RETENTION_MS)
            .filter(|expires| {
                retained_at_ms >= 0
                    && *expires > retained_at_ms
                    && *expires <= 9_007_199_254_740_991
            })
            .ok_or(StoreError::PhysicalV3RetentionInvalid)?;
        let next = self.validate_physical_v3_candidate(candidate)?;
        let prior = self
            .pending_physical_v3_admission_for_vehicle(next.vehicle_id)?
            .ok_or(StoreError::PhysicalV3AdmissionConflict)?;
        if next == prior {
            if serve_state == "public_first"
                && self.unserved_physical_v3_control_admission_for_vehicle(next.vehicle_id)?.as_ref() != Some(&prior)
            {
                return Err(StoreError::PhysicalV3AdmissionConflict);
            }
            return Ok(prior);
        }
        if next.installation_id != prior.installation_id
            || next.account_id != prior.account_id
            || next.vehicle_id != prior.vehicle_id
            || next.selected_car_id != prior.selected_car_id
            || next.manifest.generation != prior.manifest.generation
            || next.head_sequence <= prior.head_sequence
        {
            return Err(StoreError::PhysicalV3AdmissionInvalid);
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let stored_snapshot: Option<String> = transaction
            .query_row(
                "SELECT snapshot_id FROM pending_physical_v3_admissions
                  WHERE vehicle_id = ?1 AND serve_state = 'public_first'",
                [next.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if stored_snapshot.as_deref() != Some(prior.snapshot_id.to_string().as_str()) {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        insert_retained_physical_v3_admission(
            &transaction,
            &prior,
            retained_at_ms,
            expires_at_ms,
        )?;
        transaction
            .execute(
                "DELETE FROM pending_physical_v3_admissions WHERE vehicle_id = ?1",
                [next.vehicle_id.to_string()],
            )
            .map_err(StoreError::PublishManifest)?;
        insert_pending_physical_v3_admission(&transaction, &next, serve_state)?;
        if let Some(delta) = delta {
            insert_physical_v3_delta_transition(&transaction, &prior, &next, delta, &self.packs_dir)?;
        }
        self.commit_physical_v3_rotation(
            transaction,
            &prior,
            &next,
            retained_at_ms,
            expires_at_ms,
            serve_state,
        )?;
        Ok(next)
    }

    fn validate_physical_v3_candidate(
        &self,
        candidate: &crate::sync::physical_fragments::StagedPhysicalProjectionV3,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let manifest = &candidate.manifest;
        if manifest.schema != HUB_PROJECTION_SCHEMA_V3
            || manifest.mode != crate::protocol::TransferMode::FullSnapshot
            || manifest.chunks.is_empty()
            || manifest.chunks.len() != candidate.chunks.len()
        {
            return Err(StoreError::PhysicalV3AdmissionInvalid);
        }
        if self.installation_id()? != candidate.binding.installation_id
            || !self.vehicle_is_active(candidate.binding.vehicle_id)?
            || self.v2_projection_binding(candidate.binding.vehicle_id)? != candidate.binding
            || manifest.installation_id != candidate.binding.installation_id
            || manifest.account_id != candidate.binding.account_id
            || manifest.vehicle_id != candidate.binding.vehicle_id
            || manifest.generation != candidate.binding.generation
        {
            return Err(StoreError::PhysicalV3AdmissionInvalid);
        }
        validate_pending_physical_v3_manifest(manifest)?;
        for (expected, built) in manifest.chunks.iter().zip(&candidate.chunks) {
            let expected_path = self
                .packs_dir
                .join("sha256")
                .join(format!("{}.sqlite.zst", expected.sha256));
            if &built.metadata != expected || built.path != expected_path {
                return Err(StoreError::PhysicalV3AdmissionInvalid);
            }
            // One full verification per pack (M03-T03): the purpose check opens
            // the immutable bytes, verifies them against the schema-2.2 receiver
            // profile and only then proves the physical-publication identity, so
            // a separate profile verification here would decompress and hash
            // every pack a second time for the same answer.
            #[cfg(test)]
            StoreInstrumentation::bump(&self.instrumentation.physical_v3_pack_verifications);
            built
                .verify_hub_sync_v1_1_3_physical_purpose(manifest, &candidate.binding)
                .map_err(StoreError::PhysicalV3Pack)?;
        }
        physical_v3_admission_from_manifest(manifest, candidate.binding.selected_car_id)
    }

    fn commit_physical_v3_admission(
        &self,
        transaction: Transaction<'_>,
        admission: &PendingPhysicalV3Admission,
    ) -> Result<(), StoreError> {
        if let Err(source) = crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::CatalogueBeforeCommit,
        ) {
            drop(transaction);
            return match self.physical_v3_admission_commit_state(admission)? {
                ManifestCommitState::Absent => Err(StoreError::CatalogueDurability(source)),
                ManifestCommitState::Exact | ManifestCommitState::Conflicting => {
                    Err(StoreError::PhysicalV3AdmissionConflict)
                }
            };
        }
        if transaction.commit().is_err() {
            return match self.physical_v3_admission_commit_state(admission)? {
                ManifestCommitState::Exact => Ok(()),
                ManifestCommitState::Absent => Err(StoreError::PhysicalV3AdmissionConflict),
                ManifestCommitState::Conflicting => Err(StoreError::PhysicalV3AdmissionConflict),
            };
        }
        if crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::CatalogueAfterCommit,
        )
        .is_err()
        {
            return match self.physical_v3_admission_commit_state(admission)? {
                ManifestCommitState::Exact => Ok(()),
                ManifestCommitState::Absent | ManifestCommitState::Conflicting => {
                    Err(StoreError::PhysicalV3AdmissionConflict)
                }
            };
        }
        Ok(())
    }

    fn physical_v3_admission_commit_state(
        &self,
        expected: &PendingPhysicalV3Admission,
    ) -> Result<ManifestCommitState, StoreError> {
        match self.pending_physical_v3_admission_for_vehicle(expected.vehicle_id) {
            Ok(Some(stored)) if stored == *expected => Ok(ManifestCommitState::Exact),
            Ok(None) => Ok(ManifestCommitState::Absent),
            Ok(Some(_)) | Err(StoreError::PhysicalV3AdmissionConflict) => {
                Ok(ManifestCommitState::Conflicting)
            }
            Err(error) => Err(error),
        }
    }

    fn commit_physical_v3_rotation(
        &self,
        transaction: Transaction<'_>,
        prior: &PendingPhysicalV3Admission,
        next: &PendingPhysicalV3Admission,
        retained_at_ms: i64,
        expires_at_ms: i64,
        serve_state: &str,
    ) -> Result<(), StoreError> {
        if let Err(source) = crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::CatalogueBeforeCommit,
        ) {
            drop(transaction);
            return match self.physical_v3_rotation_commit_state(
                prior,
                next,
                retained_at_ms,
                expires_at_ms,
                serve_state,
            )? {
                ManifestCommitState::Absent => Err(StoreError::CatalogueDurability(source)),
                ManifestCommitState::Exact | ManifestCommitState::Conflicting => {
                    Err(StoreError::PhysicalV3AdmissionConflict)
                }
            };
        }
        if transaction.commit().is_err() {
            return match self.physical_v3_rotation_commit_state(
                prior,
                next,
                retained_at_ms,
                expires_at_ms,
                serve_state,
            )? {
                ManifestCommitState::Exact => Ok(()),
                ManifestCommitState::Absent | ManifestCommitState::Conflicting => {
                    Err(StoreError::PhysicalV3AdmissionConflict)
                }
            };
        }
        if crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::CatalogueAfterCommit,
        )
        .is_err()
        {
            return match self.physical_v3_rotation_commit_state(
                prior,
                next,
                retained_at_ms,
                expires_at_ms,
                serve_state,
            )? {
                ManifestCommitState::Exact => Ok(()),
                ManifestCommitState::Absent | ManifestCommitState::Conflicting => {
                    Err(StoreError::PhysicalV3AdmissionConflict)
                }
            };
        }
        Ok(())
    }

    fn physical_v3_rotation_commit_state(
        &self,
        prior: &PendingPhysicalV3Admission,
        next: &PendingPhysicalV3Admission,
        retained_at_ms: i64,
        expires_at_ms: i64,
        serve_state: &str,
    ) -> Result<ManifestCommitState, StoreError> {
        let current = self.pending_physical_v3_admission_for_vehicle(next.vehicle_id);
        let actual_serve_state: Option<String> = self.open_read_only_connection()?.query_row(
            "SELECT serve_state FROM pending_physical_v3_admissions WHERE vehicle_id = ?1",
            [next.vehicle_id.to_string()], |row| row.get(0),
        ).optional().map_err(StoreError::Query)?;
        let retained = self.retained_physical_v3_admission_for_receipt_at(
            next.vehicle_id,
            &prior.receipt_id,
            retained_at_ms,
            true,
        );
        match (current, retained) {
            (Ok(Some(current)), Ok(Some(retained)))
                if current == *next
                    && actual_serve_state.as_deref() == Some(serve_state)
                    && retained.admission == *prior
                    && retained.retained_at_ms == retained_at_ms
                    && retained.expires_at_ms == expires_at_ms =>
            {
                Ok(ManifestCommitState::Exact)
            }
            (Ok(Some(current)), Ok(None)) if current == *prior => Ok(ManifestCommitState::Absent),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(ManifestCommitState::Conflicting),
        }
    }

    /// Promote one already-rotated, verified PhysicalV3 successor while holding
    /// the same publication gate used for rotation. Recovery uses its persisted
    /// identity independently of any newer source snapshot.
    pub(crate) fn activate_pending_physical_v3_rotation_at(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        retained_receipt_id: &str,
        now_ms: i64,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let current = self
            .pending_physical_v3_admission_for_vehicle(vehicle_id)?
            .ok_or(StoreError::PhysicalV3AdmissionConflict)?;
        let retained = self
            .retained_physical_v3_admission_for_receipt_at(
                vehicle_id,
                retained_receipt_id,
                now_ms,
                true,
            )?
            .ok_or(StoreError::PhysicalV3AdmissionConflict)?;
        let prior = &retained.admission;
        if self.immediate_retained_physical_v3_receipt(vehicle_id, current.head_sequence)?
            .as_deref()
            != Some(retained_receipt_id)
        {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        if current.installation_id != prior.installation_id
            || current.account_id != prior.account_id
            || current.vehicle_id != prior.vehicle_id
            || current.selected_car_id != prior.selected_car_id
            || current.manifest.generation != prior.manifest.generation
            || current.head_sequence <= prior.head_sequence
        {
            return Err(StoreError::PhysicalV3AdmissionInvalid);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let state: Option<String> = transaction
            .query_row(
                "SELECT serve_state FROM pending_physical_v3_admissions
                  WHERE vehicle_id = ?1 AND snapshot_id = ?2",
                params![vehicle_id.to_string(), current.snapshot_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        match state.as_deref() {
            Some("public_first") => return Ok(current),
            Some("blocked_rotation") => {}
            _ => return Err(StoreError::PhysicalV3AdmissionConflict),
        }
        let changed = transaction
            .execute(
                "UPDATE pending_physical_v3_admissions
                    SET serve_state = 'public_first'
                  WHERE vehicle_id = ?1
                    AND snapshot_id = ?2
                    AND serve_state = 'blocked_rotation'
                    AND EXISTS (
                        SELECT 1 FROM retained_physical_v3_admissions AS retained
                         WHERE retained.vehicle_id = ?1
                           AND retained.receipt_id = ?3
                           AND retained.expires_at_ms > ?4
                    )",
                params![
                    vehicle_id.to_string(),
                    current.snapshot_id.to_string(),
                    retained_receipt_id,
                    now_ms,
                ],
            )
            .map_err(StoreError::PublishManifest)?;
        if changed != 1 {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        self.commit_physical_v3_activation(transaction, vehicle_id, current.snapshot_id)?;
        Ok(current)
    }

    fn commit_physical_v3_activation(
        &self,
        transaction: Transaction<'_>,
        vehicle_id: Uuid,
        snapshot_id: Uuid,
    ) -> Result<(), StoreError> {
        if let Err(source) = crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::CatalogueBeforeCommit,
        ) {
            drop(transaction);
            return if self.physical_v3_activation_is_exact(vehicle_id, snapshot_id)? {
                Err(StoreError::PhysicalV3AdmissionConflict)
            } else {
                Err(StoreError::CatalogueDurability(source))
            };
        }
        if transaction.commit().is_err() {
            return if self.physical_v3_activation_is_exact(vehicle_id, snapshot_id)? {
                Ok(())
            } else {
                Err(StoreError::PhysicalV3AdmissionConflict)
            };
        }
        if crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::CatalogueAfterCommit,
        )
        .is_err()
            && !self.physical_v3_activation_is_exact(vehicle_id, snapshot_id)?
        {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        Ok(())
    }

    fn physical_v3_activation_is_exact(
        &self,
        vehicle_id: Uuid,
        snapshot_id: Uuid,
    ) -> Result<bool, StoreError> {
        self.open_read_only_connection()?
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM pending_physical_v3_admissions
                     WHERE vehicle_id = ?1
                       AND snapshot_id = ?2
                       AND serve_state = 'public_first'
                 )",
                params![vehicle_id.to_string(), snapshot_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    pub(crate) fn pending_physical_v3_admission_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<PendingPhysicalV3Admission>, StoreError> {
        self.pending_physical_v3_admission_for_vehicle_with_file_digests(vehicle_id, true, false)
    }

    /// Resolve the exact private publication state while rechecking every
    /// current and retained object. A blocked successor is returned with
    /// its immediate unexpired prior receipt so a production caller can finish the
    /// already-committed rotation instead of attempting a third head.
    pub(crate) fn physical_v3_publication_state_for_vehicle_at(
        &self,
        vehicle_id: Uuid,
        now_ms: i64,
    ) -> Result<PhysicalV3PublicationState, StoreError> {
        let Some(current) = self.pending_physical_v3_admission_for_vehicle(vehicle_id)? else {
            return Ok(PhysicalV3PublicationState::Empty);
        };
        let connection = self.open_read_only_connection()?;
        let serve_state: String = connection
            .query_row(
                "SELECT serve_state FROM pending_physical_v3_admissions
                  WHERE vehicle_id = ?1 AND snapshot_id = ?2",
                params![vehicle_id.to_string(), current.snapshot_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        match serve_state.as_str() {
            "public_first" => Ok(PhysicalV3PublicationState::Public(current)),
            "blocked_rotation" => {
                let receipt_id = self
                    .immediate_retained_physical_v3_receipt(vehicle_id, current.head_sequence)?
                    .ok_or(StoreError::PhysicalV3AdmissionConflict)?;
                let retained = self
                    .retained_physical_v3_admission_for_receipt_at(
                        vehicle_id,
                        &receipt_id,
                        now_ms,
                        true,
                    )?
                    .ok_or(StoreError::PhysicalV3AdmissionConflict)?;
                Ok(PhysicalV3PublicationState::Blocked { current, retained })
            }
            _ => Err(StoreError::PhysicalV3AdmissionConflict),
        }
    }

    /// The blocked head's direct predecessor is the latest retained sequence
    /// below it. Older unexpired receipts remain available for signed rebases.
    fn immediate_retained_physical_v3_receipt(
        &self,
        vehicle_id: Uuid,
        current_sequence: u64,
    ) -> Result<Option<String>, StoreError> {
        let current_sequence = i64::try_from(current_sequence)
            .map_err(|_| StoreError::InvalidStoredSequence)?;
        self.open_read_only_connection()?
            .query_row(
                "SELECT receipt_id FROM retained_physical_v3_admissions
                  WHERE vehicle_id = ?1 AND head_sequence < ?2
                  ORDER BY head_sequence DESC LIMIT 1",
                params![vehicle_id.to_string(), current_sequence],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)
    }

    /// Load the currently bound marker and exact pack metadata for a control
    /// response without reading every pack body. Pack GET verifies its one
    /// bounded body before serving bytes.
    ///
    /// This is the served head of the negotiated 1.4 route (and of 1.3 on a
    /// 2.2 base). Once the vehicle has any of the import seam, only a head
    /// the importer planned with the seam is served (M03-T21); a raw copy of
    /// the source is not, and clients then find no physical base.
    pub(crate) fn pending_physical_v3_control_admission_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<PendingPhysicalV3Admission>, StoreError> {
        let Some(admission) = self.unserved_physical_v3_control_admission_for_vehicle(vehicle_id)?
        else {
            return Ok(None);
        };
        if !self.physical_v3_admission_served(vehicle_id, admission.snapshot_id)? {
            return Ok(None);
        }
        Ok(Some(admission))
    }

    /// Whether a physical head may be served (M03-T21, review REV-CX-M03-T19
    /// finding 1): always for a vehicle without the import seam; for a
    /// vehicle with any of it only when the importer planned the head with
    /// the seam. The control head, the snapshot packs and the changed sets
    /// (by their target) all apply this one check.
    fn physical_v3_admission_served(
        &self,
        vehicle_id: Uuid,
        snapshot_id: Uuid,
    ) -> Result<bool, StoreError> {
        let connection = self.open_read_only_connection()?;
        let vehicle_key = vehicle_id.to_string();
        if !import_seam_present_in_connection(&connection, &vehicle_key)? {
            return Ok(true);
        }
        let snapshot = snapshot_id.to_string();
        if !physical_v3_seam_planned_in_connection(&connection, vehicle_id)?.contains(&snapshot) {
            return Ok(false);
        }
        // M03-T22: a handback the store's coverage lacks (its write failed)
        // refuses every planned head until the coverage is reconciled.
        if coverage_unreconciled_in(&connection, &self.database_path)
            .map_err(|_| StoreError::PhysicalV3AdmissionConflict)?
        {
            tracing::info!(
                %vehicle_id,
                %snapshot_id,
                "physical 1.4 head refused: a window's handback has not reached the store's \
                 coverage yet"
            );
            return Ok(false);
        }
        // M03-T22: only while the coverage it was planned with is the
        // vehicle's coverage; a window handed back since refuses it until a
        // handback or an import plans the head again.
        let planned_with = physical_v3_seam_coverage_in_connection(&connection, vehicle_id)?
            .remove(&snapshot);
        let current = closed_coverage_digest_for_vehicle(&connection, &vehicle_key)
            .map_err(|_| StoreError::PhysicalV3AdmissionConflict)?;
        let served = planned_with.as_deref() == Some(current.as_str());
        if !served {
            tracing::info!(
                %vehicle_id,
                %snapshot_id,
                "physical 1.4 head refused: it was planned with other coverage than the \
                 vehicle's; a handback or the next import plans it again"
            );
        }
        Ok(served)
    }

    /// Whether the store's coverage still lacks a handback this process
    /// failed to write (M03-T22): nothing may be planned from it.
    pub(crate) fn collection_coverage_unreconciled(&self) -> Result<bool, StoreError> {
        let connection = self.open_read_only_connection()?;
        coverage_unreconciled_in(&connection, &self.database_path)
            .map_err(|_| StoreError::PhysicalV3AdmissionConflict)
    }

    /// Whether the vehicle's current physical head is served (M03-T22).
    pub(crate) fn physical_v3_current_head_served(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<bool>, StoreError> {
        let Some(admission) = self.unserved_physical_v3_control_admission_for_vehicle(vehicle_id)?
        else {
            return Ok(None);
        };
        self.physical_v3_admission_served(vehicle_id, admission.snapshot_id)
            .map(Some)
    }

    /// Records `snapshot_id` as planned with the coverage the store holds now
    /// (closed windows): the store's own tests record heads this way.
    #[cfg(test)]
    pub(crate) fn record_physical_v3_seam_planned(
        &self,
        publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        snapshot_id: Uuid,
    ) -> Result<(), StoreError> {
        let coverage = {
            let connection = self.open_read_only_connection()?;
            closed_coverage_digest_for_vehicle(&connection, &vehicle_id.to_string())
                .map_err(|_| StoreError::PhysicalV3AdmissionConflict)?
        };
        self.record_physical_v3_seam_planned_with_coverage(
            publication_gate,
            vehicle_id,
            snapshot_id,
            &coverage,
        )
    }

    /// Records `snapshot_id` as a physical head of the vehicle that the
    /// importer planned with the import seam (M03-T21), before it publishes
    /// it, so a crash leaves at most an unused id. The list keeps only the
    /// heads the store still holds, and this one. `coverage` is the digest of
    /// the coverage the plan was made with (M03-T22): the head is served only
    /// while the vehicle's coverage has that digest.
    pub(crate) fn record_physical_v3_seam_planned_with_coverage(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        snapshot_id: Uuid,
        coverage: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let new = snapshot_id.to_string();
        let mut planned = physical_v3_seam_planned_in_connection(&transaction, vehicle_id)?;
        planned.insert(new.clone());
        let mut kept = std::collections::BTreeSet::new();
        for id in planned {
            let held: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pending_physical_v3_admissions
                                    WHERE vehicle_id = ?1 AND snapshot_id = ?2)
                         OR EXISTS(SELECT 1 FROM retained_physical_v3_admissions
                                    WHERE vehicle_id = ?1 AND snapshot_id = ?2)",
                    params![vehicle_id.to_string(), id],
                    |row| row.get(0),
                )
                .map_err(StoreError::Query)?;
            if held || id == new {
                kept.insert(id);
            }
        }
        let mut coverages = physical_v3_seam_coverage_in_connection(&transaction, vehicle_id)?;
        coverages.retain(|id, _| kept.contains(id));
        coverages.insert(new, coverage.to_owned());
        let value = serde_json::to_string(&kept).map_err(StoreError::SerializeLifecycleRow)?;
        let coverages =
            serde_json::to_string(&coverages).map_err(StoreError::SerializeLifecycleRow)?;
        for (key, value) in [
            (physical_v3_seam_planned_key(vehicle_id), value),
            (physical_v3_seam_coverage_key(vehicle_id), coverages),
        ] {
            transaction
                .execute(
                    "INSERT INTO hub_metadata(key, value) VALUES(?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![key, value],
                )
                .map_err(StoreError::Query)?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)
    }

    /// The digests of the raw physical stage and the source sessions kept for
    /// the vehicle's handback (M03-T22), if any.
    pub(crate) fn physical_v3_kept(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<PhysicalV3Kept>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let value: Option<String> = connection
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = ?1",
                params![physical_v3_kept_key(vehicle_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        value
            .map(|value| {
                serde_json::from_str(&value).map_err(|_| StoreError::PhysicalV3AdmissionConflict)
            })
            .transpose()
    }

    /// The recorded inputs of the vehicle's planned physical head
    /// (M08-T19), if any; one that does not read back is none.
    pub(crate) fn physical_v3_handback_plan(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<PhysicalV3HandbackPlan>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let value: Option<String> = connection
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = ?1",
                params![physical_v3_handback_plan_key(vehicle_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        Ok(value.and_then(|value| serde_json::from_str(&value).ok()))
    }

    /// Records (or with `None` forgets) the inputs of the vehicle's planned
    /// physical head (M08-T19).
    pub(crate) fn record_physical_v3_handback_plan(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        plan: Option<&PhysicalV3HandbackPlan>,
    ) -> Result<(), StoreError> {
        let connection = self.open()?;
        match plan {
            Some(plan) => {
                let value =
                    serde_json::to_string(plan).map_err(StoreError::SerializeLifecycleRow)?;
                connection.execute(
                    "INSERT INTO hub_metadata(key, value) VALUES(?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![physical_v3_handback_plan_key(vehicle_id), value],
                )
            }
            None => connection.execute(
                "DELETE FROM hub_metadata WHERE key = ?1",
                params![physical_v3_handback_plan_key(vehicle_id)],
            ),
        }
        .map_err(StoreError::Query)?;
        Ok(())
    }

    /// The snapshot id of the vehicle's current public physical head, served
    /// or not, without reading its packs (M08-T19).
    pub(crate) fn physical_v3_current_snapshot_id(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<Uuid>, StoreError> {
        Ok(self
            .unserved_physical_v3_control_admission_for_vehicle(vehicle_id)?
            .map(|admission| admission.snapshot_id))
    }

    /// The vehicles with a kept physical stage (M03-T22).
    pub(crate) fn physical_v3_kept_vehicles(&self) -> Result<Vec<Uuid>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare("SELECT key FROM hub_metadata WHERE key LIKE 'physical_v3_kept:%' ORDER BY key")
            .map_err(StoreError::Query)?;
        let keys = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        Ok(keys
            .iter()
            .filter_map(|key| key.strip_prefix("physical_v3_kept:")?.parse().ok())
            .collect())
    }

    /// Records (or with `None` forgets) the stage and source kept for the
    /// vehicle's handback (M03-T22); replaced on each import.
    pub(crate) fn record_physical_v3_kept(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        kept: Option<&PhysicalV3Kept>,
    ) -> Result<(), StoreError> {
        let connection = self.open()?;
        match kept {
            Some(kept) => {
                let value =
                    serde_json::to_string(kept).map_err(StoreError::SerializeLifecycleRow)?;
                connection.execute(
                    "INSERT INTO hub_metadata(key, value) VALUES(?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![physical_v3_kept_key(vehicle_id), value],
                )
            }
            None => connection.execute(
                "DELETE FROM hub_metadata WHERE key = ?1",
                params![physical_v3_kept_key(vehicle_id)],
            ),
        }
        .map_err(StoreError::Query)?;
        Ok(())
    }

    /// Replaces the vehicle's kept record in one transaction (DEC-250): only
    /// when the record now is `expected` (`None`: absent), to `kept`
    /// (`None`: forgotten), and with `clear_plan` its recorded plan inputs
    /// go in the same commit. Returns whether it replaced the record; the
    /// files the record names are the caller's, written durably before and
    /// removed only after.
    pub(crate) fn swap_physical_v3_kept(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        expected: Option<&PhysicalV3Kept>,
        kept: Option<&PhysicalV3Kept>,
        clear_plan: bool,
    ) -> Result<bool, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Query)?;
        let current: Option<String> = transaction
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = ?1",
                params![physical_v3_kept_key(vehicle_id)],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let current = current
            .map(|value| {
                serde_json::from_str::<PhysicalV3Kept>(&value)
                    .map_err(|_| StoreError::PhysicalV3AdmissionConflict)
            })
            .transpose()?;
        if current.as_ref() != expected {
            return Ok(false);
        }
        match kept {
            Some(kept) => {
                let value =
                    serde_json::to_string(kept).map_err(StoreError::SerializeLifecycleRow)?;
                transaction.execute(
                    "INSERT INTO hub_metadata(key, value) VALUES(?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![physical_v3_kept_key(vehicle_id), value],
                )
            }
            None => transaction.execute(
                "DELETE FROM hub_metadata WHERE key = ?1",
                params![physical_v3_kept_key(vehicle_id)],
            ),
        }
        .map_err(StoreError::Query)?;
        if clear_plan {
            transaction
                .execute(
                    "DELETE FROM hub_metadata WHERE key = ?1",
                    params![physical_v3_handback_plan_key(vehicle_id)],
                )
                .map_err(StoreError::Query)?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(true)
    }

    /// Every kept archive the catalogue references, by vehicle (backup
    /// selection, DEC-250). A record that does not read back is refused.
    pub(crate) fn physical_v3_kept_archives(
        &self,
    ) -> Result<Vec<(Uuid, PhysicalV3KeptArchive)>, StoreError> {
        physical_v3_kept_archives_in(&self.open_read_only_connection()?)
    }

    /// The import seam planned at a handback from the kept source sessions
    /// and the coverage now (M03-T22), in one IMMEDIATE transaction. It
    /// commits the Hub's side of the plan only when the plan restates no
    /// source row; otherwise nothing is applied. Returns the plan and
    /// whether it committed.
    pub(crate) fn import_seam_for_handback(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        car_id: i64,
        drives: &[ProjectionDrive],
        charges: &[crate::hub_pack::ProjectionCharge],
    ) -> Result<(ImportSeamDryRun, bool), StoreError> {
        self.import_seam_ahead_of_packs(vehicle_id, car_id, drives, charges, |_, run| {
            Ok(run.ahead.restated_drives.is_empty() && run.ahead.restated_charges.is_empty())
        })
    }

    /// The current physical head as its publisher checks it, served or not.
    fn unserved_physical_v3_control_admission_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<PendingPhysicalV3Admission>, StoreError> {
        self.pending_physical_v3_admission_for_vehicle_with_file_digests(vehicle_id, false, true)
    }

    fn pending_physical_v3_admission_for_vehicle_with_file_digests(
        &self,
        vehicle_id: Uuid,
        verify_file_digests: bool,
        require_public_first: bool,
    ) -> Result<Option<PendingPhysicalV3Admission>, StoreError> {
        let mut connection = self.open_read_only_connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(StoreError::Begin)?;
        let row = transaction
            .query_row(
                "SELECT admission.snapshot_id, admission.installation_id,
                        admission.account_id, admission.selected_car_id,
                        admission.profile,
                        admission.head_sequence, admission.chunk_count,
                        admission.manifest_sha256, admission.ordered_chunks_sha256,
                        admission.receipt_id, admission.manifest_json,
                        admission.serve_state
                   FROM pending_physical_v3_admissions AS admission
                  WHERE admission.vehicle_id = ?1",
                [vehicle_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Vec<u8>>(10)?,
                        row.get::<_, String>(11)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((
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
            serve_state,
        )) = row
        else {
            return Ok(None);
        };
        if require_public_first && serve_state != "public_first" {
            return Err(StoreError::PhysicalV3SecondHeadUnsupported(vehicle_id));
        }
        if serve_state != "public_first" && serve_state != "blocked_rotation" {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        let manifest: SyncManifest = serde_json::from_slice(&manifest_json)
            .map_err(StoreError::DeserializeManifest)?;
        validate_pending_physical_v3_manifest(&manifest)?;
        let computed = physical_v3_admission_from_manifest(&manifest, selected_car_id)?;
        let stored_matches = profile == HUB_SYNC_V1_1_3_PROFILE
            && vehicle_id == computed.vehicle_id
            && snapshot_id == computed.snapshot_id.to_string()
            && installation_id == computed.installation_id.to_string()
            && account_id == computed.account_id.to_string()
            && u64::try_from(head_sequence).ok() == Some(computed.head_sequence)
            && u32::try_from(chunk_count).ok() == Some(computed.chunk_count)
            && manifest_sha256 == computed.manifest_sha256.to_string()
            && ordered_chunks_sha256 == computed.ordered_chunks_sha256.to_string()
            && receipt_id == computed.receipt_id;
        if !stored_matches {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        let current_binding = self
            .v2_projection_binding(vehicle_id)
            .map_err(|_| StoreError::PhysicalV3AdmissionInvalid)?;
        if self.installation_id()? != computed.installation_id
            || !self.vehicle_is_active(vehicle_id)?
            || current_binding
                != (ProjectionBinding {
                    installation_id: computed.installation_id,
                    account_id: computed.account_id,
                    vehicle_id: computed.vehicle_id,
                    generation: computed.manifest.generation,
                    selected_car_id: computed.selected_car_id,
                })
        {
            return Err(StoreError::PhysicalV3AdmissionInvalid);
        }
        #[cfg(test)]
        if let Some(publish) = ROTATE_DURING_PHYSICAL_ADMISSION_READ.with(|hook| hook.take()) {
            publish();
        }
        let pack_rows = transaction
            .prepare(
                "SELECT ordinal, sha256, relative_path,
                        compressed_bytes, uncompressed_bytes
                   FROM pending_physical_v3_packs
                  WHERE snapshot_id = ?1 ORDER BY ordinal",
            )
            .map_err(StoreError::Query)?
            .query_map([computed.snapshot_id.to_string()], |row| {
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
        // Packs are digest-named and immutable: release the snapshot before the
        // stat and hash loop so it does not pin the WAL.
        drop(transaction);
        if pack_rows.len() != manifest.chunks.len() {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        for (stored, pack) in pack_rows.iter().zip(&manifest.chunks) {
            let expected = (
                i64::from(pack.ordinal),
                pack.sha256.to_string(),
                pack.relative_path.clone(),
                i64::try_from(pack.compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
                i64::try_from(pack.uncompressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?,
            );
            if stored != &expected {
                return Err(StoreError::PhysicalV3AdmissionConflict);
            }
            let path = self
                .packs_dir
                .join("sha256")
                .join(format!("{}.sqlite.zst", pack.sha256));
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| StoreError::PhysicalV3AdmissionConflict)?;
            if !metadata.file_type().is_file() || metadata.len() != pack.compressed_bytes {
                return Err(StoreError::PhysicalV3AdmissionConflict);
            }
            if verify_file_digests {
                let authentic = sha256_file_hex(&path)
                    .map_err(|_| StoreError::PhysicalV3AdmissionConflict)?
                    == pack.sha256.to_string();
                #[cfg(test)]
                crate::hub_pack::record_verification("admission-pack-digest", pack.sha256, authentic);
                if !authentic {
                    return Err(StoreError::PhysicalV3AdmissionConflict);
                }
            }
        }
        Ok(Some(computed))
    }

    /// Resolve one exact pack from a currently valid private physical
    /// admission without adding it to the generic sync catalogue. The caller
    /// must still buffer and verify the bounded body before serving those exact
    /// immutable bytes.
    ///
    /// The owning vehicle is resolved inside the query: in a device scope a
    /// head that no granted vehicle owns is `Ok(None)`, as for an unknown pack.
    pub(crate) fn pending_physical_v3_pack_for_digest(
        &self,
        digest: Sha256Digest,
        scope: VehicleScope,
    ) -> Result<Option<StoredPack>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let vehicle_id = connection
            .query_row(
                "SELECT admission.vehicle_id
                  FROM pending_physical_v3_packs AS pack
                   JOIN pending_physical_v3_admissions AS admission
                     ON admission.snapshot_id = pack.snapshot_id
                  WHERE pack.sha256 = ?1
                    AND admission.serve_state = 'public_first'
                    AND (?2 IS NULL OR EXISTS (SELECT 1 FROM device_vehicle_grants AS grants
                                                WHERE grants.device_id = ?2
                                                  AND grants.vehicle_id = admission.vehicle_id))",
                params![digest.to_string(), scope.grant_device_param()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        drop(connection);
        let Some(vehicle_id) = vehicle_id else {
            return Ok(None);
        };
        let vehicle_id = vehicle_id
            .parse::<Uuid>()
            .map_err(|_| StoreError::PhysicalV3AdmissionConflict)?;
        let admission = self
            .pending_physical_v3_admission_for_vehicle_with_file_digests(vehicle_id, false, true)?
            .ok_or(StoreError::PhysicalV3AdmissionConflict)?;
        if !physical_v3_admission_is_public(&admission)
            || !self.physical_v3_admission_served(vehicle_id, admission.snapshot_id)?
        {
            return Ok(None);
        }
        let pack = admission
            .manifest
            .chunks
            .iter()
            .find(|pack| pack.sha256 == digest)
            .ok_or(StoreError::PhysicalV3AdmissionConflict)?;
        Ok(Some(StoredPack {
            digest,
            compressed_bytes: pack.compressed_bytes,
            path: self
                .packs_dir
                .join("sha256")
                .join(format!("{digest}.sqlite.zst")),
        }))
    }

    pub(crate) fn retained_physical_v3_admission_for_receipt_at(
        &self,
        vehicle_id: Uuid,
        receipt_id: &str,
        now_ms: i64,
        verify_file_digests: bool,
    ) -> Result<Option<RetainedPhysicalV3Admission>, StoreError> {
        if !(0..=9_007_199_254_740_991).contains(&now_ms) {
            return Err(StoreError::PhysicalV3RetentionInvalid);
        }
        let connection = self.open_read_only_connection()?;
        let row = connection
            .query_row(
                "SELECT snapshot_id, installation_id, account_id, generation,
                        selected_car_id, profile, head_sequence, chunk_count,
                        manifest_sha256, ordered_chunks_sha256,
                        retained_at_ms, expires_at_ms, manifest_json
                   FROM retained_physical_v3_admissions
                  WHERE vehicle_id = ?1 AND receipt_id = ?2 AND expires_at_ms > ?3",
                params![vehicle_id.to_string(), receipt_id, now_ms],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, Vec<u8>>(12)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((
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
            retained_at_ms,
            expires_at_ms,
            manifest_json,
        )) = row
        else {
            return Ok(None);
        };
        let manifest: SyncManifest = serde_json::from_slice(&manifest_json)
            .map_err(StoreError::DeserializeManifest)?;
        validate_pending_physical_v3_manifest(&manifest)?;
        let computed = physical_v3_admission_from_manifest(&manifest, selected_car_id)?;
        if profile != HUB_SYNC_V1_1_3_PROFILE
            || vehicle_id != computed.vehicle_id
            || receipt_id != computed.receipt_id
            || snapshot_id != computed.snapshot_id.to_string()
            || installation_id != computed.installation_id.to_string()
            || account_id != computed.account_id.to_string()
            || u64::try_from(generation).ok() != Some(computed.manifest.generation)
            || u64::try_from(head_sequence).ok() != Some(computed.head_sequence)
            || u32::try_from(chunk_count).ok() != Some(computed.chunk_count)
            || manifest_sha256 != computed.manifest_sha256.to_string()
            || ordered_chunks_sha256 != computed.ordered_chunks_sha256.to_string()
            || retained_at_ms < 0
            || expires_at_ms <= retained_at_ms
        {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        let current_binding = self
            .v2_projection_binding(vehicle_id)
            .map_err(|_| StoreError::PhysicalV3AdmissionInvalid)?;
        if self.installation_id()? != computed.installation_id
            || !self.vehicle_is_active(vehicle_id)?
            || current_binding
                != (ProjectionBinding {
                    installation_id: computed.installation_id,
                    account_id: computed.account_id,
                    vehicle_id: computed.vehicle_id,
                    generation: computed.manifest.generation,
                    selected_car_id: computed.selected_car_id,
                })
        {
            return Err(StoreError::PhysicalV3AdmissionInvalid);
        }
        let pack_rows = connection
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
        if pack_rows.len() != manifest.chunks.len() {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        for (stored, pack) in pack_rows.iter().zip(&manifest.chunks) {
            let expected = (
                i64::from(pack.ordinal),
                pack.sha256.to_string(),
                pack.relative_path.clone(),
                i64::try_from(pack.compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
                i64::try_from(pack.uncompressed_bytes)
                    .map_err(|_| StoreError::PackSizeTooLarge)?,
            );
            if stored != &expected {
                return Err(StoreError::PhysicalV3AdmissionConflict);
            }
            let path = self
                .packs_dir
                .join("sha256")
                .join(format!("{}.sqlite.zst", pack.sha256));
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| StoreError::PhysicalV3AdmissionConflict)?;
            if !metadata.file_type().is_file() || metadata.len() != pack.compressed_bytes {
                return Err(StoreError::PhysicalV3AdmissionConflict);
            }
            if verify_file_digests {
                let authentic = sha256_file_hex(&path)
                    .map_err(|_| StoreError::PhysicalV3AdmissionConflict)?
                    == pack.sha256.to_string();
                #[cfg(test)]
                crate::hub_pack::record_verification("retained-pack-digest", pack.sha256, authentic);
                if !authentic {
                    return Err(StoreError::PhysicalV3AdmissionConflict);
                }
            }
        }
        Ok(Some(RetainedPhysicalV3Admission {
            admission: computed,
            retained_at_ms,
            expires_at_ms,
        }))
    }
}

fn insert_pending_physical_v3_admission(
    transaction: &Transaction<'_>,
    admission: &PendingPhysicalV3Admission,
    serve_state: &'static str,
) -> Result<(), StoreError> {
    let manifest_json =
        serde_json::to_vec(&admission.manifest).map_err(StoreError::SerializeManifest)?;
    transaction
        .execute(
            "INSERT INTO pending_physical_v3_admissions(
                vehicle_id, snapshot_id, installation_id, account_id,
                selected_car_id, profile, head_sequence, chunk_count,
                manifest_sha256, ordered_chunks_sha256, receipt_id,
                manifest_json, serve_state
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                admission.vehicle_id.to_string(),
                admission.snapshot_id.to_string(),
                admission.installation_id.to_string(),
                admission.account_id.to_string(),
                admission.selected_car_id,
                HUB_SYNC_V1_1_3_PROFILE,
                i64::try_from(admission.head_sequence)
                    .map_err(|_| StoreError::SequenceTooLarge)?,
                i64::from(admission.chunk_count),
                admission.manifest_sha256.to_string(),
                admission.ordered_chunks_sha256.to_string(),
                admission.receipt_id.as_str(),
                manifest_json,
                serve_state,
            ],
        )
        .map_err(StoreError::PublishManifest)?;
    for pack in &admission.manifest.chunks {
        transaction
            .execute(
                "INSERT INTO pending_physical_v3_packs(
                    sha256, snapshot_id, ordinal, relative_path,
                    compressed_bytes, uncompressed_bytes
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    pack.sha256.to_string(),
                    admission.snapshot_id.to_string(),
                    i64::from(pack.ordinal),
                    pack.relative_path,
                    i64::try_from(pack.compressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                    i64::try_from(pack.uncompressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                ],
            )
            .map_err(StoreError::PublishManifest)?;
    }
    Ok(())
}

fn insert_physical_v3_delta_transition(
    transaction: &Transaction<'_>,
    prior: &PendingPhysicalV3Admission,
    target: &PendingPhysicalV3Admission,
    delta: &crate::import::source_db::physical_delta_pack::StagedPhysicalDelta,
    packs_dir: &std::path::Path,
) -> Result<(), StoreError> {
    if delta.vehicle_id != target.vehicle_id
        || delta.base_receipt_id != prior.receipt_id
        || delta.target_receipt_id != target.receipt_id
        || delta.packs.is_empty() || delta.packs.len() > 64
        || delta.total_rows == 0 || delta.total_rows > 2_000_000
        || delta.impacted_roots_count > 10_000
        || delta.receipt_json.len() > 2_097_152
    {
        return Err(StoreError::PhysicalV3AdmissionInvalid);
    }
    let receipt: serde_json::Value = serde_json::from_slice(&delta.receipt_json)
        .map_err(|_| StoreError::PhysicalV3AdmissionInvalid)?;
    if receipt.get("kind").and_then(serde_json::Value::as_str) != Some("physical_changed_set")
        || receipt.pointer("/base/receipt_id").and_then(serde_json::Value::as_str)
            != Some(prior.receipt_id.as_str())
        || receipt.pointer("/target/receipt_id").and_then(serde_json::Value::as_str)
            != Some(target.receipt_id.as_str())
        || receipt.pointer("/base/manifest_sha256").and_then(serde_json::Value::as_str)
            != Some(delta.base_manifest_signed_sha256.to_string().as_str())
        || receipt.pointer("/target/manifest_sha256").and_then(serde_json::Value::as_str)
            != Some(delta.target_manifest_signed_sha256.to_string().as_str())
    {
        return Err(StoreError::PhysicalV3AdmissionInvalid);
    }
    let total_compressed: u64 = delta.packs.iter().map(|pack| pack.compressed_bytes).sum();
    let total_uncompressed: u64 = delta.packs.iter().map(|pack| pack.uncompressed_bytes).sum();
    if total_compressed > 268_435_456 || total_uncompressed > 2_147_483_648 {
        return Err(StoreError::PhysicalV3AdmissionInvalid);
    }
    for (index,pack) in delta.packs.iter().enumerate() {
        if pack.ordinal as usize != index || pack.compressed_bytes == 0
            || pack.compressed_bytes > 16_777_216 || pack.uncompressed_bytes == 0
            || pack.uncompressed_bytes > 268_435_456
            || pack.path != packs_dir.join("sha256").join(format!("{}.sqlite.zst",pack.sha256))
            || fs::metadata(&pack.path).map_err(|_| StoreError::PhysicalV3AdmissionConflict)?.len()
                != pack.compressed_bytes
            || {
                let authentic = sha256_file_hex(&pack.path)
                    .map_err(|_| StoreError::PhysicalV3AdmissionConflict)?
                    == pack.sha256.to_string();
                #[cfg(test)]
                crate::hub_pack::record_verification("delta-pack-digest", pack.sha256, authentic);
                !authentic
            }
        {
            return Err(StoreError::PhysicalV3AdmissionInvalid);
        }
    }
    transaction.execute(
        "INSERT INTO physical_v3_delta_transitions(
            target_receipt_id,base_receipt_id,vehicle_id,
            base_manifest_signed_sha256,target_manifest_signed_sha256,receipt_json,
            chunk_count,total_compressed_bytes,total_uncompressed_bytes,total_rows,impacted_roots_count
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![target.receipt_id,prior.receipt_id,target.vehicle_id.to_string(),
            delta.base_manifest_signed_sha256.to_string(),delta.target_manifest_signed_sha256.to_string(),
            delta.receipt_json,delta.packs.len() as i64,total_compressed as i64,
            total_uncompressed as i64,delta.total_rows as i64,delta.impacted_roots_count as i64],
    ).map_err(StoreError::PublishManifest)?;
    for pack in &delta.packs {
        transaction.execute(
            "INSERT INTO physical_v3_delta_packs
             (target_receipt_id,ordinal,sha256,relative_path,compressed_bytes,uncompressed_bytes)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![target.receipt_id,i64::from(pack.ordinal),pack.sha256.to_string(),
                format!("/v1/packs/sha256/{}.sqlite.zst",pack.sha256),
                pack.compressed_bytes as i64,pack.uncompressed_bytes as i64],
        ).map_err(StoreError::PublishManifest)?;
    }
    Ok(())
}

#[cfg(test)]
type PhysicalAdmissionReadHook = Box<dyn FnOnce()>;

#[cfg(test)]
thread_local! {
    pub(crate) static ROTATE_DURING_PHYSICAL_ADMISSION_READ:
        std::cell::Cell<Option<PhysicalAdmissionReadHook>> = const { std::cell::Cell::new(None) };
}

impl HubStore {
    pub(crate) fn physical_v3_delta_receipt_for_base_at(
        &self,
        vehicle_id: Uuid,
        base_receipt_id: &str,
        target_receipt_id: &str,
        now_ms: i64,
    ) -> Result<Option<(String, String, Vec<u8>)>, StoreError> {
        let row: Option<(String, String, Vec<u8>, String)> = self
            .open_read_only_connection()?
            .query_row(
                "SELECT delta.base_manifest_signed_sha256,
                        delta.target_manifest_signed_sha256,delta.receipt_json,target.snapshot_id
                   FROM physical_v3_delta_transitions AS delta
                   JOIN pending_physical_v3_admissions AS target
                     ON target.receipt_id=delta.target_receipt_id
                   JOIN retained_physical_v3_admissions AS base
                     ON base.receipt_id=delta.base_receipt_id
                  WHERE delta.vehicle_id=?1 AND delta.base_receipt_id=?2
                    AND delta.target_receipt_id=?3
                    AND target.vehicle_id=?1 AND target.serve_state='public_first'
                    AND base.vehicle_id=?1 AND base.expires_at_ms>?4
                    AND base.head_sequence+1=target.head_sequence",
                params![vehicle_id.to_string(),base_receipt_id,target_receipt_id,now_ms],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((base_sha, target_sha, receipt, target_snapshot)) = row else {
            return Ok(None);
        };
        let target_snapshot = target_snapshot
            .parse::<Uuid>()
            .map_err(|_| StoreError::PhysicalV3AdmissionConflict)?;
        if !self.physical_v3_admission_served(vehicle_id, target_snapshot)? {
            return Ok(None);
        }
        Ok(Some((base_sha, target_sha, receipt)))
    }

    /// The delta pack a digest names, served only to a scope that can see the
    /// vehicle the transition belongs to; the owner predicate sits before the
    /// LIMIT, so an ungranted digest is `Ok(None)`, as for an unknown one.
    pub(crate) fn physical_v3_delta_pack_for_digest_at(
        &self,
        digest: Sha256Digest,
        now_ms: i64,
        scope: VehicleScope,
    ) -> Result<Option<StoredPack>, StoreError> {
        let row: Option<(String, i64, String, String)> = self
            .open_read_only_connection()?
            .query_row(
                "SELECT packs.relative_path,packs.compressed_bytes,delta.vehicle_id,target.snapshot_id
                   FROM physical_v3_delta_packs AS packs
                   JOIN physical_v3_delta_transitions AS delta
                     ON delta.target_receipt_id=packs.target_receipt_id
                   JOIN pending_physical_v3_admissions AS target
                     ON target.receipt_id=delta.target_receipt_id
                   JOIN retained_physical_v3_admissions AS base
                     ON base.receipt_id=delta.base_receipt_id
                  WHERE packs.sha256=?1 AND target.serve_state='public_first'
                    AND base.expires_at_ms>?2
                    AND (?3 IS NULL OR EXISTS (SELECT 1 FROM device_vehicle_grants AS grants
                                                WHERE grants.device_id=?3
                                                  AND grants.vehicle_id=delta.vehicle_id))
                  LIMIT 1",
                params![digest.to_string(), now_ms, scope.grant_device_param()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((relative_path, bytes, vehicle_id, target_snapshot)) = row else {
            return Ok(None);
        };
        let (Ok(vehicle_id), Ok(target_snapshot)) =
            (vehicle_id.parse::<Uuid>(), target_snapshot.parse::<Uuid>())
        else {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        };
        if !self.physical_v3_admission_served(vehicle_id, target_snapshot)? {
            return Ok(None);
        }
        if relative_path != format!("/v1/packs/sha256/{digest}.sqlite.zst")
            || !(1..=16_777_216).contains(&bytes)
        {
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        Ok(Some(StoredPack {
            digest,
            compressed_bytes: bytes as u64,
            path: self.packs_dir.join("sha256").join(format!("{digest}.sqlite.zst")),
        }))
    }
}

/// The `hub_metadata` key of a vehicle's physical heads planned with the
/// import seam (M03-T21).
fn physical_v3_seam_planned_key(vehicle_id: Uuid) -> String {
    format!("physical_v3_seam_planned:{vehicle_id}")
}

/// The `hub_metadata` key of the coverage digests the vehicle's planned
/// physical heads were planned with (M03-T22).
fn physical_v3_seam_coverage_key(vehicle_id: Uuid) -> String {
    format!("physical_v3_seam_coverage:{vehicle_id}")
}

/// Every kept archive `connection`'s catalogue references, by vehicle
/// (DEC-250). A record that does not read back is refused.
pub(crate) fn physical_v3_kept_archives_in(
    connection: &Connection,
) -> Result<Vec<(Uuid, PhysicalV3KeptArchive)>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT key, value FROM hub_metadata WHERE key LIKE 'physical_v3_kept:%' ORDER BY key",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)?;
    let mut archives = Vec::new();
    for (key, value) in rows {
        let vehicle_id = key
            .strip_prefix("physical_v3_kept:")
            .and_then(|id| id.parse::<Uuid>().ok())
            .ok_or(StoreError::PhysicalV3AdmissionConflict)?;
        let kept: PhysicalV3Kept =
            serde_json::from_str(&value).map_err(|_| StoreError::PhysicalV3AdmissionConflict)?;
        if let Some(archive) = kept.archive {
            archives.push((vehicle_id, archive));
        }
    }
    Ok(archives)
}

/// The `hub_metadata` key of the stage and source kept for the vehicle's
/// handback (M03-T22).
fn physical_v3_kept_key(vehicle_id: Uuid) -> String {
    format!("physical_v3_kept:{vehicle_id}")
}

fn physical_v3_handback_plan_key(vehicle_id: Uuid) -> String {
    format!("physical_v3_handback_plan:{vehicle_id}")
}

/// The inputs a vehicle's planned physical head was a function of
/// (M08-T19): a handback whose inputs have the same digest plans the same
/// head, so it serves `snapshot_id` again under the new coverage.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PhysicalV3HandbackPlan {
    pub(crate) snapshot_id: String,
    /// The instants the kept stage's source children span, inclusive.
    pub(crate) span: Option<(i64, i64)>,
    pub(crate) inputs: String,
    /// Whether the optional prepared month was made for this head with
    /// these inputs. Without it the early answer serves the head and runs
    /// the preparation alone (M08-T19).
    #[serde(default)]
    pub(crate) prepared: bool,
}

/// What is kept for a vehicle's handback (M03-T22): the logical digest of
/// the physical stage and the digest of the import's source sessions. With
/// `archive` the base is one compact kept archive and its generation's
/// source file (DEC-250); without it, a record an earlier build wrote, the
/// legacy raw stage and source file, readable until a verified conversion
/// replaces the record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PhysicalV3Kept {
    pub(crate) stage: String,
    pub(crate) source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) archive: Option<PhysicalV3KeptArchive>,
}

/// The kept archive of a vehicle's replay base (DEC-250): its files are
/// `<vehicle>.<generation>.kept` and `<vehicle>.<generation>.source.json`
/// in the kept directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PhysicalV3KeptArchive {
    pub(crate) generation: Uuid,
    pub(crate) format: u32,
    /// SHA-256 of the whole archive file.
    pub(crate) digest: String,
    pub(crate) bytes: u64,
    pub(crate) decoded_bytes: u64,
}

/// The coverage digest each planned head was planned with; none recorded is
/// an empty map (M03-T22).
fn physical_v3_seam_coverage_in_connection(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM hub_metadata WHERE key = ?1",
            params![physical_v3_seam_coverage_key(vehicle_id)],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    match value {
        Some(value) => {
            serde_json::from_str(&value).map_err(|_| StoreError::PhysicalV3AdmissionConflict)
        }
        None => Ok(std::collections::BTreeMap::new()),
    }
}

/// The snapshot ids of the vehicle's physical heads planned with the import
/// seam; none recorded is an empty set.
fn physical_v3_seam_planned_in_connection(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<std::collections::BTreeSet<String>, StoreError> {
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM hub_metadata WHERE key = ?1",
            params![physical_v3_seam_planned_key(vehicle_id)],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    match value {
        Some(value) => {
            serde_json::from_str(&value).map_err(|_| StoreError::PhysicalV3AdmissionConflict)
        }
        None => Ok(std::collections::BTreeSet::new()),
    }
}

fn insert_retained_physical_v3_admission(
    transaction: &Transaction<'_>,
    admission: &PendingPhysicalV3Admission,
    retained_at_ms: i64,
    expires_at_ms: i64,
) -> Result<(), StoreError> {
    let manifest_json =
        serde_json::to_vec(&admission.manifest).map_err(StoreError::SerializeManifest)?;
    transaction
        .execute(
            "INSERT INTO retained_physical_v3_admissions(
                vehicle_id, snapshot_id, installation_id, account_id,
                generation, selected_car_id, profile, head_sequence,
                chunk_count, manifest_sha256, ordered_chunks_sha256,
                receipt_id, retained_at_ms, expires_at_ms, manifest_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                admission.vehicle_id.to_string(),
                admission.snapshot_id.to_string(),
                admission.installation_id.to_string(),
                admission.account_id.to_string(),
                i64::try_from(admission.manifest.generation)
                    .map_err(|_| StoreError::SequenceTooLarge)?,
                admission.selected_car_id,
                HUB_SYNC_V1_1_3_PROFILE,
                i64::try_from(admission.head_sequence)
                    .map_err(|_| StoreError::SequenceTooLarge)?,
                i64::from(admission.chunk_count),
                admission.manifest_sha256.to_string(),
                admission.ordered_chunks_sha256.to_string(),
                admission.receipt_id.as_str(),
                retained_at_ms,
                expires_at_ms,
                manifest_json,
            ],
        )
        .map_err(StoreError::PublishManifest)?;
    for pack in &admission.manifest.chunks {
        transaction
            .execute(
                "INSERT INTO retained_physical_v3_packs(
                    receipt_id, ordinal, sha256, relative_path,
                    compressed_bytes, uncompressed_bytes
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    admission.receipt_id.as_str(),
                    i64::from(pack.ordinal),
                    pack.sha256.to_string(),
                    pack.relative_path,
                    i64::try_from(pack.compressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                    i64::try_from(pack.uncompressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                ],
            )
            .map_err(StoreError::PublishManifest)?;
    }
    Ok(())
}

pub(crate) fn physical_v3_admission_is_public(admission: &PendingPhysicalV3Admission) -> bool {
    (1..=9_007_199_254_740_991).contains(&admission.head_sequence)
        && (1..=1_771).contains(&admission.manifest.chunks.len())
        && admission
            .manifest
            .chunks
            .iter()
            .all(|pack| (1..=16 * 1024 * 1024).contains(&pack.compressed_bytes))
}

pub(crate) fn physical_v3_admission_from_manifest(
    manifest: &SyncManifest,
    selected_car_id: i64,
) -> Result<PendingPhysicalV3Admission, StoreError> {
    if i16::try_from(selected_car_id).is_err() {
        return Err(StoreError::PhysicalV3AdmissionInvalid);
    }
    let manifest_json = serde_json::to_vec(manifest).map_err(StoreError::SerializeManifest)?;
    let manifest_sha256 = Sha256Digest::of_bytes(&manifest_json);
    let mut chunks = Sha256::new();
    chunks.update(b"teslatlas-hub/hub-sync-v1/1.3.0/physical-chunks/v1\0"); // rename:keep
    chunks.update(
        u32::try_from(manifest.chunks.len())
            .map_err(|_| StoreError::PhysicalV3AdmissionInvalid)?
            .to_be_bytes(),
    );
    for pack in &manifest.chunks {
        chunks.update(pack.ordinal.to_be_bytes());
        chunks.update(pack.sha256.as_bytes());
    }
    let ordered_chunks_sha256 = Sha256Digest::from_bytes(chunks.finalize().into());
    let mut receipt = Sha256::new();
    receipt.update(b"teslatlas-hub/hub-sync-v1/1.3.0/physical-receipt/v1\0"); // rename:keep
    receipt.update(manifest.installation_id.as_bytes());
    receipt.update(manifest.account_id.as_bytes());
    receipt.update(manifest.vehicle_id.as_bytes());
    receipt.update(selected_car_id.to_be_bytes());
    receipt.update(manifest.snapshot_id.as_bytes());
    receipt.update(manifest.head_sequence.to_be_bytes());
    receipt.update(manifest_sha256.as_bytes());
    receipt.update(ordered_chunks_sha256.as_bytes());
    let receipt_id = format!("{PHYSICAL_V3_RECEIPT_PREFIX}{}", hex::encode(receipt.finalize()));
    Ok(PendingPhysicalV3Admission {
        installation_id: manifest.installation_id,
        account_id: manifest.account_id,
        vehicle_id: manifest.vehicle_id,
        selected_car_id,
        snapshot_id: manifest.snapshot_id,
        head_sequence: manifest.head_sequence,
        chunk_count: manifest.chunk_count,
        manifest_sha256,
        ordered_chunks_sha256,
        receipt_id,
        manifest: manifest.clone(),
    })
}

fn validate_pending_physical_v3_manifest(manifest: &SyncManifest) -> Result<(), StoreError> {
    if manifest.schema != HUB_PROJECTION_SCHEMA_V3
        || manifest.mode != crate::protocol::TransferMode::FullSnapshot
    {
        return Err(StoreError::PhysicalV3AdmissionInvalid);
    }
    manifest
        .validate_with_limits(ProtocolLimits::hub_sync_v1_1_3_schema_2_2())
        .map_err(StoreError::Manifest)
}
