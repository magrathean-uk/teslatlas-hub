// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    pub fn initialize(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref();
        let data_dir_was_absent = !data_dir.exists();
        fs::create_dir_all(data_dir).map_err(StoreError::CreateDataDir)?;
        // New roots receive the private one-user mode; existing roots must
        // already satisfy the same owner/mode contract or startup fails closed.
        if data_dir_was_absent {
            fs::set_permissions(
                data_dir,
                fs::Permissions::from_mode(SHARED_DATA_DIRECTORY_MODE),
            )
            .map_err(StoreError::ProtectDataDir)?;
        }
        validate_private_store_directory(data_dir)
            .map_err(|()| StoreError::UnsafeDataDir(data_dir.to_path_buf()))?;
        let packs_dir = data_dir.join("packs");
        let packs_dir_was_absent = !packs_dir.exists();
        fs::create_dir_all(&packs_dir).map_err(StoreError::CreatePacksDir)?;
        if packs_dir_was_absent {
            fs::set_permissions(
                &packs_dir,
                fs::Permissions::from_mode(SHARED_DATA_DIRECTORY_MODE),
            )
            .map_err(StoreError::ProtectPacksDir)?;
        }
        validate_private_store_directory(&packs_dir)
            .map_err(|()| StoreError::UnsafePacksDir(packs_dir.clone()))?;

        let store = Self {
            database_path: data_dir.join("hub.sqlite"),
            packs_dir,
            private_import_spool_dir: private_import_spool_root(data_dir),
            publication_lock_path: publication_lock_path(data_dir),
            immutable_snapshot: None,
            #[cfg(test)]
            stream_fault: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            projection_state_detach_fault: Arc::new(Mutex::new(false)),
        };
        store.ensure_schema_22_noop_directory()?;
        ensure_shared_sqlite_catalogue_file(&store.database_path)?;
        let mut connection = store.open()?;
        migrate(&connection)?;
        ensure_installation_id(&connection)?;
        store.recover_legacy_v2_base_bindings(&mut connection)?;
        // Never discard a staged import while another process owns its
        // publication workflow. A busy gate makes startup retryable instead.
        let publication_gate = store.try_acquire_publication_gate()?;
        cleanup_stale_pack_staging(store.packs_dir()).map_err(StoreError::PackStartupRepair)?;
        store.recover_stale_import_projection_state_spools(&publication_gate, &connection)?;
        cleanup_abandoned_import_generations(&connection)?;
        Ok(store)
    }

    pub fn open(&self) -> Result<Connection, StoreError> {
        admit_or_repair_shared_sqlite_sidecars(&self.database_path)?;
        let connection = Connection::open_with_flags(
            &self.database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::Open)?;
        configure(&connection)?;
        admit_or_repair_shared_sqlite_sidecars(&self.database_path)?;
        Ok(connection)
    }

    /// Schema 36 introduced immutable V2 projection bindings, but historical
    /// catalogues could already contain a V2 base when that table was created.
    /// Recover only from the stored base manifest and its content-addressed
    /// ordinal-zero car pack. Mutable source aliases, vehicle ownership and
    /// materialised lifecycle rows are deliberately outside this proof.
    fn recover_legacy_v2_base_bindings(
        &self,
        connection: &mut Connection,
    ) -> Result<(), StoreError> {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let candidates = {
            let mut statement = transaction
                .prepare(
                    "SELECT base.vehicle_id, base.snapshot_id, base.base_sequence,
                            base.base_digest, base.packs_json,
                            manifest.vehicle_id, manifest.head_sequence, manifest.manifest_json,
                            head.base_snapshot_id, head.head_sequence,
                            head.head_digest, head.terminal_cursor
                       FROM sync_bases AS base
                       LEFT JOIN sync_manifests AS manifest
                         ON manifest.snapshot_id = base.snapshot_id
                       LEFT JOIN sync_heads AS head
                         ON head.vehicle_id = base.vehicle_id
                       LEFT JOIN v2_base_bindings AS binding
                         ON binding.vehicle_id = base.vehicle_id
                      WHERE binding.vehicle_id IS NULL
                      ORDER BY base.vehicle_id",
                )
                .map_err(StoreError::LineageCatalog)?;
            statement
                .query_map([], |row| {
                    Ok(LegacyV2BaseBindingCandidate {
                        vehicle_id: row.get(0)?,
                        snapshot_id: row.get(1)?,
                        base_sequence: row.get(2)?,
                        base_digest: row.get(3)?,
                        packs_json: row.get(4)?,
                        manifest_vehicle_id: row.get(5)?,
                        manifest_head_sequence: row.get(6)?,
                        manifest_json: row.get(7)?,
                        head_snapshot_id: row.get(8)?,
                        head_sequence: row.get(9)?,
                        head_digest: row.get(10)?,
                        terminal_cursor: row.get(11)?,
                    })
                })
                .map_err(StoreError::LineageCatalog)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::LineageCatalog)?
        };

        let installation_id: String = transaction
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = ?1",
                params![INSTALLATION_ID_KEY],
                |row| row.get(0),
            )
            .map_err(StoreError::InstallationIdentity)?;
        let installation_id = installation_id
            .parse::<Uuid>()
            .ok()
            .filter(|value| !value.is_nil())
            .ok_or(StoreError::InvalidStoredUuid("installation identity"))?;

        for candidate in candidates {
            let manifest_json = candidate
                .manifest_json
                .as_deref()
                .ok_or(StoreError::LineageCatalogConflict)?;
            let Some(base) = legacy_v2_base_description(manifest_json)? else {
                // Generic V1 lineage bases do not carry a projection binding.
                continue;
            };
            if base.installation_id != installation_id
                || candidate.vehicle_id != base.vehicle_id.to_string()
                || candidate.snapshot_id != base.snapshot_id.to_string()
                || candidate.manifest_vehicle_id.as_deref() != Some(candidate.vehicle_id.as_str())
                || u64::try_from(candidate.base_sequence).ok() != Some(base.base_sequence)
                || candidate.manifest_head_sequence != Some(candidate.base_sequence)
                || candidate.base_digest != base.base_digest.to_string()
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            let stored_packs: Vec<TransportPack> = serde_json::from_slice(&candidate.packs_json)
                .map_err(StoreError::DeserializeManifest)?;
            if stored_packs != base.packs || stored_packs.is_empty() {
                return Err(StoreError::LineageCatalogConflict);
            }
            let head_sequence = candidate
                .head_sequence
                .and_then(|value| u64::try_from(value).ok())
                .ok_or(StoreError::LineageCatalogConflict)?;
            let head_digest = candidate
                .head_digest
                .as_deref()
                .ok_or(StoreError::LineageCatalogConflict)?
                .parse::<Sha256Digest>()
                .map_err(|_| StoreError::LineageCatalogConflict)?;
            let terminal_cursor = candidate
                .terminal_cursor
                .as_deref()
                .ok_or(StoreError::LineageCatalogConflict)?;
            let terminal_cursor: OpaqueCursor =
                serde_json::from_str(terminal_cursor).map_err(StoreError::DeserializeManifest)?;
            terminal_cursor
                .validate_shape()
                .map_err(StoreError::Manifest)?;
            if candidate.head_snapshot_id.as_deref() != Some(candidate.snapshot_id.as_str())
                || head_sequence < base.base_sequence
                || (head_sequence == base.base_sequence && head_digest != base.base_digest)
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            for pack in &base.packs {
                let catalogued: Option<(String, i64, String, i64, i64)> = transaction
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
                let expected = (
                    pack.snapshot_id.to_string(),
                    i64::from(pack.ordinal),
                    pack.relative_path.clone(),
                    i64::try_from(pack.compressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                    i64::try_from(pack.uncompressed_bytes)
                        .map_err(|_| StoreError::PackSizeTooLarge)?,
                );
                if catalogued.as_ref() != Some(&expected) {
                    return Err(StoreError::LineageCatalogConflict);
                }
            }

            let selected_car_id = self.inspect_legacy_v2_base_car_pack(
                base.packs
                    .first()
                    .ok_or(StoreError::LineageCatalogConflict)?,
                &base,
            )?;
            let binding = ProjectionBinding {
                installation_id: base.installation_id,
                account_id: base.account_id,
                vehicle_id: base.vehicle_id,
                generation: base.generation,
                selected_car_id,
            };
            record_immutable_v2_base_binding_values_in_transaction(
                &transaction,
                base.vehicle_id,
                base.snapshot_id,
                &binding,
            )?;
        }
        transaction.commit().map_err(StoreError::LineageCatalog)
    }

    /// Open an existing live Hub catalogue without creating directories,
    /// migrating schema, or issuing mutating SQL. SQLite may still create WAL
    /// coordination files while observing a concurrently active catalogue.
    pub fn open_read_only(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_read_only_with_mode(data_dir.as_ref(), false)
    }

    /// Open an already-migrated live catalogue for a short local control
    /// transaction without taking the long-lived collector process lock.
    pub fn open_existing(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_read_only_with_mode(data_dir.as_ref(), false)
    }

    /// Open a byte-stable immutable snapshot for operator diagnosis. This
    /// refuses a pending WAL and must be followed by
    /// [`Self::verify_immutable_snapshot_unchanged`] before reporting success.
    pub fn open_immutable_read_only(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_read_only_with_mode(data_dir.as_ref(), true)
    }

    fn open_read_only_with_mode(data_dir: &Path, immutable: bool) -> Result<Self, StoreError> {
        let database_path = data_dir.join("hub.sqlite");
        let immutable_snapshot = if immutable {
            Some(immutable_catalogue_fingerprint(&database_path)?)
        } else {
            None
        };
        let store = Self {
            database_path,
            packs_dir: data_dir.join("packs"),
            private_import_spool_dir: private_import_spool_root(data_dir),
            publication_lock_path: publication_lock_path(data_dir),
            immutable_snapshot,
            #[cfg(test)]
            stream_fault: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            projection_state_detach_fault: Arc::new(Mutex::new(false)),
        };
        let connection = store.open_read_only_connection()?;
        let application_id: i32 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        if application_id != APPLICATION_ID {
            return Err(StoreError::InvalidApplicationId(application_id));
        }
        let version = schema_version(&connection)?;
        if version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema(version));
        }
        Ok(store)
    }

    fn open_read_only_connection(&self) -> Result<Connection, StoreError> {
        let connection = if self.immutable_snapshot.is_some() {
            let canonical = self
                .database_path
                .canonicalize()
                .map_err(StoreError::ResolveCataloguePath)?;
            let mut uri = url::Url::from_file_path(&canonical)
                .map_err(|()| StoreError::InvalidCataloguePath)?;
            uri.set_query(Some("immutable=1&mode=ro"));
            Connection::open_with_flags(
                uri.as_str(),
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(StoreError::Open)?
        } else {
            Connection::open_with_flags(
                &self.database_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(StoreError::Open)?
        };
        configure_read_only(&connection)?;
        Ok(connection)
    }

    /// Prove that the immutable catalogue diagnosed by `doctor` was not stale
    /// or raced by a writer while its checks were running.
    pub fn verify_immutable_snapshot_unchanged(&self) -> Result<(), StoreError> {
        let expected = self
            .immutable_snapshot
            .as_ref()
            .ok_or(StoreError::ImmutableSnapshotRequired)?;
        let actual = immutable_catalogue_fingerprint(&self.database_path)?;
        if &actual == expected {
            Ok(())
        } else {
            Err(StoreError::CatalogueChangedDuringImmutableCheck)
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Finish a short-lived writer command with a byte-stable catalogue that
    /// immutable `doctor` and service preflight can inspect immediately.
    pub fn checkpoint_catalogue_for_immutable_read(&self) -> Result<(), StoreError> {
        let connection = self.open()?;
        let (busy, log_frames, checkpointed): (i64, i64, i64) = connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(StoreError::CatalogueCheckpoint)?;
        if busy != 0 || log_frames != checkpointed {
            return Err(StoreError::CatalogueCheckpointIncomplete);
        }
        Ok(())
    }
}
