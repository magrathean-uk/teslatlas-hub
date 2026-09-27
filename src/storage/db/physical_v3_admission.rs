// SPDX-License-Identifier: AGPL-3.0-only

const HUB_SYNC_V1_1_3_PROFILE: &str = "hub-sync-v1@1.3.0";
const PHYSICAL_V3_RECEIPT_PREFIX: &str = "pv3_";

impl HubStore {
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
        let manifest_json =
            serde_json::to_vec(&admission.manifest).map_err(StoreError::SerializeManifest)?;
        transaction
            .execute(
                "INSERT INTO pending_physical_v3_admissions(
                    vehicle_id, snapshot_id, installation_id, account_id,
                    selected_car_id, profile,
                    head_sequence, chunk_count, manifest_sha256,
                    ordered_chunks_sha256, receipt_id, manifest_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
        self.commit_physical_v3_admission(transaction, &admission)?;
        Ok(admission)
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

    pub(crate) fn pending_physical_v3_admission_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<PendingPhysicalV3Admission>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let row = connection
            .query_row(
                "SELECT admission.snapshot_id, admission.installation_id,
                        admission.account_id, admission.selected_car_id,
                        admission.profile,
                        admission.head_sequence, admission.chunk_count,
                        admission.manifest_sha256, admission.ordered_chunks_sha256,
                        admission.receipt_id, admission.manifest_json
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
        )) = row
        else {
            return Ok(None);
        };
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
            if sha256_file_hex(&path).map_err(|_| StoreError::PhysicalV3AdmissionConflict)?
                != pack.sha256.to_string()
            {
                return Err(StoreError::PhysicalV3AdmissionConflict);
            }
        }
        Ok(Some(computed))
    }
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
