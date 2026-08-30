// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    /// All configured cars eligible for account-owned collection. V2 bindings
    /// and Tesla EIDs are durable setup/import facts; never infer targets from
    /// mutable discovery data.
    pub fn configured_tesla_vehicles(
        &self,
    ) -> Result<Vec<(Uuid, i64, ProjectionCarSettings)>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT binding.vehicle_id, eid.alias_value,
                        settings.enabled, settings.use_streaming_api,
                        settings.suspend_after_idle_min, settings.suspend_min,
                        settings.req_not_unlocked, settings.free_supercharging,
                        settings.lfp_battery, settings.suspend_min_resolved,
                        car.car_json
                   FROM v2_base_bindings AS binding
                   LEFT JOIN vehicle_identity_aliases AS eid
                     ON eid.vehicle_id = binding.vehicle_id
                    AND eid.alias_kind = 'tesla_eid'
                   LEFT JOIN car_settings AS settings
                     ON settings.vehicle_id = binding.vehicle_id
                   LEFT JOIN materialised_cars AS car
                     ON car.vehicle_id = binding.vehicle_id
                  ORDER BY binding.vehicle_id, eid.alias_value",
            )
            .map_err(StoreError::Query)?;
        let mut rows = statement.query([]).map_err(StoreError::Query)?;
        let mut configured = Vec::new();
        let mut previous_vehicle_id = None;
        while let Some(row) = rows.next().map_err(StoreError::Query)? {
            let vehicle_id = Uuid::parse_str(&row.get::<_, String>(0).map_err(StoreError::Query)?)
                .map_err(|_| StoreError::InvalidStoredUuid("configured vehicle"))?;
            if previous_vehicle_id == Some(vehicle_id) {
                return Err(StoreError::LineageCatalogConflict);
            }
            previous_vehicle_id = Some(vehicle_id);
            let eid = row
                .get::<_, Option<String>>(1)
                .map_err(StoreError::Query)?
                .and_then(|eid| eid.parse::<i64>().ok())
                .filter(|eid| *eid > 0)
                .ok_or(StoreError::LineageCatalogConflict)?;
            let enabled = row.get::<_, Option<i64>>(2).map_err(StoreError::Query)?;
            let settings = match enabled {
                None => row
                    .get::<_, Option<String>>(10)
                    .map_err(StoreError::Query)?
                    .map(|car| {
                        serde_json::from_str::<ProjectionCar>(&car)
                            .map(|car| car.settings)
                            .map_err(StoreError::DeserializeLifecycleRow)
                    })
                    .transpose()?
                    .unwrap_or_default(),
                Some(enabled) => ProjectionCarSettings {
                    enabled: enabled != 0,
                    use_streaming_api: row
                        .get::<_, Option<i64>>(3)
                        .map_err(StoreError::Query)?
                        .unwrap_or_default()
                        != 0,
                    suspend_after_idle_min: row
                        .get::<_, Option<i64>>(4)
                        .map_err(StoreError::Query)?
                        .unwrap_or_default(),
                    suspend_min: row
                        .get::<_, Option<i64>>(5)
                        .map_err(StoreError::Query)?
                        .unwrap_or_default(),
                    req_not_unlocked: row
                        .get::<_, Option<i64>>(6)
                        .map_err(StoreError::Query)?
                        .unwrap_or_default()
                        != 0,
                    free_supercharging: row
                        .get::<_, Option<i64>>(7)
                        .map_err(StoreError::Query)?
                        .unwrap_or_default()
                        != 0,
                    lfp_battery: row
                        .get::<_, Option<i64>>(8)
                        .map_err(StoreError::Query)?
                        .unwrap_or_default()
                        != 0,
                    suspend_min_resolved: row
                        .get::<_, Option<i64>>(9)
                        .map_err(StoreError::Query)?
                        .unwrap_or_default()
                        != 0,
                },
            };
            configured.push((vehicle_id, eid, settings));
        }
        Ok(configured)
    }

    /// Resolve the durable Tesla EID and VIN for one configured Hub vehicle.
    /// Vehicle commands use this exact identity instead of mutable discovery.
    pub fn configured_tesla_vehicle_identity(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<(i64, Option<String>)>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let row: Option<(i64, Option<String>, Option<String>)> = connection
            .query_row(
                "SELECT COUNT(eid.alias_value), MIN(eid.alias_value), vehicle.vin
                   FROM v2_base_bindings AS binding
                   JOIN vehicles AS vehicle ON vehicle.vehicle_id = binding.vehicle_id
                   LEFT JOIN vehicle_identity_aliases AS eid
                     ON eid.vehicle_id = binding.vehicle_id
                    AND eid.alias_kind = 'tesla_eid'
                  WHERE binding.vehicle_id = ?1
                  GROUP BY binding.vehicle_id, vehicle.vin",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        row.map(|(eid_count, eid, vin)| {
            if eid_count != 1 {
                return Err(StoreError::LineageCatalogConflict);
            }
            let eid = eid
                .and_then(|eid| eid.parse::<i64>().ok())
                .filter(|eid| *eid > 0)
                .ok_or(StoreError::LineageCatalogConflict)?;
            Ok((eid, vin))
        })
        .transpose()
    }

    /// Compatibility helper for call sites which intentionally require one car.
    pub fn selected_tesla_eid(&self) -> Result<Option<(i64, ProjectionCarSettings)>, StoreError> {
        let configured = self.configured_tesla_vehicles()?;
        match configured.as_slice() {
            [] => Ok(None),
            [(_, eid, settings)] => Ok(Some((*eid, settings.clone()))),
            _ => Err(StoreError::LineageCatalogConflict),
        }
    }

    pub fn v2_lineage_pack_count(&self, vehicle_id: Uuid) -> Result<usize, StoreError> {
        let lineage = self
            .lineage_manifest_for_vehicle_with_verification(
                vehicle_id,
                LineagePackVerification::MetadataOnly,
            )?
            .ok_or(StoreError::LineageCatalogConflict)?;
        lineage
            .base
            .packs
            .len()
            .checked_add(lineage.deltas.len())
            .ok_or(StoreError::LineageCapacityExhausted)
    }

    /// Build an exact collector-owned suffix plan from durable provenance.
    ///
    /// Imported successors deliberately create no `sync_live_delta_spans`
    /// row. Therefore this walks backwards from the current head and stops at
    /// the first non-collector delta instead of ever guessing its contents.
    pub fn plan_live_delta_compaction(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<LiveDeltaCompactionPlan>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let vehicle_key = vehicle_id.to_string();
        let (snapshot_id, head_sequence, head_digest): (String, i64, String) = connection
            .query_row(
                "SELECT heads.base_snapshot_id, heads.head_sequence, heads.head_digest
                 FROM sync_heads AS heads WHERE heads.vehicle_id = ?1",
                params![vehicle_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        let base_snapshot_id =
            Uuid::parse_str(&snapshot_id).map_err(|_| StoreError::LineageCatalogConflict)?;
        let head_sequence =
            u64::try_from(head_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        let head_digest = head_digest
            .parse::<Sha256Digest>()
            .map_err(|_| StoreError::LineageCatalogConflict)?;

        let mut statement = connection
            .prepare(
                "SELECT deltas.from_sequence, deltas.to_sequence,
                        deltas.parent_chain_digest, deltas.chain_digest,
                        deltas.pack_digest, deltas.pack_json,
                        spans.from_revision, spans.to_revision
                 FROM sync_live_delta_spans AS spans
                 JOIN sync_deltas AS deltas
                   ON deltas.vehicle_id = spans.vehicle_id
                  AND deltas.from_sequence = spans.from_sequence
                  AND deltas.to_sequence = spans.to_sequence
                 WHERE spans.vehicle_id = ?1
                 ORDER BY deltas.to_sequence DESC",
            )
            .map_err(StoreError::LineageCatalog)?;
        let rows = statement
            .query_map(params![vehicle_key.as_str()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .map_err(StoreError::LineageCatalog)?;
        let mut expected_to = head_sequence;
        let mut expected_digest = head_digest;
        let mut reverse_spans = Vec::new();
        for row in rows {
            let (
                from_sequence,
                to_sequence,
                parent_chain_digest,
                chain_digest,
                pack_digest,
                pack_json,
                from_revision,
                to_revision,
            ) = row.map_err(StoreError::LineageCatalog)?;
            let from_sequence =
                u64::try_from(from_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
            let to_sequence =
                u64::try_from(to_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
            if reverse_spans.is_empty()
                && (to_sequence != expected_to || chain_digest != expected_digest.to_string())
            {
                return Ok(None);
            }
            if to_sequence != expected_to || chain_digest != expected_digest.to_string() {
                break;
            }
            let delta: LineageDelta =
                serde_json::from_slice(&pack_json).map_err(StoreError::DeserializeManifest)?;
            if delta.from_sequence != from_sequence
                || delta.to_sequence != to_sequence
                || delta.parent_chain_digest.to_string() != parent_chain_digest
                || delta.chain_digest.to_string() != chain_digest
                || delta.pack_digest.to_string() != pack_digest
                || delta.pack.snapshot_id != base_snapshot_id
                || to_sequence - from_sequence
                    != u64::try_from(to_revision - from_revision + 1)
                        .map_err(|_| StoreError::LineageCatalogConflict)?
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            self.verify_lineage_pack(&delta.pack)?;
            expected_to = from_sequence;
            expected_digest = delta.parent_chain_digest;
            reverse_spans.push(LiveDeltaCompactionSpan {
                delta,
                from_revision,
                to_revision,
            });
        }
        drop(statement);
        if reverse_spans.len() < 2 {
            return Ok(None);
        }
        reverse_spans.reverse();
        if reverse_spans.windows(2).any(|window| {
            window[0].delta.to_sequence != window[1].delta.from_sequence
                || window[0].delta.chain_digest != window[1].delta.parent_chain_digest
                || window[0].to_revision.checked_add(1) != Some(window[1].from_revision)
                || window[0].delta.pack.ordinal.checked_add(1) != Some(window[1].delta.pack.ordinal)
        }) {
            return Err(StoreError::LineageCatalogConflict);
        }
        let first = reverse_spans
            .first()
            .expect("two spans prove a first compaction span");
        let last = reverse_spans
            .last()
            .expect("two spans prove a final compaction span");
        let from_revision = first.from_revision;
        let to_revision = last.to_revision;
        let expected_revision_count = to_revision
            .checked_sub(from_revision)
            .and_then(|count| count.checked_add(1))
            .ok_or(StoreError::LineageCatalogConflict)?;
        let (actual_count, minimum_revision, maximum_revision, published_count): (
            i64,
            Option<i64>,
            Option<i64>,
            i64,
        ) = connection
            .query_row(
                "SELECT COUNT(*), MIN(revision), MAX(revision),
                        COALESCE(SUM(CASE WHEN published = 1 THEN 1 ELSE 0 END), 0)
                 FROM sync_mutations
                 WHERE vehicle_id = ?1 AND revision BETWEEN ?2 AND ?3",
                params![vehicle_key.as_str(), from_revision, to_revision],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(StoreError::LineageCatalog)?;
        if actual_count != expected_revision_count
            || published_count != expected_revision_count
            || minimum_revision != Some(from_revision)
            || maximum_revision != Some(to_revision)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let mut mutation_statement = connection
            .prepare(
                "SELECT mutation.revision, mutation.entity, mutation.entity_id,
                        mutation.car_id, mutation.operation, mutation.payload_json
                 FROM sync_mutations AS mutation
                 WHERE mutation.vehicle_id = ?1
                   AND mutation.revision BETWEEN ?2 AND ?3
                   AND NOT EXISTS (
                       SELECT 1 FROM sync_mutations AS newer
                       WHERE newer.vehicle_id = mutation.vehicle_id
                         AND newer.revision BETWEEN ?2 AND ?3
                         AND newer.entity = mutation.entity
                         AND newer.entity_id = mutation.entity_id
                         AND newer.revision > mutation.revision
                   )
                 ORDER BY mutation.revision, mutation.entity, mutation.entity_id",
            )
            .map_err(StoreError::LineageCatalog)?;
        let mutations = mutation_statement
            .query_map(
                params![vehicle_key.as_str(), from_revision, to_revision],
                |row| {
                    Ok(SyncMutation {
                        vehicle_id,
                        revision: row.get(0)?,
                        entity: row.get(1)?,
                        entity_id: row.get(2)?,
                        car_id: row.get(3)?,
                        operation: row.get(4)?,
                        payload_json: row.get(5)?,
                    })
                },
            )
            .map_err(StoreError::LineageCatalog)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::LineageCatalog)?;
        if mutations.is_empty() {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(Some(LiveDeltaCompactionPlan {
            vehicle_id,
            base_snapshot_id,
            anchor_sequence: first.delta.from_sequence,
            anchor_digest: first.delta.parent_chain_digest,
            head_sequence: last.delta.to_sequence,
            head_digest: last.delta.chain_digest,
            first_ordinal: first.delta.pack.ordinal,
            from_revision,
            to_revision,
            mutations,
            replaced_spans: reverse_spans,
        }))
    }

    /// Rebuild a compacted collector suffix from the journal payloads that
    /// were committed atomically with the materialised rows. This never reads
    /// newer mutable state, so concurrent collection cannot leak a future row
    /// into an earlier lineage sequence.
    pub fn projection_delta_for_compaction(
        &self,
        plan: &LiveDeltaCompactionPlan,
        binding: ProjectionBinding,
    ) -> Result<ProjectionDelta, StoreError> {
        if plan.vehicle_id != binding.vehicle_id
            || plan.anchor_sequence >= plan.head_sequence
            || plan.from_revision <= 0
            || plan.to_revision < plan.from_revision
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let mut final_mutations = HashMap::<(String, i64), SyncMutation>::new();
        for mutation in &plan.mutations {
            if mutation.vehicle_id != plan.vehicle_id
                || mutation.revision < plan.from_revision
                || mutation.revision > plan.to_revision
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            let key = (mutation.entity.clone(), mutation.entity_id);
            match final_mutations.entry(key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(mutation.clone());
                }
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if mutation.revision > entry.get().revision =>
                {
                    entry.insert(mutation.clone());
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
        let mut ordered = final_mutations.into_values().collect::<Vec<_>>();
        ordered.sort_by_key(|mutation| {
            (
                mutation.revision,
                mutation.entity.clone(),
                mutation.entity_id,
            )
        });
        let car_upserts = ordered
            .iter()
            .filter(|mutation| mutation.entity == "car" && mutation.operation == "upsert")
            .map(|mutation| (mutation.entity_id, mutation.revision))
            .collect::<HashMap<_, _>>();
        let mut settings = HashMap::<i64, (i64, ProjectionCarSettings)>::new();
        for mutation in &ordered {
            if mutation.entity == "car_setting" && mutation.operation == "upsert" {
                settings.insert(
                    mutation.entity_id,
                    (
                        mutation.revision,
                        serde_json::from_str(&mutation.payload_json)
                            .map_err(StoreError::DeserializeLifecycleRow)?,
                    ),
                );
            }
        }
        let mut delta = ProjectionDelta {
            binding,
            sequence: SequenceRange {
                from_exclusive: plan.anchor_sequence,
                to_inclusive: plan.head_sequence,
            },
            parent_digest: plan.anchor_digest,
            cars: Vec::new(),
            car_settings: Vec::new(),
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
            tombstones: Vec::new(),
        };
        for mutation in ordered {
            let entity = parse_sync_entity(&mutation.entity).ok_or_else(|| {
                StoreError::SyncMutation(format!("unknown entity {}", mutation.entity))
            })?;
            if mutation.operation == "tombstone" {
                delta.tombstones.push(ProjectionTombstone {
                    entity,
                    id: mutation.entity_id,
                    car_id: mutation.car_id,
                });
                continue;
            }
            if mutation.operation != "upsert" {
                return Err(StoreError::SyncMutation(
                    "invalid mutation operation".into(),
                ));
            }
            match entity {
                ProjectionDeltaEntity::Car => {
                    let mut car: ProjectionCar = serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?;
                    if let Some((_, settings)) = settings
                        .get(&mutation.entity_id)
                        .filter(|(revision, _)| *revision > mutation.revision)
                    {
                        car.settings = settings.clone();
                    }
                    delta.cars.push(car);
                }
                ProjectionDeltaEntity::CarSetting => {
                    if !car_upserts.contains_key(&mutation.entity_id) {
                        delta.car_settings.push(ProjectionCarSettingsPatch {
                            car_id: mutation.entity_id,
                            settings: serde_json::from_str(&mutation.payload_json)
                                .map_err(StoreError::DeserializeLifecycleRow)?,
                        });
                    }
                }
                ProjectionDeltaEntity::Drive => delta.drives.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::Position => delta.positions.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::Charge => delta.charges.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::ChargeSample => delta.charge_samples.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::State => delta.states.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::Update => delta.updates.push(
                    serde_json::from_str(&mutation.payload_json)
                        .map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                ProjectionDeltaEntity::Geofence | ProjectionDeltaEntity::Address => {
                    return Err(StoreError::SyncMutation(
                        "entity has no typed projection row".into(),
                    ));
                }
            }
        }
        if delta.is_empty() {
            return Err(StoreError::LineageCompactionUnavailable);
        }
        Ok(delta)
    }

    pub fn projection_delta_for_mutations(
        &self,
        claim: &SyncMutationClaim,
        binding: ProjectionBinding,
        sequence: SequenceRange,
        parent_digest: Sha256Digest,
    ) -> Result<ProjectionDelta, StoreError> {
        let mut final_mutations = HashMap::<(String, i64), SyncMutation>::new();
        for mutation in &claim.mutations {
            final_mutations.insert(
                (mutation.entity.clone(), mutation.entity_id),
                mutation.clone(),
            );
        }
        let mut ordered = final_mutations.into_values().collect::<Vec<_>>();
        ordered.sort_by_key(|mutation| {
            (
                mutation.revision,
                mutation.entity.clone(),
                mutation.entity_id,
            )
        });
        let has_car_upsert = ordered
            .iter()
            .any(|mutation| mutation.entity == "car" && mutation.operation == "upsert");
        let connection = self.open()?;
        let mut delta = ProjectionDelta {
            binding,
            sequence,
            parent_digest,
            cars: Vec::new(),
            car_settings: Vec::new(),
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
            tombstones: Vec::new(),
        };
        for mutation in ordered {
            let entity = parse_sync_entity(&mutation.entity).ok_or_else(|| {
                StoreError::SyncMutation(format!("unknown entity {}", mutation.entity))
            })?;
            if mutation.operation == "tombstone" {
                delta.tombstones.push(ProjectionTombstone {
                    entity,
                    id: mutation.entity_id,
                    car_id: mutation.car_id,
                });
                continue;
            }
            if mutation.operation != "upsert" {
                return Err(StoreError::SyncMutation(
                    "invalid mutation operation".into(),
                ));
            }
            match entity {
                ProjectionDeltaEntity::Car => {
                    delta.cars.push(load_projection_json(
                        &connection,
                        "materialised_cars",
                        "car_json",
                        "car_id",
                        &mutation,
                    )?);
                }
                ProjectionDeltaEntity::CarSetting => {
                    if has_car_upsert {
                        continue;
                    }
                    let car: ProjectionCar = load_projection_json(
                        &connection,
                        "materialised_cars",
                        "car_json",
                        "car_id",
                        &mutation,
                    )?;
                    delta.car_settings.push(ProjectionCarSettingsPatch {
                        car_id: mutation.entity_id,
                        settings: car.settings,
                    });
                }
                ProjectionDeltaEntity::Drive => delta.drives.push(load_projection_json(
                    &connection,
                    "materialised_drives",
                    "drive_json",
                    "drive_id",
                    &mutation,
                )?),
                ProjectionDeltaEntity::Position => delta.positions.push(load_projection_json(
                    &connection,
                    "materialised_positions",
                    "position_json",
                    "position_id",
                    &mutation,
                )?),
                ProjectionDeltaEntity::Charge => delta.charges.push(load_projection_json(
                    &connection,
                    "materialised_charges",
                    "charge_json",
                    "charge_id",
                    &mutation,
                )?),
                ProjectionDeltaEntity::ChargeSample => {
                    delta.charge_samples.push(load_projection_json(
                        &connection,
                        "materialised_charge_samples",
                        "sample_json",
                        "sample_id",
                        &mutation,
                    )?);
                }
                ProjectionDeltaEntity::State => delta.states.push(load_projection_json(
                    &connection,
                    "materialised_states",
                    "state_json",
                    "state_id",
                    &mutation,
                )?),
                ProjectionDeltaEntity::Update => delta.updates.push(load_projection_json(
                    &connection,
                    "materialised_updates",
                    "update_json",
                    "update_id",
                    &mutation,
                )?),
                ProjectionDeltaEntity::Geofence | ProjectionDeltaEntity::Address => {
                    return Err(StoreError::SyncMutation(
                        "entity has no typed projection row".into(),
                    ));
                }
            }
        }
        Ok(delta)
    }

    pub fn commit_v2_delta_claim(
        &self,
        claim: &SyncMutationClaim,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
    ) -> Result<(), StoreError> {
        self.commit_v2_delta_claim_with_limits(
            claim,
            delta,
            cursor_key,
            terminal_cursor,
            ProtocolLimits::default(),
        )
    }

    fn commit_v2_delta_claim_with_limits(
        &self,
        claim: &SyncMutationClaim,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        limits: ProtocolLimits,
    ) -> Result<(), StoreError> {
        if claim.vehicle_id.is_nil()
            || claim.mutations.is_empty()
            || claim.from_revision <= 0
            || claim.to_revision < claim.from_revision
            || claim.to_revision - claim.from_revision + 1
                != i64::try_from(claim.mutations.len()).map_err(|_| StoreError::SequenceTooLarge)?
            || claim
                .mutations
                .windows(2)
                .any(|window| window[1].revision != window[0].revision + 1)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        delta.pack.validate(limits).map_err(StoreError::Manifest)?;
        if delta.from_sequence >= delta.to_sequence
            || delta.pack_digest != delta.pack.sha256
            || delta.pack.schema != HUB_PROJECTION_SCHEMA_V2
            || delta.chain_digest
                != canonical_delta_chain_digest(delta.parent_chain_digest, delta.pack.sha256)
            || delta.pack.sequence
                != (SequenceRange {
                    from_exclusive: delta.from_sequence,
                    to_inclusive: delta.to_sequence,
                })
            || delta.to_sequence - delta.from_sequence
                != u64::try_from(claim.mutations.len()).map_err(|_| StoreError::SequenceTooLarge)?
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let binding = self.v2_projection_binding(claim.vehicle_id)?;
        self.verify_import_delta_pack(delta, &binding)?;
        let cursor_claims = terminal_cursor
            .verify(cursor_key)
            .map_err(StoreError::Manifest)?;
        if cursor_claims.protocol != crate::protocol::PROTOCOL_V1
            || cursor_claims.schema != HUB_PROJECTION_SCHEMA_V2
            || cursor_claims.installation_id != binding.installation_id
            || cursor_claims.account_id != binding.account_id
            || cursor_claims.vehicle_id != binding.vehicle_id
            || cursor_claims.generation != binding.generation
            || cursor_claims.sequence != delta.to_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        // Appending a locally verified pack needs the durable lineage shape and
        // pack metadata, not a fresh hash of every immutable historical pack.
        // Full digest verification remains mandatory at serving, backup and
        // operator-facing integrity gates.
        let mut candidate_lineage = self
            .lineage_manifest_for_vehicle_with_verification(
                claim.vehicle_id,
                LineagePackVerification::MetadataOnly,
            )?
            .ok_or(StoreError::LineageCatalogConflict)?;
        let idempotent_replay = candidate_lineage.head_sequence == delta.to_sequence
            && candidate_lineage.head_digest == delta.chain_digest
            && candidate_lineage.deltas.last() == Some(delta);
        if !idempotent_replay {
            if candidate_lineage.head_sequence != delta.from_sequence
                || candidate_lineage.head_digest != delta.parent_chain_digest
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            candidate_lineage.deltas.push(delta.clone());
            candidate_lineage.head_sequence = delta.to_sequence;
            candidate_lineage.head_digest = delta.chain_digest;
            candidate_lineage.terminal_cursor = terminal_cursor.clone();
            candidate_lineage
                .validate_with_limits(limits)
                .map_err(|error| match error {
                    crate::protocol::ProtocolError::LineageAggregateLimitExceeded => {
                        StoreError::LineageCapacityExhausted
                    }
                    other => StoreError::Manifest(other),
                })?;
        }
        let terminal_cursor_json =
            serde_json::to_string(terminal_cursor).map_err(StoreError::SerializeManifest)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let vehicle_key = claim.vehicle_id.to_string();
        let current: Option<(i64, String)> = transaction
            .query_row(
                "SELECT head_sequence, head_digest FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((head_sequence, head_digest)) = current else {
            return Err(StoreError::LineageCatalogConflict);
        };
        if delta.pack.snapshot_id
            != transaction
                .query_row(
                    "SELECT snapshot_id FROM sync_bases WHERE vehicle_id = ?1",
                    params![vehicle_key.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?
                .and_then(|snapshot| Uuid::parse_str(&snapshot).ok())
                .ok_or(StoreError::LineageCatalogConflict)?
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let existing_delta: Option<(String, String)> = transaction
            .query_row(
                "SELECT chain_digest, pack_digest FROM sync_deltas
                 WHERE vehicle_id = ?1 AND from_sequence = ?2 AND to_sequence = ?3",
                params![
                    vehicle_key.as_str(),
                    i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if head_sequence
            == i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?
            && head_digest == delta.chain_digest.to_string()
            && existing_delta.as_ref().is_some_and(|(chain, pack)| {
                chain == &delta.chain_digest.to_string() && pack == &delta.pack_digest.to_string()
            })
        {
            insert_live_delta_span_in_transaction(&transaction, claim, delta)?;
            transaction
                .execute(
                    "UPDATE sync_mutations SET published = 1, claimed_until_ms = 0
                     WHERE vehicle_id = ?1 AND revision BETWEEN ?2 AND ?3",
                    params![vehicle_key, claim.from_revision, claim.to_revision],
                )
                .map_err(StoreError::LineageCatalog)?;
            transaction.commit().map_err(StoreError::LineageCatalog)?;
            return Ok(());
        }
        if head_sequence
            != i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?
            || head_digest != delta.parent_chain_digest.to_string()
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        if let Some((chain_digest, pack_digest)) = existing_delta.as_ref()
            && (chain_digest != &delta.chain_digest.to_string()
                || pack_digest != &delta.pack_digest.to_string())
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
        let updated = transaction
            .execute(
                "INSERT INTO sync_deltas(
                    vehicle_id, from_sequence, to_sequence, parent_chain_digest,
                    chain_digest, pack_digest, pack_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(vehicle_id, from_sequence, to_sequence) DO NOTHING",
                params![
                    vehicle_key.as_str(),
                    i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.parent_chain_digest.to_string(),
                    delta.chain_digest.to_string(),
                    delta.pack_digest.to_string(),
                    pack_json,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if updated != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        insert_live_delta_span_in_transaction(&transaction, claim, delta)?;
        Self::register_lineage_pack_snapshot(
            &transaction,
            &vehicle_key,
            &delta.pack,
            delta.to_sequence,
            &pack_json,
        )?;
        let existing_pack: Option<(String, i64, String, i64, i64)> = transaction
            .query_row(
                "SELECT snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                 FROM sync_packs WHERE sha256 = ?1",
                params![delta.pack.sha256.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some((snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes)) =
            existing_pack
        {
            if snapshot_id != delta.pack.snapshot_id.to_string()
                || ordinal != i64::from(delta.pack.ordinal)
                || relative_path != delta.pack.relative_path
                || compressed_bytes
                    != i64::try_from(delta.pack.compressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?
                || uncompressed_bytes
                    != i64::try_from(delta.pack.uncompressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?
            {
                return Err(StoreError::LineageCatalogConflict);
            }
        } else {
            let occupied: Option<String> = transaction
                .query_row(
                    "SELECT sha256 FROM sync_packs
                     WHERE snapshot_id = ?1 AND ordinal = ?2",
                    params![
                        delta.pack.snapshot_id.to_string(),
                        i64::from(delta.pack.ordinal)
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if occupied.is_some() {
                return Err(StoreError::LineageCatalogConflict);
            }
            transaction
                .execute(
                    "INSERT INTO sync_packs(
                        sha256, snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        delta.pack.sha256.to_string(),
                        delta.pack.snapshot_id.to_string(),
                        i64::from(delta.pack.ordinal),
                        delta.pack.relative_path,
                        i64::try_from(delta.pack.compressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                        i64::try_from(delta.pack.uncompressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }
        let updated = transaction
            .execute(
                "UPDATE sync_heads SET head_sequence = ?1, head_digest = ?2,
                        terminal_cursor = ?3
                 WHERE vehicle_id = ?4 AND head_sequence = ?5
                   AND head_digest = ?6",
                params![
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.chain_digest.to_string(),
                    terminal_cursor_json,
                    vehicle_key.as_str(),
                    head_sequence,
                    head_digest,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if updated != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        transaction
            .execute(
                "UPDATE sync_mutations SET published = 1, claimed_until_ms = 0
                 WHERE vehicle_id = ?1 AND published = 0
                   AND revision BETWEEN ?2 AND ?3",
                params![vehicle_key, claim.from_revision, claim.to_revision],
            )
            .map_err(StoreError::LineageCatalog)?;
        transaction.commit().map_err(StoreError::LineageCatalog)
    }

    /// Atomically replace a contiguous collector-owned suffix with one
    /// journal-derived delta. The immutable base and any import-owned prefix
    /// remain byte-for-byte unchanged. The caller writes and verifies the new
    /// content-addressed pack first; a failed transaction therefore leaves at
    /// most an unreferenced object that normal repair can remove.
    pub fn commit_live_delta_compaction(
        &self,
        plan: &LiveDeltaCompactionPlan,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
    ) -> Result<(), StoreError> {
        self.commit_live_delta_compaction_at(
            plan,
            delta,
            cursor_key,
            terminal_cursor,
            retired_lineage_clock_ms()?,
        )
    }

    fn commit_live_delta_compaction_at(
        &self,
        plan: &LiveDeltaCompactionPlan,
        delta: &LineageDelta,
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        retired_at_ms: i64,
    ) -> Result<(), StoreError> {
        let expires_at_ms = retired_at_ms
            .checked_add(RETIRED_LINEAGE_PACK_RETENTION_MS)
            .ok_or(StoreError::RetiredLineageClockOverflow)?;
        let revision_span = plan
            .to_revision
            .checked_sub(plan.from_revision)
            .and_then(|count| count.checked_add(1))
            .and_then(|count| u64::try_from(count).ok())
            .ok_or(StoreError::LineageCatalogConflict)?;
        if retired_at_ms < 0
            || plan.vehicle_id.is_nil()
            || plan.replaced_spans.len() < 2
            || plan.anchor_sequence >= plan.head_sequence
            || plan.head_sequence - plan.anchor_sequence != revision_span
            || delta.from_sequence != plan.anchor_sequence
            || delta.to_sequence != plan.head_sequence
            || delta.parent_chain_digest != plan.anchor_digest
            || delta.pack.snapshot_id != plan.base_snapshot_id
            || delta.pack.ordinal != plan.first_ordinal
            || delta.pack_digest != delta.pack.sha256
            || delta.chain_digest
                != canonical_delta_chain_digest(delta.parent_chain_digest, delta.pack_digest)
            || delta.pack.sequence
                != (SequenceRange {
                    from_exclusive: plan.anchor_sequence,
                    to_inclusive: plan.head_sequence,
                })
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        delta
            .pack
            .validate(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        let binding = self.v2_projection_binding(plan.vehicle_id)?;
        self.verify_import_delta_pack(delta, &binding)?;
        let cursor_claims = terminal_cursor
            .verify(cursor_key)
            .map_err(StoreError::Manifest)?;
        if cursor_claims.protocol != crate::protocol::PROTOCOL_V1
            || cursor_claims.schema != HUB_PROJECTION_SCHEMA_V2
            || cursor_claims.installation_id != binding.installation_id
            || cursor_claims.account_id != binding.account_id
            || cursor_claims.vehicle_id != binding.vehicle_id
            || cursor_claims.generation != binding.generation
            || cursor_claims.sequence != plan.head_sequence
        {
            return Err(StoreError::LineageCatalogConflict);
        }

        let mut candidate = self
            .lineage_manifest_for_vehicle(plan.vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        if candidate.base.snapshot_id != plan.base_snapshot_id
            || candidate.head_sequence != plan.head_sequence
            || candidate.head_digest != plan.head_digest
            || candidate.deltas.len() < plan.replaced_spans.len()
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let prefix_len = candidate.deltas.len() - plan.replaced_spans.len();
        if candidate.deltas[prefix_len..]
            .iter()
            .zip(&plan.replaced_spans)
            .any(|(stored, planned)| stored != &planned.delta)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        candidate
            .validate_with_limits(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        let retired_head_digest = candidate.head_digest;
        let retired_manifest_json =
            serde_json::to_vec(&candidate).map_err(StoreError::SerializeManifest)?;
        candidate.deltas.truncate(prefix_len);
        candidate.deltas.push(delta.clone());
        candidate.head_digest = delta.chain_digest;
        candidate.terminal_cursor = terminal_cursor.clone();
        candidate
            .validate_with_limits(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;

        let terminal_cursor_json =
            serde_json::to_string(terminal_cursor).map_err(StoreError::SerializeManifest)?;
        let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
        let vehicle_key = plan.vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current: Option<(String, i64, String)> = transaction
            .query_row(
                "SELECT base_snapshot_id, head_sequence, head_digest
                 FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if current
            != Some((
                plan.base_snapshot_id.to_string(),
                i64::try_from(plan.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                plan.head_digest.to_string(),
            ))
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        for span in &plan.replaced_spans {
            let stored: Option<(i64, i64, String, String)> = transaction
                .query_row(
                    "SELECT spans.from_revision, spans.to_revision,
                            spans.pack_digest, deltas.chain_digest
                     FROM sync_live_delta_spans AS spans
                     JOIN sync_deltas AS deltas
                       ON deltas.vehicle_id = spans.vehicle_id
                      AND deltas.from_sequence = spans.from_sequence
                      AND deltas.to_sequence = spans.to_sequence
                     WHERE spans.vehicle_id = ?1
                       AND spans.from_sequence = ?2 AND spans.to_sequence = ?3",
                    params![
                        vehicle_key.as_str(),
                        i64::try_from(span.delta.from_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        i64::try_from(span.delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if stored
                != Some((
                    span.from_revision,
                    span.to_revision,
                    span.delta.pack_digest.to_string(),
                    span.delta.chain_digest.to_string(),
                ))
            {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        let retired_cleanup_cutoff =
            retired_at_ms.saturating_sub(RETIRED_LINEAGE_PACK_DELETE_GRACE_MS);
        transaction
            .execute(
                "DELETE FROM sync_retired_lineages WHERE expires_at_ms <= ?1",
                params![retired_cleanup_cutoff],
            )
            .map_err(StoreError::LineageCatalog)?;
        transaction
            .execute(
                "INSERT INTO sync_retired_lineages(
                    vehicle_id, head_digest, manifest_json,
                    retired_at_ms, expires_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    vehicle_key.as_str(),
                    retired_head_digest.to_string(),
                    retired_manifest_json,
                    retired_at_ms,
                    expires_at_ms,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        for span in &plan.replaced_spans {
            transaction
                .execute(
                    "INSERT INTO sync_retired_lineage_packs(
                        vehicle_id, head_digest, pack_digest,
                        relative_path, compressed_bytes
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        vehicle_key.as_str(),
                        retired_head_digest.to_string(),
                        span.delta.pack_digest.to_string(),
                        span.delta.pack.relative_path,
                        i64::try_from(span.delta.pack.compressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }
        for span in &plan.replaced_spans {
            let deleted_span = transaction
                .execute(
                    "DELETE FROM sync_live_delta_spans
                     WHERE vehicle_id = ?1 AND from_sequence = ?2 AND to_sequence = ?3
                       AND from_revision = ?4 AND to_revision = ?5 AND pack_digest = ?6",
                    params![
                        vehicle_key.as_str(),
                        i64::try_from(span.delta.from_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        i64::try_from(span.delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        span.from_revision,
                        span.to_revision,
                        span.delta.pack_digest.to_string(),
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
            if deleted_span != 1 {
                return Err(StoreError::LineageCatalogConflict);
            }
            let deleted = transaction
                .execute(
                    "DELETE FROM sync_deltas
                     WHERE vehicle_id = ?1 AND from_sequence = ?2 AND to_sequence = ?3
                       AND chain_digest = ?4 AND pack_digest = ?5",
                    params![
                        vehicle_key.as_str(),
                        i64::try_from(span.delta.from_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        i64::try_from(span.delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        span.delta.chain_digest.to_string(),
                        span.delta.pack_digest.to_string(),
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
            if deleted != 1 {
                return Err(StoreError::LineageCatalogConflict);
            }
            let deleted_pack = transaction
                .execute(
                    "DELETE FROM sync_packs WHERE sha256 = ?1 AND snapshot_id = ?2",
                    params![
                        span.delta.pack_digest.to_string(),
                        plan.base_snapshot_id.to_string(),
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
            if deleted_pack != 1 {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        let inserted = transaction
            .execute(
                "INSERT INTO sync_deltas(
                    vehicle_id, from_sequence, to_sequence, parent_chain_digest,
                    chain_digest, pack_digest, pack_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    vehicle_key.as_str(),
                    i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.parent_chain_digest.to_string(),
                    delta.chain_digest.to_string(),
                    delta.pack_digest.to_string(),
                    pack_json,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if inserted != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        Self::register_lineage_pack_snapshot(
            &transaction,
            &vehicle_key,
            &delta.pack,
            delta.to_sequence,
            &pack_json,
        )?;
        transaction
            .execute(
                "INSERT INTO sync_packs(
                    sha256, snapshot_id, ordinal, relative_path,
                    compressed_bytes, uncompressed_bytes
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    delta.pack.sha256.to_string(),
                    delta.pack.snapshot_id.to_string(),
                    i64::from(delta.pack.ordinal),
                    delta.pack.relative_path,
                    i64::try_from(delta.pack.compressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                    i64::try_from(delta.pack.uncompressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        let compacted_claim = SyncMutationClaim {
            vehicle_id: plan.vehicle_id,
            from_revision: plan.from_revision,
            to_revision: plan.to_revision,
            mutations: plan.mutations.clone(),
        };
        insert_live_delta_span_in_transaction(&transaction, &compacted_claim, delta)?;
        let updated = transaction
            .execute(
                "UPDATE sync_heads SET head_digest = ?1, terminal_cursor = ?2
                 WHERE vehicle_id = ?3 AND base_snapshot_id = ?4
                   AND head_sequence = ?5 AND head_digest = ?6",
                params![
                    delta.chain_digest.to_string(),
                    terminal_cursor_json,
                    vehicle_key,
                    plan.base_snapshot_id.to_string(),
                    i64::try_from(plan.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    plan.head_digest.to_string(),
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        if updated != 1 {
            return Err(StoreError::LineageCatalogConflict);
        }
        transaction.commit().map_err(StoreError::LineageCatalog)
    }
}
