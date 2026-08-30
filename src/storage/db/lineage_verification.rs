// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    /// Commit a validated lineage only after every referenced immutable pack
    /// is present, size-correct, and hash-correct. The DB transaction never
    /// becomes visible before that verification completes.
    pub fn commit_lineage_catalog(&self, lineage: &LineageManifestV2) -> Result<(), StoreError> {
        // The generic lineage API does not carry ProjectionBinding.  It is
        // retained for schema-1 lineage scenarios only; schema-2.1 bases must
        // use a binding-aware finalizer so the persisted base cannot later be
        // retargeted from mutable local state.
        if lineage.schema == HUB_PROJECTION_SCHEMA_V2 {
            return Err(StoreError::ImmutableBaseBindingMissing(lineage.vehicle_id));
        }
        lineage.validate().map_err(StoreError::Manifest)?;
        let mut packs = lineage.base.packs.clone();
        packs.extend(lineage.deltas.iter().map(|delta| delta.pack.clone()));
        for pack in &packs {
            self.verify_lineage_pack(pack)?;
        }

        let vehicle_id = lineage.vehicle_id.to_string();
        let base_json =
            serde_json::to_vec(&lineage.base.packs).map_err(StoreError::SerializeManifest)?;
        let cursor = serde_json::to_string(&lineage.terminal_cursor)
            .map_err(StoreError::SerializeManifest)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;

        let existing_base: Option<(String, i64, String, Vec<u8>)> = transaction
            .query_row(
                "SELECT snapshot_id, base_sequence, base_digest, packs_json
                 FROM sync_bases WHERE vehicle_id = ?1",
                params![vehicle_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some((snapshot_id, sequence, digest, stored_packs)) = existing_base {
            if snapshot_id != lineage.base.snapshot_id.to_string()
                || u64::try_from(sequence).ok() != Some(lineage.base.sequence)
                || digest != lineage.base.digest.to_string()
                || stored_packs != base_json
            {
                return Err(StoreError::LineageCatalogConflict);
            }
        } else {
            transaction
                .execute(
                    "INSERT INTO sync_bases
                     (vehicle_id, snapshot_id, base_sequence, base_digest, packs_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        vehicle_id.as_str(),
                        lineage.base.snapshot_id.to_string(),
                        i64::try_from(lineage.base.sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        lineage.base.digest.to_string(),
                        base_json,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }

        for delta in &lineage.deltas {
            let existing: Option<(String, String)> = transaction
                .query_row(
                    "SELECT chain_digest, pack_digest FROM sync_deltas
                     WHERE vehicle_id = ?1 AND from_sequence = ?2 AND to_sequence = ?3",
                    params![
                        vehicle_id.as_str(),
                        i64::try_from(delta.from_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        i64::try_from(delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if let Some((chain_digest, pack_digest)) = existing {
                if chain_digest != delta.chain_digest.to_string()
                    || pack_digest != delta.pack_digest.to_string()
                {
                    return Err(StoreError::LineageCatalogConflict);
                }
                continue;
            }
            let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
            transaction
                .execute(
                    "INSERT INTO sync_deltas
                     (vehicle_id, from_sequence, to_sequence, parent_chain_digest,
                      chain_digest, pack_digest, pack_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        vehicle_id.as_str(),
                        i64::try_from(delta.from_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        i64::try_from(delta.to_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        delta.parent_chain_digest.to_string(),
                        delta.chain_digest.to_string(),
                        delta.pack_digest.to_string(),
                        pack_json,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }

        for pack in &packs {
            Self::register_lineage_pack_snapshot(
                &transaction,
                &vehicle_id,
                pack,
                lineage
                    .deltas
                    .iter()
                    .find(|delta| delta.pack.sha256 == pack.sha256)
                    .map_or(lineage.base.sequence, |delta| delta.to_sequence),
                &serde_json::to_vec(lineage).map_err(StoreError::SerializeManifest)?,
            )?;
            let existing_pack: Option<(String, i64, String, i64, i64)> = transaction
                .query_row(
                    "SELECT snapshot_id, ordinal, relative_path,
                            compressed_bytes, uncompressed_bytes
                     FROM sync_packs WHERE sha256 = ?1",
                    params![pack.sha256.to_string()],
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
            if let Some((
                snapshot_id,
                ordinal,
                relative_path,
                compressed_bytes,
                uncompressed_bytes,
            )) = existing_pack
            {
                if snapshot_id != pack.snapshot_id.to_string()
                    || ordinal != i64::from(pack.ordinal)
                    || relative_path != pack.relative_path
                    || compressed_bytes
                        != i64::try_from(pack.compressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?
                    || uncompressed_bytes
                        != i64::try_from(pack.uncompressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?
                {
                    return Err(StoreError::LineageCatalogConflict);
                }
                continue;
            }
            let occupied: Option<String> = transaction
                .query_row(
                    "SELECT sha256 FROM sync_packs
                     WHERE snapshot_id = ?1 AND ordinal = ?2",
                    params![pack.snapshot_id.to_string(), i64::from(pack.ordinal)],
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
                        pack.sha256.to_string(),
                        pack.snapshot_id.to_string(),
                        i64::from(pack.ordinal),
                        pack.relative_path,
                        i64::try_from(pack.compressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                        i64::try_from(pack.uncompressed_bytes)
                            .map_err(|_| StoreError::PackSizeTooLarge)?,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }

        let existing_head: Option<(i64, String)> = transaction
            .query_row(
                "SELECT head_sequence, head_digest FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some((sequence, digest)) = existing_head {
            let sequence =
                u64::try_from(sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
            if sequence > lineage.head_sequence
                || (sequence == lineage.head_sequence && digest != lineage.head_digest.to_string())
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            if sequence < lineage.head_sequence {
                transaction
                    .execute(
                        "UPDATE sync_heads
                         SET head_sequence = ?1, head_digest = ?2, terminal_cursor = ?3
                         WHERE vehicle_id = ?4 AND head_sequence = ?5 AND head_digest = ?6",
                        params![
                            i64::try_from(lineage.head_sequence)
                                .map_err(|_| StoreError::SequenceTooLarge)?,
                            lineage.head_digest.to_string(),
                            cursor,
                            vehicle_id.as_str(),
                            i64::try_from(sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                            digest,
                        ],
                    )
                    .map_err(StoreError::LineageCatalog)?;
            }
        } else {
            transaction
                .execute(
                    "INSERT INTO sync_heads
                     (vehicle_id, base_snapshot_id, head_sequence, head_digest, terminal_cursor)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        vehicle_id.as_str(),
                        lineage.base.snapshot_id.to_string(),
                        i64::try_from(lineage.head_sequence)
                            .map_err(|_| StoreError::SequenceTooLarge)?,
                        lineage.head_digest.to_string(),
                        cursor,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
        }
        transaction.commit().map_err(StoreError::LineageCatalog)
    }

    fn register_lineage_pack_snapshot(
        transaction: &Transaction<'_>,
        vehicle_id: &str,
        pack: &TransportPack,
        head_sequence: u64,
        manifest_json: &[u8],
    ) -> Result<(), StoreError> {
        let snapshot_id = pack.snapshot_id.to_string();
        let existing: Option<String> = transaction
            .query_row(
                "SELECT vehicle_id FROM sync_manifests WHERE snapshot_id = ?1",
                params![snapshot_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        if let Some(existing_vehicle_id) = existing {
            if existing_vehicle_id != vehicle_id {
                return Err(StoreError::LineageCatalogConflict);
            }
            return Ok(());
        }
        transaction
            .execute(
                "INSERT INTO sync_manifests
                 (snapshot_id, vehicle_id, head_sequence, manifest_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    snapshot_id,
                    vehicle_id,
                    i64::try_from(head_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    manifest_json,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        Ok(())
    }

    fn inspect_legacy_v2_base_car_pack(
        &self,
        pack: &TransportPack,
        base: &LegacyV2BaseDescription,
    ) -> Result<i64, StoreError> {
        if pack.ordinal != 0
            || pack.snapshot_id != base.snapshot_id
            || pack.schema != HUB_PROJECTION_SCHEMA_V2
            || pack.format != crate::protocol::PackFormat::HubProjectionSqlite
            || pack.sha256 != base.base_digest
            || pack.sequence
                != (SequenceRange {
                    from_exclusive: base.base_sequence,
                    to_inclusive: base.base_sequence,
                })
            || !pack.tables.contains(&crate::protocol::MirrorTable::Car)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        pack.validate(ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;

        let path = self
            .packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        let mut file = File::open(&path).map_err(StoreError::OpenLineagePack)?;
        let metadata = file.metadata().map_err(StoreError::OpenLineagePack)?;
        if !metadata.is_file() || metadata.len() != pack.compressed_bytes {
            return Err(StoreError::LineagePackNotReady);
        }
        pack.verify_reader(&mut file, ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        file.seek(SeekFrom::Start(0))
            .map_err(StoreError::OpenLineagePack)?;
        let decoder =
            zstd::stream::read::Decoder::new(file).map_err(StoreError::DecodeLineagePack)?;
        let maximum = pack
            .uncompressed_bytes
            .checked_add(1)
            .ok_or(StoreError::LineageCatalogConflict)?;
        let inspection = LineagePackInspection {
            path: self.packs_dir.join(format!(
                ".legacy-binding-inspection-{}.sqlite",
                Uuid::new_v4()
            )),
        };
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&inspection.path)
            .map_err(|source| StoreError::CreateLineagePackInspection {
                path: inspection.path.clone(),
                source,
            })?;
        let decoded = std::io::copy(&mut decoder.take(maximum), &mut output)
            .map_err(StoreError::DecodeLineagePack)?;
        if decoded != pack.uncompressed_bytes {
            return Err(StoreError::LineageCatalogConflict);
        }
        output
            .sync_all()
            .map_err(StoreError::SyncLineagePackInspection)?;
        drop(output);

        let connection = Connection::open_with_flags(
            &inspection.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::LineageCatalog)?;
        connection
            .execute_batch("PRAGMA trusted_schema = OFF;")
            .map_err(StoreError::LineageCatalog)?;
        let application_id: i64 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?;
        let user_version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?;
        let quick_check: Vec<String> = connection
            .prepare("PRAGMA quick_check")
            .map_err(StoreError::LineageCatalog)?
            .query_map([], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?
            .collect::<Result<_, _>>()
            .map_err(StoreError::LineageCatalog)?;
        if application_id != i64::from(SQLITE_HUB_PROJECTION_APPLICATION_ID)
            || user_version != i64::from(HUB_PROJECTION_SCHEMA_V2.sqlite_user_version())
            || quick_check.as_slice() != ["ok"]
        {
            return Err(StoreError::LineageCatalogConflict);
        }

        let pack_metadata = {
            let mut statement = connection
                .prepare("SELECT key, value FROM hub_pack_metadata")
                .map_err(StoreError::LineageCatalog)?;
            let mut rows = statement.query([]).map_err(StoreError::LineageCatalog)?;
            let mut values = HashMap::new();
            while let Some(row) = rows.next().map_err(StoreError::LineageCatalog)? {
                let key: String = row.get(0).map_err(StoreError::LineageCatalog)?;
                let value: String = row.get(1).map_err(StoreError::LineageCatalog)?;
                if values.insert(key, value).is_some() {
                    return Err(StoreError::LineageCatalogConflict);
                }
            }
            values
        };
        if pack_metadata.len() != FULL_SNAPSHOT_METADATA_KEYS.len()
            || FULL_SNAPSHOT_METADATA_KEYS
                .iter()
                .any(|key| !pack_metadata.contains_key(*key))
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let selected_car_id = pack_metadata
            .get("selected_car_id")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .ok_or(StoreError::LineageCatalogConflict)?;
        let expected = [
            ("protocol", "teslatlas-sync".to_owned()),
            ("pack_format", "hub_projection_sqlite".to_owned()),
            ("schema_major", HUB_PROJECTION_SCHEMA_V2.major.to_string()),
            ("schema_minor", HUB_PROJECTION_SCHEMA_V2.minor.to_string()),
            ("pack_id", pack.pack_id.to_string()),
            ("snapshot_id", base.snapshot_id.to_string()),
            ("ordinal", pack.ordinal.to_string()),
            ("mode", "full_snapshot".to_owned()),
            ("installation_id", base.installation_id.to_string()),
            ("account_id", base.account_id.to_string()),
            ("vehicle_id", base.vehicle_id.to_string()),
            ("generation", base.generation.to_string()),
            ("selected_car_id", selected_car_id.to_string()),
            ("base_sequence", base.base_sequence.to_string()),
            ("head_sequence", base.base_sequence.to_string()),
            ("row_count", pack.row_count.to_string()),
        ];
        if expected
            .iter()
            .any(|(key, expected)| pack_metadata.get(*key) != Some(expected))
        {
            return Err(StoreError::LineageCatalogConflict);
        }

        let car_ids = connection
            .prepare("SELECT id FROM cars ORDER BY id")
            .map_err(StoreError::LineageCatalog)?
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(StoreError::LineageCatalog)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::LineageCatalog)?;
        if car_ids.as_slice() != [selected_car_id] {
            return Err(StoreError::LineageCatalogConflict);
        }
        for (table, column) in [
            ("car_settings", "car_id"),
            ("drives", "car_id"),
            ("charges", "car_id"),
            ("positions", "car_id"),
            ("states", "car_id"),
            ("updates", "car_id"),
        ] {
            let out_of_scope: bool = connection
                .query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {column} != ?1)"),
                    params![selected_car_id],
                    |row| row.get(0),
                )
                .map_err(StoreError::LineageCatalog)?;
            if out_of_scope {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        Ok(selected_car_id)
    }

    fn verify_lineage_pack_for_mode(
        &self,
        pack: &TransportPack,
        verification: LineagePackVerification,
    ) -> Result<(), StoreError> {
        self.verify_lineage_pack_metadata(pack)?;
        if verification == LineagePackVerification::MetadataOnly {
            return Ok(());
        }
        let path = self
            .packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        if sha256_file_hex(&path)? != pack.sha256.to_string() {
            return Err(StoreError::LineagePackDigestMismatch);
        }
        Ok(())
    }

    fn verify_lineage_pack(&self, pack: &TransportPack) -> Result<(), StoreError> {
        self.verify_lineage_pack_for_mode(pack, LineagePackVerification::FullDigest)
    }

    fn verify_lineage_pack_metadata(&self, pack: &TransportPack) -> Result<(), StoreError> {
        if pack.relative_path != TransportPack::canonical_relative_path(pack.sha256) {
            return Err(StoreError::LineagePackNotReady);
        }
        let path = self
            .packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        let metadata = fs::symlink_metadata(&path).map_err(|_| StoreError::LineagePackNotReady)?;
        if !metadata.file_type().is_file() || metadata.len() != pack.compressed_bytes {
            return Err(StoreError::LineagePackNotReady);
        }
        Ok(())
    }

    fn verify_typed_delta_schema(connection: &Connection) -> Result<(), StoreError> {
        let objects = connection
            .prepare(
                "SELECT type, name, tbl_name, sql FROM sqlite_schema
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            )
            .map_err(StoreError::LineageCatalog)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(StoreError::LineageCatalog)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::LineageCatalog)?;
        if objects.len() != TYPED_DELTA_TABLES.len()
            || objects.iter().any(|(kind, name, table, sql)| {
                kind != "table"
                    || table != name
                    || !TYPED_DELTA_TABLES.contains(&name.as_str())
                    || sql.is_none()
            })
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let mut contract = Sha256::new();
        for (kind, name, table, sql) in &objects {
            for part in [kind.as_bytes(), name.as_bytes(), table.as_bytes()] {
                contract.update(part);
                contract.update([0]);
            }
            contract.update(sql.as_deref().expect("checked schema SQL").as_bytes());
            contract.update([0, b'\n']);
        }
        let contract = hex::encode(contract.finalize());
        if contract != TYPED_DELTA_SCHEMA_CONTRACT_SHA256 {
            return Err(StoreError::LineageCatalogConflict);
        }

        let unexpected_internal: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_schema
                     WHERE name LIKE 'sqlite_%'
                       AND name NOT IN (
                           'sqlite_stat1', 'sqlite_stat4',
                           'sqlite_autoindex_hub_pack_metadata_1'
                       )
                )",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)?;
        if unexpected_internal {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(())
    }

    fn verify_typed_delta_real_values(
        connection: &Connection,
        table: &str,
        column: &str,
        nonnegative: bool,
    ) -> Result<(), StoreError> {
        let mut statement = connection
            .prepare(&format!(
                "SELECT {column} FROM {table} WHERE {column} IS NOT NULL"
            ))
            .map_err(StoreError::LineageCatalog)?;
        let values = statement
            .query_map([], |row| row.get::<_, f64>(0))
            .map_err(StoreError::LineageCatalog)?;
        for value in values {
            let value = value.map_err(StoreError::LineageCatalog)?;
            if !value.is_finite() || (nonnegative && value < 0.0) {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        Ok(())
    }

    fn verify_typed_delta_soc_values(
        connection: &Connection,
        table: &str,
        column: &str,
    ) -> Result<(), StoreError> {
        let mut statement = connection
            .prepare(&format!(
                "SELECT {column} FROM {table} WHERE {column} IS NOT NULL"
            ))
            .map_err(StoreError::LineageCatalog)?;
        let values = statement
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(StoreError::LineageCatalog)?;
        for value in values {
            if !(0..=100).contains(&value.map_err(StoreError::LineageCatalog)?) {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        Ok(())
    }

    fn verify_typed_delta_text_values(
        connection: &Connection,
        table: &str,
        column: &str,
        required: bool,
    ) -> Result<(), StoreError> {
        let mut statement = connection
            .prepare(&format!("SELECT {column} FROM {table}"))
            .map_err(StoreError::LineageCatalog)?;
        let values = statement
            .query_map([], |row| row.get::<_, Option<String>>(0))
            .map_err(StoreError::LineageCatalog)?;
        for value in values {
            let value = value.map_err(StoreError::LineageCatalog)?;
            let Some(value) = value else {
                if required {
                    return Err(StoreError::LineageCatalogConflict);
                }
                continue;
            };
            if value.len() > MAX_TEXT_BYTES
                || value.as_bytes().contains(&0)
                || (required && value.is_empty())
            {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        Ok(())
    }

    fn verify_typed_delta_coordinate_pairs(
        connection: &Connection,
        table: &str,
        latitude_column: &str,
        longitude_column: &str,
    ) -> Result<(), StoreError> {
        let mut statement = connection
            .prepare(&format!(
                "SELECT {latitude_column}, {longitude_column} FROM {table}"
            ))
            .map_err(StoreError::LineageCatalog)?;
        let coordinates = statement
            .query_map([], |row| {
                Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, Option<f64>>(1)?))
            })
            .map_err(StoreError::LineageCatalog)?;
        for coordinate in coordinates {
            match coordinate.map_err(StoreError::LineageCatalog)? {
                (None, None) => {}
                (Some(latitude), Some(longitude))
                    if latitude.is_finite()
                        && longitude.is_finite()
                        && (-90.0..=90.0).contains(&latitude)
                        && (-180.0..=180.0).contains(&longitude)
                        && (latitude != 0.0 || longitude != 0.0) => {}
                _ => return Err(StoreError::LineageCatalogConflict),
            }
        }
        Ok(())
    }

    fn verify_typed_delta_row_semantics(
        connection: &Connection,
        selected_car_id: i64,
    ) -> Result<(), StoreError> {
        let malformed: bool = connection
            .query_row(
                "SELECT
                    EXISTS(SELECT 1 FROM cars
                           WHERE id <= 0 OR length(name) = 0 OR length(model) = 0)
                    OR EXISTS(SELECT 1 FROM cars AS car
                                LEFT JOIN car_settings AS settings
                                  ON settings.car_id = car.id
                                WHERE settings.car_id IS NULL)
                    OR EXISTS(SELECT 1 FROM car_settings
                                WHERE car_id <= 0
                                   OR enabled NOT IN (0, 1)
                                   OR use_streaming_api NOT IN (0, 1)
                                   OR suspend_after_idle_min <= 0
                                   OR suspend_min <= 0
                                   OR suspend_min_resolved NOT IN (0, 1)
                                   OR req_not_unlocked NOT IN (0, 1)
                                   OR free_supercharging NOT IN (0, 1)
                                   OR lfp_battery NOT IN (0, 1))
                    OR EXISTS(SELECT 1 FROM drives
                                WHERE id <= 0 OR start_date_ms <= 0
                                   OR end_date_ms < start_date_ms
                                   OR distance_km < 0)
                    OR EXISTS(SELECT 1 FROM charges
                                WHERE id <= 0 OR start_date_ms <= 0
                                   OR end_date_ms < start_date_ms
                                   OR charge_energy_added < 0)
                    OR EXISTS(SELECT 1 FROM positions
                                WHERE id <= 0 OR date_ms <= 0
                                   OR drive_id <= 0
                                   OR latitude NOT BETWEEN -90.0 AND 90.0
                                   OR longitude NOT BETWEEN -180.0 AND 180.0
                                   OR (latitude = 0.0 AND longitude = 0.0)
                                   OR odometer < 0)
                    OR EXISTS(SELECT 1 FROM charge_samples
                                WHERE id <= 0 OR charge_process_id <= 0
                                   OR timestamp_ms <= 0)
                    OR EXISTS(SELECT 1 FROM states
                                WHERE id <= 0 OR start_date_ms <= 0
                                   OR end_date_ms < start_date_ms
                                   OR state NOT IN ('online', 'offline', 'asleep'))
                    OR EXISTS(SELECT 1 FROM updates
                                WHERE id <= 0 OR start_date_ms <= 0
                                   OR end_date_ms < start_date_ms OR length(version) = 0)
                    OR EXISTS(SELECT 1 FROM tombstones
                                WHERE entity NOT IN (
                                    'drive', 'position', 'charge', 'charge_sample',
                                    'state', 'update'
                                )
                                   OR entity_id <= 0 OR car_id <= 0)",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)?;
        if malformed {
            return Err(StoreError::LineageCatalogConflict);
        }
        let conflicting_tombstone: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM tombstones AS tombstone
                     WHERE (tombstone.entity = 'car'
                            AND tombstone.entity_id IN (SELECT id FROM cars))
                        OR (tombstone.entity = 'car_setting'
                            AND tombstone.entity_id IN (SELECT car_id FROM car_settings))
                        OR (tombstone.entity = 'drive'
                            AND tombstone.entity_id IN (SELECT id FROM drives))
                        OR (tombstone.entity = 'position'
                            AND tombstone.entity_id IN (SELECT id FROM positions))
                        OR (tombstone.entity = 'charge'
                            AND tombstone.entity_id IN (SELECT id FROM charges))
                        OR (tombstone.entity = 'charge_sample'
                            AND tombstone.entity_id IN (SELECT id FROM charge_samples))
                        OR (tombstone.entity = 'state'
                            AND tombstone.entity_id IN (SELECT id FROM states))
                        OR (tombstone.entity = 'update'
                            AND tombstone.entity_id IN (SELECT id FROM updates))
                )",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)?;
        if conflicting_tombstone {
            return Err(StoreError::LineageCatalogConflict);
        }
        for (table, column, nonnegative) in [
            ("cars", "efficiency_wh_per_km", true),
            ("drives", "distance_km", true),
            ("drives", "efficiency", false),
            ("drives", "power_max", false),
            ("drives", "power_min", false),
            ("charges", "charge_energy_added", true),
            ("charges", "cost", false),
            ("positions", "power", false),
            ("positions", "odometer", true),
            ("positions", "ideal_battery_range_km", true),
            ("charge_samples", "charge_energy_added_kwh", true),
        ] {
            Self::verify_typed_delta_real_values(connection, table, column, nonnegative)?;
        }
        for (table, latitude, longitude) in [
            ("drives", "start_latitude", "start_longitude"),
            ("drives", "end_latitude", "end_longitude"),
            ("charges", "start_latitude", "start_longitude"),
            ("positions", "latitude", "longitude"),
        ] {
            Self::verify_typed_delta_coordinate_pairs(connection, table, latitude, longitude)?;
        }
        for (table, column) in [
            ("drives", "start_soc"),
            ("drives", "end_soc"),
            ("charges", "start_battery_level"),
            ("charges", "end_battery_level"),
            ("positions", "battery_level"),
            ("positions", "usable_battery_level"),
            ("charge_samples", "battery_level"),
            ("charge_samples", "usable_battery_level"),
        ] {
            Self::verify_typed_delta_soc_values(connection, table, column)?;
        }
        for (table, column, required) in [
            ("cars", "name", true),
            ("cars", "model", true),
            ("cars", "vin", false),
            ("cars", "firmware_version", false),
            ("drives", "start_address", false),
            ("drives", "end_address", false),
            ("drives", "start_geofence", false),
            ("drives", "end_geofence", false),
            ("charges", "address", false),
            ("charges", "location_name", false),
            ("charges", "geofence", false),
            ("updates", "version", true),
        ] {
            Self::verify_typed_delta_text_values(connection, table, column, required)?;
        }
        let multiple_open_states: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT car_id FROM states
                     WHERE end_date_ms IS NULL
                     GROUP BY car_id
                    HAVING COUNT(*) > 1
                )",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)?;
        if multiple_open_states {
            return Err(StoreError::LineageCatalogConflict);
        }
        for (table, column) in [
            ("cars", "id"),
            ("car_settings", "car_id"),
            ("drives", "car_id"),
            ("charges", "car_id"),
            ("positions", "car_id"),
            ("states", "car_id"),
            ("updates", "car_id"),
            ("tombstones", "car_id"),
        ] {
            let out_of_scope: bool = connection
                .query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {column} != ?1)"),
                    params![selected_car_id],
                    |row| row.get(0),
                )
                .map_err(StoreError::LineageCatalog)?;
            if out_of_scope {
                return Err(StoreError::LineageCatalogConflict);
            }
        }
        Ok(())
    }

    /// Confirm that a changed-history successor is the sparse, externally
    /// based delta produced by [`ProjectionPackWriter::write_delta`]. The
    /// transport manifest is caller input, so its fields cannot stand in for
    /// the immutable SQLite object's own metadata.
    fn verify_import_delta_pack(
        &self,
        delta: &LineageDelta,
        binding: &ProjectionBinding,
    ) -> Result<(), StoreError> {
        let path = self
            .packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", delta.pack.sha256));
        // Verify and decode the exact same opened descriptor. Reopening the
        // content-addressed path after verification would leave a same-user
        // replacement window between the digest check and SQLite inspection.
        let mut file = File::open(&path).map_err(StoreError::OpenLineagePack)?;
        delta
            .pack
            .verify_reader(&mut file, ProtocolLimits::default())
            .map_err(StoreError::Manifest)?;
        file.seek(SeekFrom::Start(0))
            .map_err(StoreError::OpenLineagePack)?;
        let decoder =
            zstd::stream::read::Decoder::new(file).map_err(StoreError::DecodeLineagePack)?;
        let maximum = delta
            .pack
            .uncompressed_bytes
            .checked_add(1)
            .ok_or(StoreError::LineageCatalogConflict)?;
        let inspection = LineagePackInspection {
            path: self
                .packs_dir
                .join(format!(".lineage-inspection-{}.sqlite", Uuid::new_v4())),
        };
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&inspection.path)
            .map_err(|source| StoreError::CreateLineagePackInspection {
                path: inspection.path.clone(),
                source,
            })?;
        let decoded = std::io::copy(&mut decoder.take(maximum), &mut output)
            .map_err(StoreError::DecodeLineagePack)?;
        if decoded != delta.pack.uncompressed_bytes {
            return Err(StoreError::LineageCatalogConflict);
        }
        output
            .sync_all()
            .map_err(StoreError::SyncLineagePackInspection)?;
        drop(output);

        let connection = Connection::open_with_flags(
            &inspection.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::LineageCatalog)?;
        connection
            .execute_batch("PRAGMA trusted_schema = OFF;")
            .map_err(StoreError::LineageCatalog)?;
        let application_id: i64 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?;
        let user_version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?;
        if application_id != i64::from(SQLITE_HUB_PROJECTION_APPLICATION_ID)
            || user_version != i64::from(HUB_PROJECTION_SCHEMA_V2.sqlite_user_version())
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let quick_check: Vec<String> = connection
            .prepare("PRAGMA quick_check")
            .map_err(StoreError::LineageCatalog)?
            .query_map([], |row| row.get(0))
            .map_err(StoreError::LineageCatalog)?
            .collect::<Result<_, _>>()
            .map_err(StoreError::LineageCatalog)?;
        if quick_check.as_slice() != ["ok"] {
            return Err(StoreError::LineageCatalogConflict);
        }
        Self::verify_typed_delta_schema(&connection)?;

        let metadata = {
            let mut statement = connection
                .prepare("SELECT key, value FROM hub_pack_metadata")
                .map_err(StoreError::LineageCatalog)?;
            let mut rows = statement.query([]).map_err(StoreError::LineageCatalog)?;
            let mut metadata = HashMap::new();
            while let Some(row) = rows.next().map_err(StoreError::LineageCatalog)? {
                let key: String = row.get(0).map_err(StoreError::LineageCatalog)?;
                let value: String = row.get(1).map_err(StoreError::LineageCatalog)?;
                if metadata.insert(key, value).is_some() {
                    return Err(StoreError::LineageCatalogConflict);
                }
            }
            metadata
        };
        if metadata.len() != TYPED_DELTA_METADATA_KEYS.len()
            || TYPED_DELTA_METADATA_KEYS
                .iter()
                .any(|key| !metadata.contains_key(*key))
            || metadata
                .iter()
                .any(|(key, value)| key.len() > 64 || value.len() > 16 * 1024)
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let expected_metadata = [
            ("protocol", "teslatlas-sync".to_owned()),
            ("pack_format", "hub_projection_sqlite".to_owned()),
            ("schema_major", HUB_PROJECTION_SCHEMA_V2.major.to_string()),
            ("schema_minor", HUB_PROJECTION_SCHEMA_V2.minor.to_string()),
            ("delta_schema_version", "1".to_owned()),
            ("pack_id", delta.pack.pack_id.to_string()),
            ("snapshot_id", delta.pack.snapshot_id.to_string()),
            ("ordinal", delta.pack.ordinal.to_string()),
            ("mode", "typed_delta".to_owned()),
            ("installation_id", binding.installation_id.to_string()),
            ("account_id", binding.account_id.to_string()),
            ("vehicle_id", binding.vehicle_id.to_string()),
            ("generation", binding.generation.to_string()),
            ("from_sequence", delta.from_sequence.to_string()),
            ("to_sequence", delta.to_sequence.to_string()),
            ("parent_digest", delta.parent_chain_digest.to_string()),
            ("external_base", "true".to_owned()),
        ];
        if expected_metadata
            .iter()
            .any(|(key, expected)| metadata.get(*key) != Some(expected))
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let selected_car_id = metadata
            .get("selected_car_id")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .ok_or(StoreError::LineageCatalogConflict)?;
        if selected_car_id != binding.selected_car_id {
            return Err(StoreError::LineageCatalogConflict);
        }

        let table_count = |table: &str| -> Result<u64, StoreError> {
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(StoreError::LineageCatalog)
                .and_then(|count| {
                    u64::try_from(count).map_err(|_| StoreError::LineageCatalogConflict)
                })
        };
        let cars = table_count("cars")?;
        let car_settings = table_count("car_settings")?;
        let drives = table_count("drives")?;
        let charges = table_count("charges")?;
        let positions = table_count("positions")?;
        let charge_samples = table_count("charge_samples")?;
        let states = table_count("states")?;
        let updates = table_count("updates")?;
        let tombstones = table_count("tombstones")?;
        // Every `ProjectionCar` materialises its embedded settings in the
        // companion table, so `car_settings` is the writer's logical count
        // for both car rows and explicit settings-only patches.
        let row_count = [
            car_settings,
            drives,
            charges,
            positions,
            charge_samples,
            states,
            updates,
            tombstones,
        ]
        .into_iter()
        .try_fold(0_u64, |total, count| {
            total
                .checked_add(count)
                .ok_or(StoreError::LineageCatalogConflict)
        })?;
        if row_count == 0
            || row_count != delta.pack.row_count
            || metadata.get("row_count") != Some(&row_count.to_string())
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        Self::verify_typed_delta_row_semantics(&connection, selected_car_id)?;
        let mut populated = HashSet::new();
        if cars != 0 || car_settings != 0 {
            populated.insert(crate::protocol::MirrorTable::Car);
        }
        for (count, table) in [
            (drives, crate::protocol::MirrorTable::Drive),
            (charges, crate::protocol::MirrorTable::Charge),
            (positions, crate::protocol::MirrorTable::Position),
            (charge_samples, crate::protocol::MirrorTable::ChargeSample),
            (states, crate::protocol::MirrorTable::State),
            (updates, crate::protocol::MirrorTable::Update),
            (tombstones, crate::protocol::MirrorTable::Tombstone),
        ] {
            if count != 0 {
                populated.insert(table);
            }
        }
        if populated != delta.pack.tables.iter().copied().collect() {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(())
    }

    pub fn pack_for_digest(&self, digest: Sha256Digest) -> Result<Option<StoredPack>, StoreError> {
        let connection = self.open_read_only_connection()?;
        if let Some(pack) = self.active_pack_for_digest(&connection, digest)? {
            return Ok(Some(pack));
        }
        self.retired_pack_for_digest_at(&connection, digest, retired_lineage_clock_ms()?)
    }

    fn active_pack_for_digest(
        &self,
        connection: &Connection,
        digest: Sha256Digest,
    ) -> Result<Option<StoredPack>, StoreError> {
        let entry = connection
            .query_row(
                "SELECT manifests.manifest_json, packs.relative_path, packs.compressed_bytes
                   FROM sync_packs AS packs
                   JOIN sync_manifests AS manifests
                     ON manifests.snapshot_id = packs.snapshot_id
                  WHERE packs.sha256 = ?1",
                params![digest.to_string()],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((manifest, relative_path, compressed_bytes)) = entry else {
            return Ok(None);
        };
        validate_catalogued_pack_manifest(&manifest)?;
        self.stored_pack_from_catalogue(digest, &relative_path, compressed_bytes)
            .map(Some)
    }

    fn retired_pack_for_digest_at(
        &self,
        connection: &Connection,
        digest: Sha256Digest,
        now_ms: i64,
    ) -> Result<Option<StoredPack>, StoreError> {
        if now_ms < 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let row: Option<(String, String, Vec<u8>, String, i64)> = connection
            .query_row(
                "SELECT lineage.vehicle_id, lineage.head_digest,
                        lineage.manifest_json, packs.relative_path,
                        packs.compressed_bytes
                 FROM sync_retired_lineage_packs AS packs
                 JOIN sync_retired_lineages AS lineage
                   ON lineage.vehicle_id = packs.vehicle_id
                  AND lineage.head_digest = packs.head_digest
                 WHERE packs.pack_digest = ?1 AND lineage.expires_at_ms > ?2
                 ORDER BY lineage.expires_at_ms DESC,
                          lineage.vehicle_id, lineage.head_digest
                 LIMIT 1",
                params![digest.to_string(), now_ms],
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
        let Some((vehicle_id, head_digest, manifest_json, relative_path, compressed_bytes)) = row
        else {
            return Ok(None);
        };
        validate_retired_lineage_pack_binding(
            &vehicle_id,
            &head_digest,
            &manifest_json,
            &digest.to_string(),
            &relative_path,
            compressed_bytes,
        )?;
        self.stored_pack_from_catalogue(digest, &relative_path, compressed_bytes)
            .map(Some)
    }

    fn stored_pack_from_catalogue(
        &self,
        digest: Sha256Digest,
        relative_path: &str,
        compressed_bytes: i64,
    ) -> Result<StoredPack, StoreError> {
        let compressed_bytes =
            u64::try_from(compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?;
        if relative_path != TransportPack::canonical_relative_path(digest) {
            return Err(StoreError::UnsafeStoredPackPath);
        }
        Ok(StoredPack {
            digest,
            compressed_bytes,
            path: self
                .packs_dir
                .join("sha256")
                .join(format!("{digest}.sqlite.zst")),
        })
    }

    pub fn lineage_manifest_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<LineageManifestV2>, StoreError> {
        self.lineage_manifest_for_vehicle_with_verification(
            vehicle_id,
            LineagePackVerification::FullDigest,
        )
    }

    fn lineage_manifest_for_vehicle_with_verification(
        &self,
        vehicle_id: Uuid,
        verification: LineagePackVerification,
    ) -> Result<Option<LineageManifestV2>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let base_row: Option<(String, i64, String, Vec<u8>)> = connection
            .query_row(
                "SELECT snapshot_id, base_sequence, base_digest, packs_json
                 FROM sync_bases WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?;
        let Some((snapshot_id, base_sequence, base_digest, packs_json)) = base_row else {
            return Ok(None);
        };
        let base_sequence =
            u64::try_from(base_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        let base_snapshot_id = Uuid::parse_str(&snapshot_id)
            .map_err(|_| StoreError::InvalidStoredUuid("lineage base snapshot"))?;
        let base_digest = base_digest
            .parse::<Sha256Digest>()
            .map_err(|_| StoreError::LineageCatalogConflict)?;
        let base_packs: Vec<TransportPack> =
            serde_json::from_slice(&packs_json).map_err(StoreError::DeserializeManifest)?;
        if base_packs.is_empty() {
            return Err(StoreError::LineageCatalogConflict);
        }
        for pack in &base_packs {
            self.verify_lineage_pack_for_mode(pack, verification)?;
        }

        let mut deltas = Vec::new();
        let mut statement = connection
            .prepare(
                "SELECT from_sequence, to_sequence, parent_chain_digest,
                        chain_digest, pack_digest, pack_json
                 FROM sync_deltas WHERE vehicle_id = ?1
                 ORDER BY from_sequence, to_sequence",
            )
            .map_err(StoreError::LineageCatalog)?;
        let rows = statement
            .query_map(params![vehicle_id.to_string()], |row| {
                let from_sequence: i64 = row.get(0)?;
                let to_sequence: i64 = row.get(1)?;
                let parent_chain_digest: String = row.get(2)?;
                let chain_digest: String = row.get(3)?;
                let pack_digest: String = row.get(4)?;
                let pack_json: Vec<u8> = row.get(5)?;
                Ok((
                    from_sequence,
                    to_sequence,
                    parent_chain_digest,
                    chain_digest,
                    pack_digest,
                    pack_json,
                ))
            })
            .map_err(StoreError::LineageCatalog)?;
        for row in rows {
            let (from_sequence, to_sequence, parent_chain_digest, chain_digest, pack_digest, json) =
                row.map_err(StoreError::LineageCatalog)?;
            let delta: LineageDelta =
                serde_json::from_slice(&json).map_err(StoreError::DeserializeManifest)?;
            if delta.from_sequence != u64::try_from(from_sequence).unwrap_or(u64::MAX)
                || delta.to_sequence != u64::try_from(to_sequence).unwrap_or(u64::MAX)
                || delta.parent_chain_digest.to_string() != parent_chain_digest
                || delta.chain_digest.to_string() != chain_digest
                || delta.pack_digest.to_string() != pack_digest
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            self.verify_lineage_pack_for_mode(&delta.pack, verification)?;
            deltas.push(delta);
        }
        drop(statement);

        let (head_base_snapshot, head_sequence, head_digest, terminal_cursor): (
            String,
            i64,
            String,
            String,
        ) = connection
            .query_row(
                "SELECT base_snapshot_id, head_sequence, head_digest, terminal_cursor
                     FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StoreError::LineageCatalog)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        if head_base_snapshot != snapshot_id {
            return Err(StoreError::LineageCatalogConflict);
        }
        let terminal_cursor: OpaqueCursor =
            serde_json::from_str(&terminal_cursor).map_err(StoreError::DeserializeManifest)?;
        let binding = self.v2_projection_binding(vehicle_id)?;
        let lineage = LineageManifestV2 {
            protocol: LINEAGE_PROTOCOL_V2,
            capability: LineageCapability::ImmutableBaseOrderedDeltas,
            schema: base_packs[0].schema,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id,
            generation: binding.generation,
            base: LineageBase {
                snapshot_id: base_snapshot_id,
                sequence: base_sequence,
                digest: base_digest,
                packs: base_packs,
            },
            deltas,
            head_sequence: u64::try_from(head_sequence)
                .map_err(|_| StoreError::InvalidStoredSequence)?,
            head_digest: head_digest
                .parse::<Sha256Digest>()
                .map_err(|_| StoreError::LineageCatalogConflict)?,
            terminal_cursor,
        };
        lineage.validate().map_err(StoreError::Manifest)?;
        Ok(Some(lineage))
    }
}
