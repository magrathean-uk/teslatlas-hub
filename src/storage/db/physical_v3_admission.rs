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
                    {
                        if let Err(source) = self.remove_unretained_pack(
                            publication_gate,
                            chunk.metadata.sha256,
                            &chunk.path,
                        ) && cleanup_error.is_none()
                        {
                            cleanup_error = Some(source);
                        }
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

    /// Atomically replace the first admitted physical head while retaining its
    /// exact signed checkpoint for a bounded future rebase response. The new
    /// head is deliberately blocked from public control and pack routes until
    /// the retained-prior 409 adapter is installed in a later cut.
    pub(crate) fn rotate_pending_physical_v3_admission_at(
        &self,
        publication_gate: &PublicationGate,
        mut candidate: crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
        retained_at_ms: i64,
    ) -> Result<PendingPhysicalV3Admission, StoreError> {
        let result = self.rotate_pending_physical_v3_admission_inner(&candidate, retained_at_ms);
        candidate.retain_catalogued_objects();
        match result {
            Ok(admission) => Ok(admission),
            Err(error) => {
                let mut cleanup_error = None;
                for chunk in &candidate.chunks {
                    if chunk.ownership()
                        == crate::hub_pack::ProjectionPackOwnership::Created
                    {
                        if let Err(source) = self.remove_unretained_pack(
                            publication_gate,
                            chunk.metadata.sha256,
                            &chunk.path,
                        ) && cleanup_error.is_none()
                        {
                            cleanup_error = Some(source);
                        }
                    }
                }
                Err(cleanup_error.unwrap_or(error))
            }
        }
    }

    fn rotate_pending_physical_v3_admission_inner(
        &self,
        candidate: &crate::import::teslamate::physical_fragments::StagedPhysicalProjectionV3,
        retained_at_ms: i64,
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
        if retained_physical_v3_row_exists(&transaction, next.vehicle_id)? {
            return Err(StoreError::PhysicalV3SecondHeadUnsupported(next.vehicle_id));
        }
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
        insert_pending_physical_v3_admission(&transaction, &next, "blocked_rotation")?;
        self.commit_physical_v3_rotation(
            transaction,
            &prior,
            &next,
            retained_at_ms,
            expires_at_ms,
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
    ) -> Result<ManifestCommitState, StoreError> {
        let current = self.pending_physical_v3_admission_for_vehicle(next.vehicle_id);
        let retained = self.retained_physical_v3_admission_for_receipt_at(
            next.vehicle_id,
            &prior.receipt_id,
            retained_at_ms,
            true,
        );
        match (current, retained) {
            (Ok(Some(current)), Ok(Some(retained)))
                if current == *next
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

    pub(crate) fn pending_physical_v3_admission_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<PendingPhysicalV3Admission>, StoreError> {
        self.pending_physical_v3_admission_for_vehicle_with_file_digests(vehicle_id, true, false)
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
        if now_ms < 0 || now_ms > 9_007_199_254_740_991 {
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

fn retained_physical_v3_row_exists(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<bool, StoreError> {
    transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM retained_physical_v3_admissions WHERE vehicle_id = ?1
             )",
            [vehicle_id.to_string()],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)
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

fn physical_v3_admission_from_manifest(
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
