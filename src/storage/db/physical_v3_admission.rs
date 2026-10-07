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
        candidate: crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
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
        candidate: crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
        retained_receipt_id: &str,
        retained_at_ms: i64,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let publication_gate = self.try_acquire_publication_gate()?;
        let rotated = self.rotate_pending_physical_v3_admission_at(
            &publication_gate,
            candidate,
            retained_at_ms,
        )?;
        match self.pending_physical_v3_control_admission_for_vehicle(rotated.vehicle_id) {
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
            .pending_physical_v3_control_admission_for_vehicle(active.vehicle_id)?
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
        mut candidate: crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
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
        candidate: &crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
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
        candidate: crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
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
        candidate: crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
        delta: Option<crate::import::teslamate::physical_delta_pack::StagedPhysicalDelta>,
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
        candidate: crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
        delta: Option<crate::import::teslamate::physical_delta_pack::StagedPhysicalDelta>,
        retained_at_ms: i64,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        self.rotate_physical_v3_admission_at(publication_gate, candidate, delta, retained_at_ms, true)
    }

    fn rotate_physical_v3_admission_at(
        &self,
        publication_gate: &PublicationGate,
        mut candidate: crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
        mut delta: Option<crate::import::teslamate::physical_delta_pack::StagedPhysicalDelta>,
        retained_at_ms: i64,
        public: bool,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let serve_state = if public { "public_first" } else { "blocked_rotation" };
        let result = self.rotate_pending_physical_v3_admission_inner(
            &candidate, delta.as_ref(), retained_at_ms, serve_state,
        );
        candidate.retain_catalogued_objects();
        match result {
            Ok(admission) => {
                if let Some(delta) = delta.as_mut() { delta.retain_catalogued_objects(); }
                Ok(admission)
            }
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

    fn rotate_pending_physical_v3_admission_inner(
        &self,
        candidate: &crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
        delta: Option<&crate::import::teslamate::physical_delta_pack::StagedPhysicalDelta>,
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
                && self.pending_physical_v3_control_admission_for_vehicle(next.vehicle_id)?.as_ref() != Some(&prior)
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
        candidate: &crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let manifest = &candidate.manifest;
        let limits = ProtocolLimits::hub_sync_v1_1_3_schema_2_2();
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
            built
                .verify_with_limits(limits)
                .map_err(StoreError::PhysicalV3Pack)?;
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
                ManifestCommitState::Conflicting => {
                    Err(StoreError::PhysicalV3AdmissionConflict)
                }
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
            (Ok(Some(current)), Ok(None)) if current == *prior => {
                Ok(ManifestCommitState::Absent)
            }
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
    pub(crate) fn pending_physical_v3_control_admission_for_vehicle(
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
        let connection = self.open_read_only_connection()?;
        let row = connection
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
        let pack_rows = connection
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
            if verify_file_digests
                && sha256_file_hex(&path).map_err(|_| StoreError::PhysicalV3AdmissionConflict)?
                    != pack.sha256.to_string()
            {
                return Err(StoreError::PhysicalV3AdmissionConflict);
            }
        }
        Ok(Some(computed))
    }

    /// Resolve one exact pack from a currently valid private physical
    /// admission without adding it to the generic sync catalogue. The caller
    /// must still buffer and verify the bounded body before serving those exact
    /// immutable bytes.
    pub(crate) fn pending_physical_v3_pack_for_digest(
        &self,
        digest: Sha256Digest,
    ) -> Result<Option<StoredPack>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let vehicle_id = connection
            .query_row(
                "SELECT admission.vehicle_id
                  FROM pending_physical_v3_packs AS pack
                   JOIN pending_physical_v3_admissions AS admission
                     ON admission.snapshot_id = pack.snapshot_id
                  WHERE pack.sha256 = ?1
                    AND admission.serve_state = 'public_first'",
                [digest.to_string()],
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
        if !physical_v3_admission_is_public(&admission) {
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
            if verify_file_digests
                && sha256_file_hex(&path).map_err(|_| StoreError::PhysicalV3AdmissionConflict)?
                    != pack.sha256.to_string()
            {
                return Err(StoreError::PhysicalV3AdmissionConflict);
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
    delta: &crate::import::teslamate::physical_delta_pack::StagedPhysicalDelta,
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
            || sha256_file_hex(&pack.path).map_err(|_| StoreError::PhysicalV3AdmissionConflict)?
                != pack.sha256.to_string()
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

impl HubStore {
    pub(crate) fn physical_v3_delta_receipt_for_base_at(
        &self,
        vehicle_id: Uuid,
        base_receipt_id: &str,
        target_receipt_id: &str,
        now_ms: i64,
    ) -> Result<Option<(String, String, Vec<u8>)>, StoreError> {
        self.open_read_only_connection()?
            .query_row(
                "SELECT delta.base_manifest_signed_sha256,
                        delta.target_manifest_signed_sha256,delta.receipt_json
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
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::Query)
    }

    pub(crate) fn physical_v3_delta_pack_for_digest_at(
        &self,
        digest: Sha256Digest,
        now_ms: i64,
    ) -> Result<Option<StoredPack>, StoreError> {
        let row: Option<(String, i64)> = self.open_read_only_connection()?
            .query_row(
                "SELECT packs.relative_path,packs.compressed_bytes
                   FROM physical_v3_delta_packs AS packs
                   JOIN physical_v3_delta_transitions AS delta
                     ON delta.target_receipt_id=packs.target_receipt_id
                   JOIN pending_physical_v3_admissions AS target
                     ON target.receipt_id=delta.target_receipt_id
                   JOIN retained_physical_v3_admissions AS base
                     ON base.receipt_id=delta.base_receipt_id
                  WHERE packs.sha256=?1 AND target.serve_state='public_first'
                    AND base.expires_at_ms>?2
                  LIMIT 1",
                params![digest.to_string(),now_ms],
                |row| Ok((row.get(0)?,row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((relative_path,bytes)) = row else { return Ok(None); };
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
    chunks.update(b"teslatlas-hub/hub-sync-v1/1.3.0/physical-chunks/v1\0");
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
    receipt.update(b"teslatlas-hub/hub-sync-v1/1.3.0/physical-receipt/v1\0");
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
