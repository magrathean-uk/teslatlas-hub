// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    /// Serialize complete local publication workflows across every Hub process
    /// sharing this data directory. Callers must keep the returned guard alive
    /// from before sequence reservation until catalogue, lifecycle, and pack
    /// ownership work has completed.
    pub(crate) async fn acquire_publication_gate(&self) -> Result<PublicationGate, StoreError> {
        let file = self.open_publication_gate()?;
        loop {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(PublicationGate { _file: file }),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(PUBLICATION_GATE_RETRY).await;
                }
                Err(error) => return Err(StoreError::LockPublicationGate(error.into())),
            }
        }
    }

    /// Attempt to acquire the publication gate without ever waiting. This is
    /// used only by synchronous library seams; async production workflows use
    /// `acquire_publication_gate` so contention yields to Tokio rather than
    /// blocking a worker thread.
    pub(crate) fn try_acquire_publication_gate(&self) -> Result<PublicationGate, StoreError> {
        let file = self.open_publication_gate()?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(PublicationGate { _file: file }),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Err(StoreError::PublicationGateBusy)
            }
            Err(error) => Err(StoreError::LockPublicationGate(error.into())),
        }
    }

    fn open_publication_gate(&self) -> Result<File, StoreError> {
        let expected_gid = shared_sqlite_group_id(&self.database_path)?;
        let path = &self.publication_lock_path;
        let parent_path = path
            .parent()
            .ok_or_else(|| StoreError::UnsafePublicationGate(path.clone()))?;
        let name = path
            .file_name()
            .ok_or_else(|| StoreError::UnsafePublicationGate(path.clone()))?;
        let parent_fd = open(
            parent_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| StoreError::OpenPublicationGate(error.into()))?;
        let parent = File::from(parent_fd);
        let parent_metadata =
            fstat(&parent).map_err(|error| StoreError::OpenPublicationGate(error.into()))?;
        if !FileType::from_raw_mode(parent_metadata.st_mode).is_dir()
            || parent_metadata.st_gid != expected_gid
        {
            return Err(StoreError::UnsafePublicationGate(parent_path.to_path_buf()));
        }

        let mut created = false;
        let mut gate_fd = None;
        for _ in 0..32 {
            match openat(
                &parent,
                name,
                OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => {
                    gate_fd = Some(fd);
                    break;
                }
                Err(Errno::NOENT) => match openat(
                    &parent,
                    name,
                    OFlags::RDWR
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(SHARED_DATA_FILE_MODE as _),
                ) {
                    Ok(fd) => {
                        created = true;
                        gate_fd = Some(fd);
                        break;
                    }
                    Err(Errno::EXIST) => continue,
                    Err(error) => return Err(StoreError::OpenPublicationGate(error.into())),
                },
                Err(error) => return Err(StoreError::OpenPublicationGate(error.into())),
            }
        }
        let gate_fd = gate_fd.ok_or_else(|| {
            StoreError::OpenPublicationGate(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "publication gate path did not settle",
            ))
        })?;
        let file = File::from(gate_fd);
        if created {
            fchmod(&file, Mode::from_raw_mode(SHARED_DATA_FILE_MODE as _))
                .map_err(|error| StoreError::ProtectPublicationGate(error.into()))?;
            file.sync_all()
                .map_err(StoreError::ProtectPublicationGate)?;
            parent
                .sync_all()
                .map_err(StoreError::ProtectPublicationGate)?;
        }
        let metadata =
            fstat(&file).map_err(|error| StoreError::OpenPublicationGate(error.into()))?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file()
            || metadata.st_gid != expected_gid
            || Mode::from_raw_mode(metadata.st_mode).as_raw_mode() as u32 & 0o777
                != SHARED_DATA_FILE_MODE
        {
            return Err(StoreError::UnsafePublicationGate(path.clone()));
        }
        Ok(file)
    }

    pub fn upsert_car_settings(
        &self,
        vehicle_id: Uuid,
        car_id: i64,
        settings: &ProjectionCarSettings,
    ) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let before = load_car_settings_row(&transaction, vehicle_id)?;
        transaction
            .execute(
                "INSERT INTO car_settings(
                    vehicle_id, car_id, enabled, use_streaming_api,
                    suspend_after_idle_min, suspend_min, req_not_unlocked,
                    free_supercharging, lfp_battery, suspend_min_resolved
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    car_id=excluded.car_id, enabled=excluded.enabled,
                    use_streaming_api=excluded.use_streaming_api,
                    suspend_after_idle_min=excluded.suspend_after_idle_min,
                    suspend_min=CASE WHEN car_settings.suspend_min_resolved != 0
                        THEN car_settings.suspend_min ELSE excluded.suspend_min END,
                    suspend_min_resolved=MAX(car_settings.suspend_min_resolved,
                        excluded.suspend_min_resolved),
                    req_not_unlocked=excluded.req_not_unlocked,
                    free_supercharging=excluded.free_supercharging,
                    lfp_battery=excluded.lfp_battery",
                params![
                    vehicle_id.to_string(),
                    car_id,
                    settings.enabled,
                    settings.use_streaming_api,
                    settings.suspend_after_idle_min,
                    settings.suspend_min,
                    settings.req_not_unlocked,
                    settings.free_supercharging,
                    settings.lfp_battery,
                    settings.suspend_min_resolved,
                ],
            )
            .map_err(StoreError::Query)?;
        let (effective_car_id, effective_settings) =
            load_car_settings_row(&transaction, vehicle_id)?
                .ok_or_else(|| StoreError::Query(rusqlite::Error::QueryReturnedNoRows))?;
        let changed = before.as_ref() != Some(&(effective_car_id, effective_settings.clone()));
        if !changed {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(());
        }
        let current_car: Option<String> = transaction
            .query_row(
                "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if let Some(current_car) = current_car {
            let mut car: ProjectionCar =
                serde_json::from_str(&current_car).map_err(StoreError::DeserializeLifecycleRow)?;
            car.settings = effective_settings.clone();
            let car_json =
                serde_json::to_string(&car).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_cars SET car_json = ?1 WHERE vehicle_id = ?2",
                    params![car_json, vehicle_id.to_string()],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        let payload = serde_json::to_string(&effective_settings)
            .map_err(StoreError::SerializeLifecycleRow)?;
        record_sync_mutation_in_transaction(
            &transaction,
            vehicle_id,
            "car_setting",
            effective_car_id,
            effective_car_id,
            "upsert",
            &payload,
        )?;
        mark_export_dirty_in_transaction(&transaction, vehicle_id)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(())
    }

    /// Replace operator-controlled settings exactly. Collector discovery uses
    /// `upsert_car_settings`, which preserves an already-resolved suspend
    /// value; this path is the explicit owner override.
    pub fn replace_car_settings(
        &self,
        vehicle_id: Uuid,
        settings: &ProjectionCarSettings,
    ) -> Result<(), StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if settings.suspend_after_idle_min <= 0 || settings.suspend_min <= 0 {
            return Err(StoreError::InvalidCarSettings);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let before = load_car_settings_row(&transaction, vehicle_id)?
            .ok_or(StoreError::UnknownVehicle(vehicle_id))?;
        transaction
            .execute(
                "UPDATE car_settings SET
                    enabled = ?2,
                    use_streaming_api = ?3,
                    suspend_after_idle_min = ?4,
                    suspend_min = ?5,
                    req_not_unlocked = ?6,
                    free_supercharging = ?7,
                    lfp_battery = ?8,
                    suspend_min_resolved = ?9
                 WHERE vehicle_id = ?1",
                params![
                    vehicle_id.to_string(),
                    settings.enabled,
                    settings.use_streaming_api,
                    settings.suspend_after_idle_min,
                    settings.suspend_min,
                    settings.req_not_unlocked,
                    settings.free_supercharging,
                    settings.lfp_battery,
                    settings.suspend_min_resolved,
                ],
            )
            .map_err(StoreError::Query)?;
        let (car_id, effective) = load_car_settings_row(&transaction, vehicle_id)?
            .ok_or(StoreError::UnknownVehicle(vehicle_id))?;
        if before == (car_id, effective.clone()) {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(());
        }
        let current_car: Option<String> = transaction
            .query_row(
                "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if let Some(current_car) = current_car {
            let mut car: ProjectionCar =
                serde_json::from_str(&current_car).map_err(StoreError::DeserializeLifecycleRow)?;
            car.settings = effective.clone();
            let car_json =
                serde_json::to_string(&car).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_cars SET car_json = ?1 WHERE vehicle_id = ?2",
                    params![car_json, vehicle_id.to_string()],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        let payload =
            serde_json::to_string(&effective).map_err(StoreError::SerializeLifecycleRow)?;
        record_sync_mutation_in_transaction(
            &transaction,
            vehicle_id,
            "car_setting",
            car_id,
            car_id,
            "upsert",
            &payload,
        )?;
        mark_export_dirty_in_transaction(&transaction, vehicle_id)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(())
    }

    /// Materialise the first car record without replacing an existing
    /// authoritative record. Later lifecycle metadata patches update it.
    pub fn persist_materialised_car_if_absent(
        &self,
        vehicle_id: Uuid,
        car: &crate::hub_pack::ProjectionCar,
    ) -> Result<(), StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let car_json = serde_json::to_string(car).map_err(StoreError::SerializeLifecycleRow)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let inserted = transaction
            .execute(
                "INSERT INTO materialised_cars(vehicle_id, car_id, car_json)
                 VALUES (?1, ?2, ?3) ON CONFLICT(vehicle_id) DO NOTHING",
                params![vehicle_id.to_string(), car.id, car_json],
            )
            .map_err(StoreError::LifecycleWrite)?;
        if inserted != 0 {
            record_sync_mutation_in_transaction(
                &transaction,
                vehicle_id,
                "car",
                car.id,
                car.id,
                "upsert",
                &car_json,
            )?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    pub fn load_car_settings(&self, vehicle_id: Uuid) -> Result<ProjectionCarSettings, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT enabled, use_streaming_api, suspend_after_idle_min, suspend_min,
                        req_not_unlocked, free_supercharging, lfp_battery,
                        suspend_min_resolved
                 FROM car_settings WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| {
                    Ok(ProjectionCarSettings {
                        enabled: row.get::<_, i64>(0)? != 0,
                        use_streaming_api: row.get::<_, i64>(1)? != 0,
                        suspend_after_idle_min: row.get(2)?,
                        suspend_min: row.get(3)?,
                        suspend_min_resolved: row.get::<_, i64>(7)? != 0,
                        req_not_unlocked: row.get::<_, i64>(4)? != 0,
                        free_supercharging: row.get::<_, i64>(5)? != 0,
                        lfp_battery: row.get::<_, i64>(6)? != 0,
                    })
                },
            )
            .optional()
            .map(|settings| settings.unwrap_or_default())
            .map_err(StoreError::Query)
    }

    pub fn resolve_car_suspend_min(
        &self,
        vehicle_id: Uuid,
        model: Option<&str>,
        trim_badging: Option<&str>,
        marketing_name: Option<&str>,
    ) -> Result<bool, StoreError> {
        let Some(suspend_min) =
            crate::hub_pack::teslamate_suspend_min_default(model, trim_badging, marketing_name)
        else {
            return Ok(false);
        };
        let connection = self.open()?;
        let changed = connection
            .execute(
                "UPDATE car_settings
                 SET suspend_min = ?1, suspend_min_resolved = 1
                 WHERE vehicle_id = ?2 AND suspend_min_resolved = 0",
                params![suspend_min, vehicle_id.to_string()],
            )
            .map_err(StoreError::Query)?;
        Ok(changed != 0)
    }

    /// Create one consistent SQLite catalogue backup through SQLite's online
    /// backup API. The destination must be a new Hub-owned file; packs are
    /// intentionally handled by a separate immutable-object backup step.
    pub fn backup_catalogue_to(&self, destination: &Path) -> Result<(), StoreError> {
        if destination == self.database_path {
            return Err(StoreError::BackupDestinationIsLiveDatabase);
        }
        if destination.exists() {
            return Err(StoreError::BackupDestinationExists(
                destination.to_path_buf(),
            ));
        }
        let source = self.open()?;
        let mut backup_destination = Connection::open(destination).map_err(StoreError::Open)?;
        let result = Backup::new(&source, &mut backup_destination)
            .and_then(|backup| backup.run_to_completion(128, Duration::ZERO, None));
        drop(backup_destination);
        match result {
            Ok(()) => {
                // This is a newly created catalogue, not an upgrade repair.
                // Give a later split-UID HubStore the same group-writable
                // catalogue shape it requires for the live tree.
                fs::set_permissions(
                    destination,
                    fs::Permissions::from_mode(SHARED_SQLITE_FILE_MODE),
                )
                .map_err(StoreError::ProtectSharedSqlite)?;
                File::open(destination)
                    .and_then(|file| file.sync_all())
                    .map_err(StoreError::ProtectSharedSqlite)?;
                Ok(())
            }
            Err(error) => {
                let _ = fs::remove_file(destination);
                Err(StoreError::Backup(error))
            }
        }
    }

    /// Exact copy-byte admission for [`Self::backup_to`]. SQLite's online
    /// backup copies the live page set; immutable packs and schema-2.2 no-op
    /// files come from the same catalogue-selected sets used by the copier.
    /// Unreferenced files are deliberately excluded.
    fn backup_copy_bytes_with_gate(
        &self,
        _publication_gate: &PublicationGate,
    ) -> Result<u64, StoreError> {
        let connection = self.open()?;
        let page_count: i64 = connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        let page_size: i64 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        let mut total = u64::try_from(page_count)
            .ok()
            .and_then(|pages| {
                u64::try_from(page_size)
                    .ok()
                    .and_then(|size| pages.checked_mul(size))
            })
            .ok_or(StoreError::BackupCapacityOverflow)?;
        for (_, _, expected_bytes) in
            referenced_pack_rows_at(&connection, retired_lineage_clock_ms()?)?
        {
            total = total
                .checked_add(
                    u64::try_from(expected_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
                )
                .ok_or(StoreError::BackupCapacityOverflow)?;
        }
        let manifest_rows = connection
            .prepare(
                "SELECT manifest_json FROM sync_manifests
                 WHERE json_extract(manifest_json, '$.mode') = 'full_snapshot'
                 ORDER BY vehicle_id, head_sequence DESC, snapshot_id DESC",
            )
            .map_err(StoreError::Query)?
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        let mut visited = HashSet::new();
        for payload in manifest_rows {
            let manifest = decode_manifest(payload)?;
            if !visited.insert(manifest.vehicle_id) || manifest.schema != HUB_PROJECTION_SCHEMA_V3 {
                continue;
            }
            let bytes = self
                .schema_22_noop_for_snapshot(manifest.vehicle_id, manifest.snapshot_id)?
                .ok_or(StoreError::Schema22NoOpNotFound)?;
            total = total
                .checked_add(
                    u64::try_from(bytes.len()).map_err(|_| StoreError::BackupCapacityOverflow)?,
                )
                .ok_or(StoreError::BackupCapacityOverflow)?;
        }
        Ok(total)
    }

    pub(crate) fn begin_backup_snapshot(&self) -> Result<HubBackupSnapshot<'_>, StoreError> {
        Ok(HubBackupSnapshot {
            store: self,
            publication_gate: self.try_acquire_publication_gate()?,
        })
    }

    #[cfg(test)]
    pub(crate) fn backup_copy_bytes(&self) -> Result<u64, StoreError> {
        self.begin_backup_snapshot()?.copy_bytes()
    }

    /// Create a complete Hub-owned restore directory. The catalogue is copied
    /// first through SQLite's online backup API; immutable packs are then
    /// copied from the exact referenced set in that copied catalogue.
    pub fn backup_to(&self, destination: &Path) -> Result<(), StoreError> {
        self.begin_backup_snapshot()?.copy_to(destination)
    }

    fn backup_to_with_gate(
        &self,
        destination: &Path,
        publication_gate: &PublicationGate,
    ) -> Result<(), StoreError> {
        if destination.exists() {
            return Err(StoreError::BackupDestinationExists(
                destination.to_path_buf(),
            ));
        }
        fs::create_dir(destination).map_err(StoreError::CreateBackupDirectory)?;
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(SHARED_DATA_DIRECTORY_MODE),
        )
        .map_err(StoreError::CreateBackupDirectory)?;
        let result = self.backup_to_created_directory(destination, publication_gate);
        if result.is_err() {
            let _ = fs::remove_dir_all(destination);
        }
        result
    }

    fn backup_to_created_directory(
        &self,
        destination: &Path,
        publication_gate: &PublicationGate,
    ) -> Result<(), StoreError> {
        let catalogue = destination.join("hub.sqlite");
        self.backup_catalogue_to(&catalogue)?;
        let copied_catalogue = Connection::open_with_flags(
            &catalogue,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::Open)?;
        let rows = referenced_pack_rows_at(&copied_catalogue, retired_lineage_clock_ms()?)?;
        let packs = destination.join("packs").join("sha256");
        fs::create_dir_all(&packs).map_err(StoreError::CreateBackupDirectory)?;
        fs::set_permissions(
            destination.join("packs"),
            fs::Permissions::from_mode(SHARED_DATA_DIRECTORY_MODE),
        )
        .and_then(|()| {
            fs::set_permissions(
                &packs,
                fs::Permissions::from_mode(SHARED_DATA_DIRECTORY_MODE),
            )
        })
        .map_err(StoreError::CreateBackupDirectory)?;
        for (sha256, relative_path, expected_bytes) in rows {
            let expected_bytes =
                u64::try_from(expected_bytes).map_err(|_| StoreError::PackSizeTooLarge)?;
            if !is_sha256_hex(&sha256)
                || relative_path != format!("/v1/packs/sha256/{sha256}.sqlite.zst")
            {
                return Err(StoreError::UnsafeStoredPackPath);
            }
            let filename = format!("{sha256}.sqlite.zst");
            let source = self.packs_dir.join("sha256").join(&filename);
            let backup = packs.join(&filename);
            let copied =
                fs::copy(&source, &backup).map_err(|source_error| StoreError::CopyBackupPack {
                    source_path: source.clone(),
                    destination: backup.clone(),
                    source_error,
                })?;
            if copied != expected_bytes {
                return Err(StoreError::BackupPackSizeMismatch {
                    path: source,
                    expected: expected_bytes,
                    actual: copied,
                });
            }
            if sha256_file_hex(&backup)? != sha256 {
                return Err(StoreError::BackupPackDigestMismatch { path: backup });
            }
        }
        self.backup_current_schema_22_noops(
            destination,
            &catalogue,
            &copied_catalogue,
            publication_gate,
        )?;
        Ok(())
    }

    fn backup_current_schema_22_noops(
        &self,
        destination: &Path,
        catalogue: &Path,
        copied_catalogue: &Connection,
        publication_gate: &PublicationGate,
    ) -> Result<(), StoreError> {
        let manifest_rows = copied_catalogue
            .prepare(
                "SELECT manifest_json FROM sync_manifests
                 WHERE json_extract(manifest_json, '$.mode') = 'full_snapshot'
                 ORDER BY vehicle_id, head_sequence DESC, snapshot_id DESC",
            )
            .map_err(StoreError::Query)?
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        let backup_store = Self {
            database_path: catalogue.to_path_buf(),
            packs_dir: destination.join("packs"),
            private_import_spool_dir: private_import_spool_root(destination),
            publication_lock_path: publication_lock_path(destination),
            immutable_snapshot: None,
            #[cfg(test)]
            stream_fault: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            projection_state_detach_fault: Arc::new(Mutex::new(false)),
        };
        let mut visited = HashSet::new();
        for payload in manifest_rows {
            let manifest = decode_manifest(payload)?;
            if !visited.insert(manifest.vehicle_id) || manifest.schema != HUB_PROJECTION_SCHEMA_V3 {
                continue;
            }
            let bytes = self
                .schema_22_noop_for_snapshot(manifest.vehicle_id, manifest.snapshot_id)?
                .ok_or(StoreError::Schema22NoOpNotFound)?;
            let noop: crate::updates_delivery::SignedNoOpState = serde_json::from_slice(&bytes)
                .map_err(|error| StoreError::InvalidSchema22Pair(error.to_string()))?;
            let canonical = serde_json::to_vec(&noop)
                .map_err(|error| StoreError::InvalidSchema22Pair(error.to_string()))?;
            if canonical != bytes {
                return Err(StoreError::InvalidSchema22Pair(
                    "no-op is not canonical typed JSON".into(),
                ));
            }
            crate::updates_delivery::validate_schema_22_pair(&manifest, &noop)
                .map_err(|error| StoreError::InvalidSchema22Pair(error.message))?;
            backup_store.publish_schema_22_noop(publication_gate, &noop)?;
        }
        Ok(())
    }

    pub fn packs_dir(&self) -> &Path {
        &self.packs_dir
    }

    /// Construct a production direct-import state spool only while the caller
    /// holds the publication gate and the exact generation is still staging.
    /// This is deliberately narrower than the generic projection-state
    /// constructor used by isolated tests and non-generation seams.
    pub(crate) fn create_import_projection_state(
        &self,
        _publication_gate: &PublicationGate,
        run_id: Uuid,
        limits: TeslaMateProjectionStateLimits,
        maximum_changed_row_payload_bytes: u64,
    ) -> Result<TeslaMateProjectionState, StoreError> {
        if run_id.is_nil() {
            return Err(StoreError::InvalidImportGeneration);
        }
        let connection = self.open()?;
        let staging: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM import_generations
                     WHERE run_id = ?1 AND status = 'staging'
                )",
                params![run_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::ImportGeneration)?;
        if !staging {
            return Err(StoreError::ImportGenerationNotFound);
        }
        let data_root = self
            .database_path
            .parent()
            .expect("Hub database path always has a data directory");
        let expected_spool_identity = private_import_spool_identity(data_root)?;
        if rustix::process::geteuid().as_raw() != expected_spool_identity.uid {
            return Err(StoreError::UnsafeImportSpool(
                self.private_import_spool_dir.clone(),
            ));
        }
        ensure_private_import_spool_directory(
            &self.private_import_spool_dir,
            expected_spool_identity,
        )?;
        TeslaMateProjectionState::create_for_import_generation(
            &self.private_import_spool_dir,
            run_id,
            limits,
            maximum_changed_row_payload_bytes,
        )
        .map_err(StoreError::TeslaMateProjectionState)
    }

    fn recover_stale_import_projection_state_spools(
        &self,
        _publication_gate: &PublicationGate,
        connection: &Connection,
    ) -> Result<(), StoreError> {
        // `initialize` holds the same process-wide gate held through every
        // production capture. A v1 run can therefore be reclaimed only here,
        // after the namespace has been fully validated by the state module.
        // A collector-only process must not create the Hub-private import
        // spool during ordinary startup. It is created atomically by the Hub
        // import identity only when a direct import is actually admitted.
        let data_root = self
            .database_path
            .parent()
            .expect("Hub database path always has a data directory");
        let expected_spool_identity = private_import_spool_identity(data_root)?;
        let spool_metadata = match fs::symlink_metadata(&self.private_import_spool_dir) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(StoreError::InspectImportSpool(error)),
            Ok(metadata) => metadata,
        };
        validate_private_import_spool_directory(
            &self.private_import_spool_dir,
            &spool_metadata,
            expected_spool_identity,
        )?;
        match fs::read_dir(&self.private_import_spool_dir) {
            // The collector deliberately cannot traverse the Hub-only spool.
            // It must never reclaim another identity's interrupted import;
            // the next Hub/API startup performs the owned recovery instead.
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    && rustix::process::geteuid().as_raw() != expected_spool_identity.uid =>
            {
                return Ok(());
            }
            Err(error) => return Err(StoreError::InspectImportSpool(error)),
            Ok(_) => {}
        }
        for run_id in recover_stale_import_generation_spools(&self.private_import_spool_dir)? {
            connection
                .execute(
                    "DELETE FROM import_generations
                      WHERE run_id = ?1 AND status = 'staging'",
                    params![run_id.to_string()],
                )
                .map_err(StoreError::ImportGeneration)?;
        }
        Ok(())
    }

    /// Private, disposable local capture area. TeslaMate source snapshots are
    /// never written into the Hub catalogue database.
    pub fn imports_dir(&self) -> PathBuf {
        self.database_path
            .parent()
            .expect("Hub database path always has a data directory")
            .join("imports")
    }

    /// Atomically claim the singleton supervised-collector lease. A live
    /// predecessor cannot be displaced; an expired predecessor can be
    /// replaced without deleting its crash evidence first.
    pub(crate) fn acquire_supervised_collector_lease(
        &self,
        now_ms: i64,
    ) -> Result<SupervisedCollectorLease, StoreError> {
        validate_timestamp("collector lease acquisition", now_ms)?;
        let lease_until_ms = supervised_collector_lease_deadline(now_ms)?;
        let lease = SupervisedCollectorLease {
            instance_id: Uuid::new_v4(),
        };
        let connection = self.open()?;
        let changed = connection
            .execute(
                "INSERT INTO supervised_collector_lease(
                    singleton_id, instance_id, state, started_at_ms,
                    heartbeat_at_ms, lease_until_ms
                 ) VALUES (1, ?1, 'active', ?2, ?2, ?3)
                 ON CONFLICT(singleton_id) DO UPDATE SET
                    instance_id = excluded.instance_id,
                    state = excluded.state,
                    started_at_ms = excluded.started_at_ms,
                    heartbeat_at_ms = excluded.heartbeat_at_ms,
                    lease_until_ms = excluded.lease_until_ms
                 WHERE supervised_collector_lease.lease_until_ms <= excluded.heartbeat_at_ms",
                params![lease.instance_id.to_string(), now_ms, lease_until_ms],
            )
            .map_err(StoreError::SupervisedCollectorLeaseWrite)?;
        if changed == 1 {
            Ok(lease)
        } else {
            Err(StoreError::SupervisedCollectorLeaseHeld)
        }
    }

    /// Renew only the exact lease owned by this process. The macOS data-dir
    /// lock is the process singleton, so a delayed local heartbeat may revive
    /// its expired readiness record unless another instance has replaced it.
    pub(crate) fn heartbeat_supervised_collector_lease(
        &self,
        lease: SupervisedCollectorLease,
        state: SupervisedCollectorState,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("collector heartbeat", now_ms)?;
        let lease_until_ms = supervised_collector_lease_deadline(now_ms)?;
        let connection = self.open()?;
        let changed = connection
            .execute(
                "UPDATE supervised_collector_lease
                    SET state = ?1, heartbeat_at_ms = ?2, lease_until_ms = ?3
                 WHERE singleton_id = 1
                    AND instance_id = ?4",
                params![
                    state.as_str(),
                    now_ms,
                    lease_until_ms,
                    lease.instance_id.to_string()
                ],
            )
            .map_err(StoreError::SupervisedCollectorLeaseWrite)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(StoreError::SupervisedCollectorLeaseLost)
        }
    }

    /// Remove only this process's lease on an orderly exit. A stale process
    /// cannot clear the replacement that acquired the singleton after expiry.
    pub(crate) fn release_supervised_collector_lease(
        &self,
        lease: SupervisedCollectorLease,
    ) -> Result<(), StoreError> {
        let connection = self.open()?;
        connection
            .execute(
                "DELETE FROM supervised_collector_lease
                  WHERE singleton_id = 1 AND instance_id = ?1",
                params![lease.instance_id.to_string()],
            )
            .map_err(StoreError::SupervisedCollectorLeaseWrite)?;
        Ok(())
    }

    pub fn quick_check(&self) -> Result<(), StoreError> {
        let connection = self.open_read_only_connection()?;
        let result: String = connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        if result == "ok" {
            Ok(())
        } else {
            Err(StoreError::Integrity(result))
        }
    }

    /// Fast service readiness for `/readyz`.
    ///
    /// Opens the catalogue, probes that core tables respond, and refuses when
    /// lifecycle state is quarantined. Deliberately does **not** run
    /// `PRAGMA quick_check` — that full-table scan blocks the TLS accept path
    /// for multi-GB post-import databases (10M+ positions) for many minutes and
    /// makes the Hub appear dead to readiness probes. Operators use
    /// [`Self::catalogue_check`] / [`Self::quick_check`] for integrity gates.
    pub fn readiness_check(&self) -> Result<(), StoreError> {
        let connection = self.open_read_only_connection()?;
        // Cheap openability probe (fails closed on corrupt headers / missing schema).
        let _: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let _: i64 = connection
            .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        let quarantined: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM vehicle_lifecycle_state WHERE quarantined != 0",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let quarantined =
            usize::try_from(quarantined).map_err(|_| StoreError::InvalidStoredCount)?;
        if quarantined == 0 {
            Ok(())
        } else {
            Err(StoreError::QuarantinedLifecycle(quarantined))
        }
    }

    /// Fast, redacted service readiness used by `/readyz`. This deliberately
    /// stops at catalogue/manifest validation and file metadata. Same-size
    /// content corruption remains a `doctor` / [`Self::catalogue_check`] gate;
    /// hashing a multi-gigabyte published corpus on every HTTP probe would
    /// itself make the service unavailable.
    pub fn service_readiness_at(
        &self,
        supervised_collector_required: bool,
        now_ms: i64,
    ) -> Result<(), ReadinessFailure> {
        match self.readiness_check() {
            Ok(()) => {}
            Err(StoreError::QuarantinedLifecycle(_)) => {
                return Err(ReadinessFailure {
                    code: ReadinessReasonCode::LifecycleQuarantined,
                });
            }
            Err(_) => {
                return Err(ReadinessFailure {
                    code: ReadinessReasonCode::CatalogueUnavailable,
                });
            }
        }
        self.verify_active_published_content_metadata()
            .map_err(|_| ReadinessFailure {
                code: ReadinessReasonCode::PublishedContentUnservable,
            })?;
        if supervised_collector_required {
            self.verify_supervised_collector_readiness_at(now_ms)?;
        }
        Ok(())
    }

    fn verify_supervised_collector_readiness_at(
        &self,
        now_ms: i64,
    ) -> Result<(), ReadinessFailure> {
        if now_ms < 0 {
            return Err(ReadinessFailure {
                code: ReadinessReasonCode::CatalogueUnavailable,
            });
        }
        let connection = self
            .open_read_only_connection()
            .map_err(|_| ReadinessFailure {
                code: ReadinessReasonCode::CatalogueUnavailable,
            })?;
        let row: Option<(String, String, i64, i64, i64)> = connection
            .query_row(
                "SELECT instance_id, state, started_at_ms,
                        heartbeat_at_ms, lease_until_ms
                   FROM supervised_collector_lease WHERE singleton_id = 1",
                [],
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
            .map_err(|_| ReadinessFailure {
                code: ReadinessReasonCode::CatalogueUnavailable,
            })?;
        let Some((instance_id, state, started_at_ms, heartbeat_at_ms, lease_until_ms)) = row else {
            return Err(ReadinessFailure {
                code: ReadinessReasonCode::CollectorAbsent,
            });
        };
        if Uuid::parse_str(&instance_id)
            .ok()
            .is_none_or(|value| value.is_nil())
            || started_at_ms < 0
            || heartbeat_at_ms < started_at_ms
            || lease_until_ms <= heartbeat_at_ms
            || !matches!(state.as_str(), "active" | "auth_terminal")
        {
            return Err(ReadinessFailure {
                code: ReadinessReasonCode::CatalogueUnavailable,
            });
        }
        if lease_until_ms <= now_ms {
            return Err(ReadinessFailure {
                code: ReadinessReasonCode::CollectorStale,
            });
        }
        if state == SupervisedCollectorState::AuthenticationTerminal.as_str() {
            return Err(ReadinessFailure {
                code: ReadinessReasonCode::CollectorAuthTerminal,
            });
        }
        Ok(())
    }

    fn verify_active_published_content_metadata(&self) -> Result<(), StoreError> {
        type PackCatalogueEntry = (String, i64, String, i64, i64);

        let connection = self.open_read_only_connection()?;
        let pack_rows = connection
            .prepare(
                "SELECT sha256, snapshot_id, ordinal, relative_path,
                        compressed_bytes, uncompressed_bytes
                   FROM sync_packs ORDER BY sha256",
            )
            .map_err(StoreError::Query)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        let mut catalogue = HashMap::<String, PackCatalogueEntry>::with_capacity(pack_rows.len());
        for (sha256, snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes) in
            pack_rows
        {
            let digest = sha256
                .parse::<Sha256Digest>()
                .map_err(|_| StoreError::LineageCatalogConflict)?;
            if digest.to_string() != sha256
                || relative_path != TransportPack::canonical_relative_path(digest)
                || ordinal < 0
                || compressed_bytes <= 0
                || uncompressed_bytes <= 0
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            let expected_bytes =
                u64::try_from(compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?;
            let path = self
                .packs_dir
                .join("sha256")
                .join(format!("{sha256}.sqlite.zst"));
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| StoreError::InspectCatalogPack {
                    path: path.clone(),
                    source,
                })?;
            if !metadata.file_type().is_file() {
                return Err(StoreError::CatalogPackNotRegular { path });
            }
            if metadata.len() != expected_bytes {
                return Err(StoreError::CatalogPackSizeMismatch {
                    path,
                    expected: expected_bytes,
                    actual: metadata.len(),
                });
            }
            let entry = (
                snapshot_id,
                ordinal,
                relative_path,
                compressed_bytes,
                uncompressed_bytes,
            );
            if catalogue.insert(sha256, entry).is_some() {
                return Err(StoreError::LineageCatalogConflict);
            }
        }

        let manifest_rows = connection
            .prepare(
                "SELECT snapshot_id, vehicle_id, head_sequence, manifest_json
                   FROM sync_manifests ORDER BY vehicle_id, snapshot_id",
            )
            .map_err(StoreError::Query)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        for (snapshot_id, vehicle_id, head_sequence, payload) in manifest_rows {
            let manifest = decode_manifest(payload)?;
            if manifest.snapshot_id.to_string() != snapshot_id
                || manifest.vehicle_id.to_string() != vehicle_id
                || i64::try_from(manifest.head_sequence).ok() != Some(head_sequence)
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            for pack in &manifest.chunks {
                verify_transport_pack_catalogue_binding(&catalogue, pack)?;
            }
        }

        let lineage_vehicle_ids = connection
            .prepare("SELECT vehicle_id FROM sync_bases ORDER BY vehicle_id")
            .map_err(StoreError::Query)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        drop(connection);
        for vehicle_id in lineage_vehicle_ids {
            let vehicle_id = Uuid::parse_str(&vehicle_id)
                .map_err(|_| StoreError::InvalidStoredUuid("lineage vehicle"))?;
            let lineage = self
                .lineage_manifest_for_vehicle_with_verification(
                    vehicle_id,
                    LineagePackVerification::MetadataOnly,
                )?
                .ok_or(StoreError::LineageCatalogConflict)?;
            for pack in lineage
                .base
                .packs
                .iter()
                .chain(lineage.deltas.iter().map(|delta| &delta.pack))
            {
                verify_transport_pack_catalogue_binding(&catalogue, pack)?;
            }
        }
        Ok(())
    }

    /// Perform the operator-facing integrity gate. Unlike the fast readiness
    /// path, this runs full `PRAGMA quick_check` and hashes every currently
    /// referenced immutable pack.
    pub fn catalogue_check(&self) -> Result<(), StoreError> {
        self.quick_check()?;
        self.readiness_check()?;
        self.verify_referenced_packs()
    }

    /// Bounded inventory for collector/serve startup logging. This avoids
    /// parsing retained manifests and walking pack directories.
    pub fn runtime_inventory(&self) -> Result<RuntimeInventory, StoreError> {
        let connection = self.open_read_only_connection()?;
        let journal_mode = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        let retired_expiry_cutoff_ms = retired_lineage_clock_ms()?;
        let referenced_packs: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM (
                    SELECT sha256 AS digest FROM sync_packs
                    UNION
                    SELECT packs.pack_digest AS digest
                      FROM sync_retired_lineage_packs AS packs
                      JOIN sync_retired_lineages AS lineage
                        ON lineage.vehicle_id = packs.vehicle_id
                       AND lineage.head_digest = packs.head_digest
                     WHERE lineage.expires_at_ms > ?1
                 )",
                params![retired_expiry_cutoff_ms],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        Ok(RuntimeInventory {
            journal_mode,
            vehicles: read_only_count(&connection, "SELECT COUNT(*) FROM vehicles")?,
            raw_observations: read_only_count(
                &connection,
                "SELECT COUNT(*) FROM raw_observations",
            )?,
            quarantined_sessions: read_only_count(
                &connection,
                "SELECT COUNT(*) FROM vehicle_lifecycle_state WHERE quarantined != 0",
            )?,
            referenced_packs: u64::try_from(referenced_packs)
                .map_err(|_| StoreError::InvalidStoredCount)?,
            teslamate_legacy_token_rows: read_only_count(
                &connection,
                "SELECT COUNT(*) FROM teslamate_legacy_tokens",
            )?,
            fleet_token_rows: read_only_count(&connection, "SELECT COUNT(*) FROM fleet_tokens")?,
        })
    }

    /// Full read-only catalogue inventory for `doctor`. This also validates
    /// retained-lineage bindings and inventories physical pack files.
    pub fn catalogue_inventory(&self) -> Result<CatalogueInventory, StoreError> {
        let connection = self.open_read_only_connection()?;
        // SQLite reports `delete` for an `immutable=1` handle even when the
        // persisted database header is in WAL mode. Doctor uses that handle
        // for a byte-stable inspection, so read the header in that case.
        let journal_mode = if self.immutable_snapshot.is_some() {
            persistent_journal_mode(&self.database_path)?
        } else {
            connection
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .map_err(StoreError::Query)?
        };
        let page_size: i64 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        let page_count: i64 = connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        let freelist_count: i64 = connection
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        let foreign_keys: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        let schema_version = schema_version(&connection)?;
        let referenced_pack_rows =
            referenced_pack_rows_at(&connection, retired_lineage_clock_ms()?)?;
        let referenced_packs = u64::try_from(referenced_pack_rows.len())
            .map_err(|_| StoreError::InvalidStoredCount)?;
        let referenced_pack_bytes =
            referenced_pack_rows
                .iter()
                .try_fold(0_u64, |total, (_, _, compressed_bytes)| {
                    let compressed_bytes = u64::try_from(*compressed_bytes)
                        .map_err(|_| StoreError::InvalidStoredCount)?;
                    total
                        .checked_add(compressed_bytes)
                        .ok_or(StoreError::InvalidStoredCount)
                })?;
        let (physical_pack_files, physical_pack_bytes) = physical_pack_inventory(&self.packs_dir)?;
        let page_size_u = u64::try_from(page_size).map_err(|_| StoreError::InvalidStoredCount)?;
        let page_count_u = u64::try_from(page_count).map_err(|_| StoreError::InvalidStoredCount)?;
        let sqlite_page_bytes = page_size_u
            .checked_mul(page_count_u)
            .ok_or(StoreError::InvalidStoredCount)?;
        let wal_path = {
            let mut path = self.database_path.as_os_str().to_os_string();
            path.push("-wal");
            PathBuf::from(path)
        };
        let (wal_present, wal_bytes) = match fs::symlink_metadata(&wal_path) {
            Ok(metadata) if metadata.file_type().is_file() => (true, metadata.len()),
            Ok(_) => (false, 0),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (false, 0),
            Err(error) => return Err(StoreError::InspectCatalogue(error)),
        };
        Ok(CatalogueInventory {
            schema_version,
            journal_mode,
            page_size,
            page_count,
            freelist_count,
            sqlite_page_bytes,
            wal_present,
            wal_bytes,
            synchronous,
            foreign_keys_enabled: foreign_keys != 0,
            vehicles: read_only_count(&connection, "SELECT COUNT(*) FROM vehicles")?,
            raw_observations: read_only_count(
                &connection,
                "SELECT COUNT(*) FROM raw_observations",
            )?,
            current_observations: read_only_count(
                &connection,
                "SELECT COUNT(*) FROM current_observations",
            )?,
            quarantined_sessions: read_only_count(
                &connection,
                "SELECT COUNT(*) FROM vehicle_lifecycle_state WHERE quarantined != 0",
            )?,
            open_lifecycle_rows: read_only_count(
                &connection,
                "SELECT COUNT(*) FROM lifecycle_open_rows",
            )?,
            referenced_packs,
            referenced_pack_bytes,
            physical_pack_files,
            physical_pack_bytes,
            teslamate_legacy_token_rows: read_only_count(
                &connection,
                "SELECT COUNT(*) FROM teslamate_legacy_tokens",
            )?,
            fleet_token_rows: read_only_count(&connection, "SELECT COUNT(*) FROM fleet_tokens")?,
            paired_devices: read_only_count(&connection, "SELECT COUNT(*) FROM paired_devices")?,
            installation_id: self.installation_id()?,
        })
    }

    fn verify_referenced_packs(&self) -> Result<(), StoreError> {
        self.verify_referenced_packs_at(retired_lineage_clock_ms()?)
    }

    fn verify_referenced_packs_at(&self, now_ms: i64) -> Result<(), StoreError> {
        if now_ms < 0 {
            return Err(StoreError::LineageCatalogConflict);
        }
        let connection = self.open_read_only_connection()?;
        let rows = referenced_pack_rows_at(&connection, now_ms)?;

        for (sha256, relative_path, compressed_bytes) in rows {
            let compressed_bytes =
                u64::try_from(compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?;
            if !is_sha256_hex(&sha256)
                || relative_path != format!("/v1/packs/sha256/{sha256}.sqlite.zst")
            {
                return Err(StoreError::UnsafeStoredPackPath);
            }
            let path = self
                .packs_dir
                .join("sha256")
                .join(format!("{sha256}.sqlite.zst"));
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| StoreError::InspectCatalogPack {
                    path: path.clone(),
                    source,
                })?;
            if !metadata.file_type().is_file() {
                return Err(StoreError::CatalogPackNotRegular { path });
            }
            if metadata.len() != compressed_bytes {
                return Err(StoreError::CatalogPackSizeMismatch {
                    path,
                    expected: compressed_bytes,
                    actual: metadata.len(),
                });
            }
            if sha256_file_hex(&path)? != sha256 {
                return Err(StoreError::CatalogPackDigestMismatch { path });
            }
        }
        Ok(())
    }

    pub fn sqlite_version(&self) -> Result<String, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row("SELECT sqlite_version()", [], |row| row.get(0))
            .map_err(StoreError::Query)
    }

    /// Stable random identity of this Hub installation. It never comes from a
    /// remote source and survives package upgrades and restarts.
    pub fn installation_id(&self) -> Result<Uuid, StoreError> {
        let connection = self.open_read_only_connection()?;
        let value: String = connection
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = ?1",
                params![INSTALLATION_ID_KEY],
                |row| row.get(0),
            )
            .map_err(StoreError::InstallationIdentity)?;
        parse_stored_uuid("installation_id", &value)
    }
}
