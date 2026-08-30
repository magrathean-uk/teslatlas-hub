// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    pub fn publish_manifest(&self, manifest: &SyncManifest) -> Result<(), StoreError> {
        if manifest.schema == HUB_PROJECTION_SCHEMA_V3 {
            return Err(StoreError::Schema22PairPublicationRequired(
                manifest.vehicle_id,
            ));
        }
        let _publication_gate = self.try_acquire_publication_gate()?;
        self.publish_manifest_catalogue(manifest)
    }

    fn publish_manifest_catalogue(&self, manifest: &SyncManifest) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        // A schema-2.1 full base is not self-describing enough to recover the
        // selected source car later.  The generic manifest entry point has no
        // immutable ProjectionBinding, so it must never create (or extend) a
        // V2 lineage.  Binding-aware import finalizers pass the exact binding
        // into the transactional helper instead.
        publish_manifest_in_transaction(&transaction, manifest, None)?;
        self.commit_manifest_transaction(transaction, manifest)
    }

    pub(crate) fn publish_schema_22_manifest(
        &self,
        _publication_gate: &PublicationGate,
        manifest: &SyncManifest,
    ) -> Result<(), StoreError> {
        if manifest.schema != HUB_PROJECTION_SCHEMA_V3 {
            return Err(StoreError::Schema22PairPublicationRequired(
                manifest.vehicle_id,
            ));
        }
        let noop_bytes = self
            .schema_22_noop_for_snapshot(manifest.vehicle_id, manifest.snapshot_id)?
            .ok_or(StoreError::Schema22NoOpNotFound)?;
        let noop: crate::updates_delivery::SignedNoOpState = serde_json::from_slice(&noop_bytes)
            .map_err(|error| StoreError::InvalidSchema22Pair(error.to_string()))?;
        let canonical = serde_json::to_vec(&noop)
            .map_err(|error| StoreError::InvalidSchema22Pair(error.to_string()))?;
        if canonical != noop_bytes {
            return Err(StoreError::InvalidSchema22Pair(
                "no-op is not canonical typed JSON".into(),
            ));
        }
        crate::updates_delivery::validate_schema_22_pair(manifest, &noop)
            .map_err(|error| StoreError::InvalidSchema22Pair(error.message))?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let candidate_manifest =
            serde_json::to_vec(manifest).map_err(StoreError::SerializeManifest)?;
        let existing = transaction
            .query_row(
                "SELECT manifest_json FROM sync_manifests WHERE snapshot_id = ?1",
                params![manifest.snapshot_id.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if let Some(stored_manifest) = existing {
            if stored_manifest != candidate_manifest {
                return Err(StoreError::Schema22SnapshotConflict {
                    vehicle_id: manifest.vehicle_id,
                    snapshot_id: manifest.snapshot_id,
                });
            }
            decode_manifest(stored_manifest)?;
        }
        publish_manifest_in_transaction(&transaction, manifest, None)?;
        self.commit_manifest_transaction(transaction, manifest)
    }

    fn commit_manifest_transaction(
        &self,
        transaction: Transaction<'_>,
        manifest: &SyncManifest,
    ) -> Result<(), StoreError> {
        crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::CatalogueBeforeCommit,
        )
        .map_err(StoreError::CatalogueDurability)?;
        let commit = transaction.commit();
        if let Err(source) = commit {
            return match self.manifest_commit_state(manifest)? {
                ManifestCommitState::Exact => Ok(()),
                ManifestCommitState::Absent => Err(StoreError::PublishManifest(source)),
                ManifestCommitState::Conflicting => Err(StoreError::AmbiguousCatalogueCommit {
                    vehicle_id: manifest.vehicle_id,
                    snapshot_id: manifest.snapshot_id,
                }),
            };
        }
        if let Err(source) = crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::CatalogueAfterCommit,
        ) {
            return match self.manifest_commit_state(manifest)? {
                ManifestCommitState::Exact => Ok(()),
                ManifestCommitState::Absent => Err(StoreError::CatalogueDurability(source)),
                ManifestCommitState::Conflicting => Err(StoreError::AmbiguousCatalogueCommit {
                    vehicle_id: manifest.vehicle_id,
                    snapshot_id: manifest.snapshot_id,
                }),
            };
        }
        Ok(())
    }

    fn commit_catalogue_receipted_transaction(
        &self,
        transaction: Transaction<'_>,
        domain: &'static str,
        vehicle_id: Uuid,
        snapshot_id: Uuid,
        commit_error: fn(rusqlite::Error) -> StoreError,
    ) -> Result<(), StoreError> {
        let prior = transaction
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = ?1",
                params![CATALOGUE_COMMIT_RECEIPT_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let operation_id = Uuid::new_v4();
        let mut digest = Sha256::new();
        digest.update(b"teslatlas-hub/catalogue-commit-receipt/v1\0");
        digest.update(domain.as_bytes());
        digest.update([0]);
        digest.update(vehicle_id.as_bytes());
        digest.update(snapshot_id.as_bytes());
        digest.update(operation_id.as_bytes());
        let candidate = format!("v1:{operation_id}:{}", hex::encode(digest.finalize()));
        transaction
            .execute(
                "INSERT INTO hub_metadata(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![CATALOGUE_COMMIT_RECEIPT_KEY, candidate.as_str()],
            )
            .map_err(StoreError::Query)?;

        if let Err(source) = crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::CatalogueBeforeCommit,
        ) {
            drop(transaction);
            return match self.catalogue_commit_receipt_state(&candidate, prior.as_deref())? {
                CatalogueCommitReceiptState::Prior => Err(StoreError::CatalogueDurability(source)),
                CatalogueCommitReceiptState::Exact | CatalogueCommitReceiptState::Conflicting => {
                    Err(StoreError::AmbiguousCatalogueCommit {
                        vehicle_id,
                        snapshot_id,
                    })
                }
            };
        }

        if let Err(source) = transaction.commit() {
            return match self.catalogue_commit_receipt_state(&candidate, prior.as_deref())? {
                CatalogueCommitReceiptState::Exact => Ok(()),
                CatalogueCommitReceiptState::Prior => Err(commit_error(source)),
                CatalogueCommitReceiptState::Conflicting => {
                    Err(StoreError::AmbiguousCatalogueCommit {
                        vehicle_id,
                        snapshot_id,
                    })
                }
            };
        }
        if let Err(_source) = crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::CatalogueAfterCommit,
        ) {
            return match self.catalogue_commit_receipt_state(&candidate, prior.as_deref())? {
                CatalogueCommitReceiptState::Exact => Ok(()),
                CatalogueCommitReceiptState::Prior | CatalogueCommitReceiptState::Conflicting => {
                    Err(StoreError::AmbiguousCatalogueCommit {
                        vehicle_id,
                        snapshot_id,
                    })
                }
            };
        }
        Ok(())
    }

    fn catalogue_commit_receipt_state(
        &self,
        candidate: &str,
        prior: Option<&str>,
    ) -> Result<CatalogueCommitReceiptState, StoreError> {
        let connection = self.open()?;
        let stored = connection
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = ?1",
                params![CATALOGUE_COMMIT_RECEIPT_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if stored.as_deref() == Some(candidate) {
            Ok(CatalogueCommitReceiptState::Exact)
        } else if stored.as_deref() == prior {
            Ok(CatalogueCommitReceiptState::Prior)
        } else {
            Ok(CatalogueCommitReceiptState::Conflicting)
        }
    }

    fn manifest_commit_state(
        &self,
        manifest: &SyncManifest,
    ) -> Result<ManifestCommitState, StoreError> {
        let connection = self.open()?;
        let candidate = serde_json::to_vec(manifest).map_err(StoreError::SerializeManifest)?;
        let stored = connection
            .query_row(
                "SELECT manifest_json FROM sync_manifests WHERE snapshot_id = ?1",
                params![manifest.snapshot_id.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let pack_rows = {
            let mut statement = connection
                .prepare(
                    "SELECT ordinal, sha256, relative_path, compressed_bytes, uncompressed_bytes
                       FROM sync_packs WHERE snapshot_id = ?1 ORDER BY ordinal",
                )
                .map_err(StoreError::Query)?;
            statement
                .query_map(params![manifest.snapshot_id.to_string()], |row| {
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
                .map_err(StoreError::Query)?
        };
        let Some(stored) = stored else {
            return Ok(if pack_rows.is_empty() {
                ManifestCommitState::Absent
            } else {
                ManifestCommitState::Conflicting
            });
        };
        if stored != candidate {
            return Ok(ManifestCommitState::Conflicting);
        }
        let current = connection
            .query_row(
                "SELECT manifest_json FROM sync_manifests
                  WHERE vehicle_id = ?1 ORDER BY head_sequence DESC LIMIT 1",
                params![manifest.vehicle_id.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if current.as_deref() != Some(candidate.as_slice())
            || pack_rows.len() != manifest.chunks.len()
        {
            return Ok(ManifestCommitState::Conflicting);
        }
        for (stored, expected) in pack_rows.iter().zip(&manifest.chunks) {
            let ordinal = i64::from(expected.ordinal);
            let compressed = i64::try_from(expected.compressed_bytes)
                .map_err(|_| StoreError::PackSizeTooLarge)?;
            let uncompressed = i64::try_from(expected.uncompressed_bytes)
                .map_err(|_| StoreError::PackSizeTooLarge)?;
            if stored
                != &(
                    ordinal,
                    expected.sha256.to_string(),
                    expected.relative_path.clone(),
                    compressed,
                    uncompressed,
                )
            {
                return Ok(ManifestCommitState::Conflicting);
            }
        }
        Ok(ManifestCommitState::Exact)
    }

    fn ensure_schema_22_noop_directory(&self) -> Result<(), StoreError> {
        self.open_schema_22_noop_directory(true).map(|_| ())
    }

    fn open_schema_22_noop_directory(
        &self,
        create: bool,
    ) -> Result<SharedSchema22NoOpDirectory, StoreError> {
        let data_root = self
            .database_path
            .parent()
            .ok_or_else(|| StoreError::UnsafeSchema22NoOpPath(self.database_path.clone()))?;
        let data_root_fd = open(
            data_root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        let data_root_stat =
            fstat(&data_root_fd).map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        if !FileType::from_raw_mode(data_root_stat.st_mode).is_dir() {
            return Err(StoreError::UnsafeSchema22NoOpPath(data_root.to_path_buf()));
        }
        let packs_fd = openat(
            &data_root_fd,
            "packs",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        let mut packs_stat =
            fstat(&packs_fd).map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        let packs_path = data_root.join("packs");
        // Projection writers and historical backup roots can create this
        // non-secret directory with the platform's ordinary 0755 default
        // before HubStore opens it. Only its owner may tighten that one known
        // default to the shared setgid contract; peer-owned or novel modes
        // remain package-admission failures.
        if FileType::from_raw_mode(packs_stat.st_mode).is_dir()
            && packs_stat.st_uid == rustix::process::geteuid().as_raw()
            && packs_stat.st_gid == data_root_stat.st_gid
            && stat_mode(packs_stat.st_mode) == 0o755
        {
            fchmod(
                &packs_fd,
                Mode::from_raw_mode(SHARED_DATA_DIRECTORY_MODE as _),
            )
            .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
            File::from(
                packs_fd
                    .try_clone()
                    .map_err(StoreError::AccessSchema22NoOp)?,
            )
            .sync_all()
            .map_err(StoreError::AccessSchema22NoOp)?;
            packs_stat =
                fstat(&packs_fd).map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        }
        if !FileType::from_raw_mode(packs_stat.st_mode).is_dir()
            || packs_stat.st_gid != data_root_stat.st_gid
            || stat_mode(packs_stat.st_mode) != SHARED_DATA_DIRECTORY_MODE
        {
            return Err(StoreError::UnsafeSchema22NoOpPath(packs_path));
        }

        let created = if create {
            match mkdirat(
                &packs_fd,
                "noop",
                Mode::from_raw_mode(SHARED_SCHEMA_22_NOOP_DIRECTORY_MODE as _),
            ) {
                Ok(()) => true,
                Err(Errno::EXIST) => false,
                Err(error) => return Err(StoreError::AccessSchema22NoOp(error.into())),
            }
        } else {
            false
        };
        let noop_path = data_root.join("packs/noop");
        let noop_fd = match openat(
            &packs_fd,
            "noop",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) if !create => {
                return Err(StoreError::Schema22NoOpNotFound);
            }
            Err(error) => return Err(StoreError::AccessSchema22NoOp(error.into())),
        };
        if created {
            fchmod(
                &noop_fd,
                Mode::from_raw_mode(SHARED_SCHEMA_22_NOOP_DIRECTORY_MODE as _),
            )
            .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
            File::from(
                noop_fd
                    .try_clone()
                    .map_err(StoreError::AccessSchema22NoOp)?,
            )
            .sync_all()
            .map_err(StoreError::AccessSchema22NoOp)?;
            File::from(
                packs_fd
                    .try_clone()
                    .map_err(StoreError::AccessSchema22NoOp)?,
            )
            .sync_all()
            .map_err(StoreError::AccessSchema22NoOp)?;
        }
        let noop_stat =
            fstat(&noop_fd).map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        if !FileType::from_raw_mode(noop_stat.st_mode).is_dir()
            || noop_stat.st_gid != packs_stat.st_gid
            || stat_mode(noop_stat.st_mode) != SHARED_SCHEMA_22_NOOP_DIRECTORY_MODE
        {
            return Err(StoreError::UnsafeSchema22NoOpPath(noop_path.clone()));
        }
        Ok(SharedSchema22NoOpDirectory {
            file: File::from(noop_fd),
            gid: noop_stat.st_gid,
            path: noop_path,
        })
    }

    pub(crate) fn prepare_schema_22_noop_publication(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        keep_snapshot_id: Option<Uuid>,
    ) -> Result<(), StoreError> {
        let directory = self.open_schema_22_noop_directory(true)?;
        let prefix = format!("{vehicle_id}.");
        let temporary_prefix = format!(".{vehicle_id}.");
        let mut removed = false;
        let entries = Dir::read_from(&directory.file)
            .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
        for entry in entries {
            let entry = entry.map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
            let Ok(name) = entry.file_name().to_str() else {
                return Err(StoreError::UnsafeSchema22NoOpPath(directory.path.clone()));
            };
            if matches!(name, "." | "..")
                || (!name.starts_with(&prefix) && !name.starts_with(&temporary_prefix))
            {
                continue;
            }
            let path = directory.path.join(name);
            let stat = statat(&directory.file, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
            let final_snapshot = parse_schema_22_noop_filename(name, vehicle_id);
            let valid_final = final_snapshot.is_some()
                && validate_schema_22_noop_file_stat(
                    &stat,
                    directory.gid,
                    SHARED_SCHEMA_22_NOOP_FILE_MODE,
                );
            let valid_temporary = is_schema_22_noop_temporary_filename(name, vehicle_id)
                && (validate_schema_22_noop_file_stat(
                    &stat,
                    directory.gid,
                    PRIVATE_SCHEMA_22_NOOP_STAGING_MODE,
                ) || validate_schema_22_noop_file_stat(
                    &stat,
                    directory.gid,
                    SHARED_SCHEMA_22_NOOP_FILE_MODE,
                ));
            if !valid_final && !valid_temporary {
                return Err(StoreError::UnsafeSchema22NoOpPath(path));
            }
            if final_snapshot == keep_snapshot_id {
                continue;
            }
            unlinkat(&directory.file, name, AtFlags::empty())
                .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
            removed = true;
        }
        if removed {
            directory
                .file
                .sync_all()
                .map_err(StoreError::AccessSchema22NoOp)?;
        }
        Ok(())
    }

    /// Persist a schema-2.2 no-op under its immutable snapshot identity. The
    /// publication gate keeps same-vehicle cleanup and installation ordered.
    pub(crate) fn publish_schema_22_noop(
        &self,
        _publication_gate: &PublicationGate,
        noop: &crate::updates_delivery::SignedNoOpState,
    ) -> Result<(), StoreError> {
        let directory = self.open_schema_22_noop_directory(true)?;
        let final_name = schema_22_noop_filename(noop.vehicle_id, noop.snapshot_id);
        let path = directory.path.join(&final_name);
        let payload = serde_json::to_vec(noop).map_err(StoreError::SerializeManifest)?;
        if u64::try_from(payload.len()).unwrap_or(u64::MAX) > MAX_SCHEMA_22_NOOP_BYTES {
            return Err(StoreError::UnsafeSchema22NoOpPath(path));
        }
        if let Some(existing) =
            self.schema_22_noop_bytes_in_directory(&directory, final_name.as_str())?
        {
            return if existing == payload {
                self.sync_schema_22_noop_directory(&directory)?;
                Ok(())
            } else {
                Err(StoreError::Schema22SnapshotConflict {
                    vehicle_id: noop.vehicle_id,
                    snapshot_id: noop.snapshot_id,
                })
            };
        }
        let temporary_name = format!(
            ".{}.{}.{}.tmp",
            noop.vehicle_id,
            noop.snapshot_id,
            Uuid::new_v4()
        );
        let temporary_path = directory.path.join(&temporary_name);
        let result = (|| {
            let fd = openat(
                &directory.file,
                temporary_name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(PRIVATE_SCHEMA_22_NOOP_STAGING_MODE as _),
            )
            .map_err(|error| StoreError::WriteSchema22NoOp(error.into()))?;
            let mut file = File::from(fd);
            fchmod(
                &file,
                Mode::from_raw_mode(PRIVATE_SCHEMA_22_NOOP_STAGING_MODE as _),
            )
            .map_err(|error| StoreError::WriteSchema22NoOp(error.into()))?;
            let private_stat =
                fstat(&file).map_err(|error| StoreError::WriteSchema22NoOp(error.into()))?;
            if private_stat.st_uid != rustix::process::geteuid().as_raw()
                || !validate_schema_22_noop_file_stat(
                    &private_stat,
                    directory.gid,
                    PRIVATE_SCHEMA_22_NOOP_STAGING_MODE,
                )
            {
                return Err(StoreError::UnsafeSchema22NoOpPath(temporary_path.clone()));
            }
            file.write_all(&payload)
                .map_err(StoreError::WriteSchema22NoOp)?;
            crate::durability_fault::check(
                crate::durability_fault::DurabilityFaultPoint::Schema22NoOpWrite,
            )
            .map_err(StoreError::WriteSchema22NoOp)?;
            crate::durability_fault::check(
                crate::durability_fault::DurabilityFaultPoint::Schema22NoOpFsync,
            )
            .map_err(StoreError::WriteSchema22NoOp)?;
            file.sync_all().map_err(StoreError::WriteSchema22NoOp)?;
            fchmod(
                &file,
                Mode::from_raw_mode(SHARED_SCHEMA_22_NOOP_FILE_MODE as _),
            )
            .map_err(|error| StoreError::WriteSchema22NoOp(error.into()))?;
            let shared_stat =
                fstat(&file).map_err(|error| StoreError::WriteSchema22NoOp(error.into()))?;
            if !validate_schema_22_noop_file_stat(
                &shared_stat,
                directory.gid,
                SHARED_SCHEMA_22_NOOP_FILE_MODE,
            ) {
                return Err(StoreError::UnsafeSchema22NoOpPath(temporary_path.clone()));
            }
            drop(file);
            match renameat_with(
                &directory.file,
                temporary_name.as_str(),
                &directory.file,
                final_name.as_str(),
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {
                    crate::durability_fault::check(
                        crate::durability_fault::DurabilityFaultPoint::Schema22NoOpRename,
                    )
                    .map_err(StoreError::WriteSchema22NoOp)?;
                    self.sync_schema_22_noop_directory(&directory)
                }
                Err(Errno::EXIST) => {
                    unlinkat(&directory.file, temporary_name.as_str(), AtFlags::empty())
                        .map_err(|error| StoreError::AccessSchema22NoOp(error.into()))?;
                    let existing = self
                        .schema_22_noop_bytes_in_directory(&directory, final_name.as_str())?
                        .ok_or(StoreError::Schema22NoOpNotFound)?;
                    if existing == payload {
                        self.sync_schema_22_noop_directory(&directory)?;
                        Ok(())
                    } else {
                        Err(StoreError::Schema22SnapshotConflict {
                            vehicle_id: noop.vehicle_id,
                            snapshot_id: noop.snapshot_id,
                        })
                    }
                }
                Err(error) => Err(StoreError::WriteSchema22NoOp(error.into())),
            }
        })();
        if result.is_err() {
            let _ = unlinkat(&directory.file, temporary_name.as_str(), AtFlags::empty());
        }
        result
    }

    fn sync_schema_22_noop_directory(
        &self,
        directory: &SharedSchema22NoOpDirectory,
    ) -> Result<(), StoreError> {
        crate::durability_fault::check(
            crate::durability_fault::DurabilityFaultPoint::Schema22NoOpDirectoryFsync,
        )
        .map_err(StoreError::WriteSchema22NoOp)?;
        directory
            .file
            .sync_all()
            .map_err(StoreError::WriteSchema22NoOp)
    }

    pub fn schema_22_noop_for_snapshot(
        &self,
        vehicle_id: Uuid,
        snapshot_id: Uuid,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let directory = match self.open_schema_22_noop_directory(false) {
            Ok(directory) => directory,
            Err(StoreError::Schema22NoOpNotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let name = schema_22_noop_filename(vehicle_id, snapshot_id);
        self.schema_22_noop_bytes_in_directory(&directory, name.as_str())
    }

    fn schema_22_noop_bytes_in_directory(
        &self,
        directory: &SharedSchema22NoOpDirectory,
        name: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let path = directory.path.join(name);
        let fd = match openat(
            &directory.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(StoreError::ReadSchema22NoOp(error.into())),
        };
        let mut file = File::from(fd);
        let stat = fstat(&file).map_err(|error| StoreError::ReadSchema22NoOp(error.into()))?;
        if !validate_schema_22_noop_file_stat(&stat, directory.gid, SHARED_SCHEMA_22_NOOP_FILE_MODE)
        {
            return Err(StoreError::UnsafeSchema22NoOpPath(path));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_SCHEMA_22_NOOP_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(StoreError::ReadSchema22NoOp)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SCHEMA_22_NOOP_BYTES {
            return Err(StoreError::UnsafeSchema22NoOpPath(path));
        }
        Ok(Some(bytes))
    }

    pub fn claim_export_outbox(
        &self,
        now_ms: i64,
    ) -> Result<Option<ExportOutboxClaim>, StoreError> {
        validate_timestamp("export outbox now_ms", now_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let row: Option<(String, i64, i64)> = transaction
            .query_row(
                "SELECT vehicle_id, dirty_revision, attempts
                 FROM export_outbox
                 WHERE next_attempt_ms <= ?1 AND claimed_until_ms <= ?1
                 ORDER BY next_attempt_ms, vehicle_id LIMIT 1",
                params![now_ms],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((vehicle, revision, attempts)) = row else {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(None);
        };
        let vehicle_id = Uuid::parse_str(&vehicle)
            .map_err(|_| StoreError::InvalidStoredUuid("export vehicle"))?;
        let lease_until_ms = now_ms.saturating_add(60_000);
        transaction
            .execute(
                "UPDATE export_outbox
                 SET attempts = attempts + 1, claimed_until_ms = ?1
                 WHERE vehicle_id = ?2 AND dirty_revision = ?3",
                params![lease_until_ms, vehicle, revision],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(Some(ExportOutboxClaim {
            vehicle_id,
            dirty_revision: revision,
            attempts: attempts + 1,
            claimed_at_ms: now_ms,
            lease_until_ms,
        }))
    }

    pub fn complete_export_outbox(&self, claim: &ExportOutboxClaim) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let deleted = transaction
            .execute(
                "DELETE FROM export_outbox
                 WHERE vehicle_id = ?1 AND dirty_revision = ?2
                   AND claimed_until_ms = ?3",
                params![
                    claim.vehicle_id.to_string(),
                    claim.dirty_revision,
                    claim.lease_until_ms
                ],
            )
            .map_err(StoreError::Query)?;
        if deleted == 0 {
            // A mutation can advance the revision while this claim is being
            // published. Keep that newer work, but place it behind already
            // eligible vehicles so one busy vehicle cannot monopolise replay.
            transaction
                .execute(
                    "UPDATE export_outbox
                     SET claimed_until_ms = 0, attempts = 0,
                         next_attempt_ms = MAX(next_attempt_ms, ?1), last_error = NULL
                     WHERE vehicle_id = ?2 AND dirty_revision > ?3
                       AND claimed_until_ms = ?4",
                    params![
                        claim.claimed_at_ms.saturating_add(1),
                        claim.vehicle_id.to_string(),
                        claim.dirty_revision,
                        claim.lease_until_ms
                    ],
                )
                .map_err(StoreError::Query)?;
        }
        transaction.commit().map_err(StoreError::Query)
    }

    pub fn fail_export_outbox(
        &self,
        claim: &ExportOutboxClaim,
        error: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("export outbox failure now_ms", now_ms)?;
        let delay = 1_i64
            .checked_shl(claim.attempts.min(16) as u32)
            .unwrap_or(60 * 60)
            .min(60 * 60)
            .saturating_mul(1_000);
        let safe_error = error
            .chars()
            .filter(|character| !character.is_control())
            .take(256)
            .collect::<String>();
        let safe_error = if safe_error.contains("://") {
            "publication_failed".to_owned()
        } else {
            safe_error
        };
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let updated = transaction
            .execute(
                "UPDATE export_outbox
                 SET claimed_until_ms = 0, next_attempt_ms = ?1, last_error = ?2
                 WHERE vehicle_id = ?3 AND dirty_revision = ?4
                   AND claimed_until_ms = ?5",
                params![
                    now_ms.saturating_add(delay),
                    safe_error,
                    claim.vehicle_id.to_string(),
                    claim.dirty_revision,
                    claim.lease_until_ms
                ],
            )
            .map_err(StoreError::Query)?;
        if updated == 0 {
            transaction
                .execute(
                    "UPDATE export_outbox
                     SET claimed_until_ms = 0, attempts = 0,
                         next_attempt_ms = MAX(next_attempt_ms, ?1), last_error = NULL
                     WHERE vehicle_id = ?2 AND dirty_revision > ?3
                       AND claimed_until_ms = ?4",
                    params![
                        claim.claimed_at_ms.saturating_add(1),
                        claim.vehicle_id.to_string(),
                        claim.dirty_revision,
                        claim.lease_until_ms
                    ],
                )
                .map_err(StoreError::Query)?;
        }
        transaction.commit().map_err(StoreError::Query)
    }

    pub fn release_export_outbox(&self, claim: &ExportOutboxClaim) -> Result<(), StoreError> {
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE export_outbox
                 SET claimed_until_ms = 0,
                     attempts = MAX(attempts - 1, 0),
                     next_attempt_ms = CASE WHEN dirty_revision > ?1
                         THEN MAX(next_attempt_ms, ?2) ELSE next_attempt_ms END
                 WHERE vehicle_id = ?3 AND dirty_revision >= ?1
                   AND claimed_until_ms = ?4",
                params![
                    claim.dirty_revision,
                    claim.claimed_at_ms.saturating_add(1),
                    claim.vehicle_id.to_string(),
                    claim.lease_until_ms
                ],
            )
            .map_err(StoreError::Query)?;
        Ok(())
    }

    pub fn vehicle_has_v2_base(&self, vehicle_id: Uuid) -> Result<bool, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_bases WHERE vehicle_id = ?1)",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    pub fn has_unpublished_sync_mutations(&self, vehicle_id: Uuid) -> Result<bool, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sync_mutations
                    WHERE vehicle_id = ?1 AND published = 0
                )",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    pub fn claim_sync_mutations(
        &self,
        vehicle_id: Uuid,
        now_ms: i64,
        maximum: usize,
    ) -> Result<Option<SyncMutationClaim>, StoreError> {
        if vehicle_id.is_nil() || maximum == 0 {
            return Ok(None);
        }
        validate_timestamp("sync mutation claim now_ms", now_ms)?;
        let maximum = maximum.min(10_000);
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let vehicle_key = vehicle_id.to_string();
        let first: Option<(i64, i64)> = transaction
            .query_row(
                "SELECT revision, claimed_until_ms
                 FROM sync_mutations
                 WHERE vehicle_id = ?1 AND published = 0
                 ORDER BY revision LIMIT 1",
                params![vehicle_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((first_revision, claimed_until_ms)) = first else {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(None);
        };
        if claimed_until_ms > now_ms {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(None);
        }
        let mut statement = transaction
            .prepare(
                "SELECT revision, entity, entity_id, car_id, operation, payload_json,
                        claimed_until_ms
                 FROM sync_mutations
                 WHERE vehicle_id = ?1 AND published = 0 AND revision >= ?2
                 ORDER BY revision LIMIT ?3",
            )
            .map_err(StoreError::Query)?;
        let mut rows = statement
            .query(params![
                vehicle_key.as_str(),
                first_revision,
                maximum as i64
            ])
            .map_err(StoreError::Query)?;
        let mut mutations = Vec::with_capacity(maximum);
        while let Some(row) = rows.next().map_err(StoreError::Query)? {
            let claimed_until: i64 = row.get(6).map_err(StoreError::Query)?;
            if claimed_until > now_ms {
                break;
            }
            mutations.push(SyncMutation {
                vehicle_id,
                revision: row.get(0).map_err(StoreError::Query)?,
                entity: row.get(1).map_err(StoreError::Query)?,
                entity_id: row.get(2).map_err(StoreError::Query)?,
                car_id: row.get(3).map_err(StoreError::Query)?,
                operation: row.get(4).map_err(StoreError::Query)?,
                payload_json: row.get(5).map_err(StoreError::Query)?,
            });
        }
        drop(rows);
        drop(statement);
        if mutations.is_empty() {
            transaction.commit().map_err(StoreError::Query)?;
            return Ok(None);
        }
        let first_revision = mutations[0].revision;
        let last_revision = mutations.last().expect("non-empty mutations").revision;
        let lease_until_ms = now_ms.saturating_add(60_000);
        transaction
            .execute(
                "UPDATE sync_mutations
                 SET claimed_until_ms = ?1
                 WHERE vehicle_id = ?2 AND published = 0
                   AND revision BETWEEN ?3 AND ?4",
                params![lease_until_ms, vehicle_key, first_revision, last_revision],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(Some(SyncMutationClaim {
            vehicle_id,
            from_revision: first_revision,
            to_revision: last_revision,
            mutations,
        }))
    }

    pub fn release_sync_mutations(&self, claim: &SyncMutationClaim) -> Result<(), StoreError> {
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE sync_mutations SET claimed_until_ms = 0
                 WHERE vehicle_id = ?1 AND published = 0
                   AND revision BETWEEN ?2 AND ?3",
                params![
                    claim.vehicle_id.to_string(),
                    claim.from_revision,
                    claim.to_revision
                ],
            )
            .map_err(StoreError::Query)?;
        Ok(())
    }

    pub fn v2_head(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<(Uuid, i64, Sha256Digest)>, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT base_snapshot_id, head_sequence, head_digest
                 FROM sync_heads WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| {
                    let snapshot_id: String = row.get(0)?;
                    let digest: String = row.get(2)?;
                    Ok((
                        Uuid::parse_str(&snapshot_id).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        row.get(1)?,
                        digest.parse::<Sha256Digest>().map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::Query)
    }

    pub(crate) fn next_v2_pack_ordinal(&self, snapshot_id: Uuid) -> Result<u32, StoreError> {
        let connection = self.open()?;
        let maximum: Option<i64> = connection
            .query_row(
                "SELECT MAX(ordinal) FROM sync_packs WHERE snapshot_id = ?1",
                params![snapshot_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let next = maximum
            .unwrap_or(-1)
            .checked_add(1)
            .ok_or(StoreError::PackOrdinalTooLarge)?;
        u32::try_from(next).map_err(|_| StoreError::PackOrdinalTooLarge)
    }

    pub fn v2_projection_binding(&self, vehicle_id: Uuid) -> Result<ProjectionBinding, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open_read_only_connection()?;
        let immutable: Option<(String, String, String, i64, i64)> = connection
            .query_row(
                "SELECT binding.snapshot_id, binding.installation_id,
                        binding.account_id, binding.generation, binding.selected_car_id
                   FROM sync_bases AS base
                   JOIN v2_base_bindings AS binding
                     ON binding.vehicle_id = base.vehicle_id
                    AND binding.snapshot_id = base.snapshot_id
                  WHERE base.vehicle_id = ?1",
                params![vehicle_id.to_string()],
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
        if let Some((snapshot_id, installation_id, account_id, generation, selected_car_id)) =
            immutable
        {
            let base_snapshot: Uuid = snapshot_id
                .parse()
                .map_err(|_| StoreError::LineageCatalogConflict)?;
            if base_snapshot.is_nil() || selected_car_id <= 0 {
                return Err(StoreError::LineageCatalogConflict);
            }
            return Ok(ProjectionBinding {
                installation_id: installation_id
                    .parse()
                    .map_err(|_| StoreError::LineageCatalogConflict)?,
                account_id: account_id
                    .parse()
                    .map_err(|_| StoreError::LineageCatalogConflict)?,
                vehicle_id,
                generation: u64::try_from(generation)
                    .ok()
                    .filter(|generation| *generation > 0)
                    .ok_or(StoreError::LineageCatalogConflict)?,
                selected_car_id,
            });
        }
        let has_v2_base: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_bases WHERE vehicle_id = ?1)",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LineageCatalog)?;
        if has_v2_base {
            // A base created before immutable binding persistence cannot be
            // reconstructed from mutable source ownership, aliases, or
            // settings.  Refuse a successor rather than retargeting it.
            return Err(StoreError::ImmutableBaseBindingMissing(vehicle_id));
        }

        let installation_id = connection
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = ?1",
                params![INSTALLATION_ID_KEY],
                |row| row.get::<_, String>(0),
            )
            .map_err(StoreError::InstallationIdentity)?
            .parse()
            .map_err(|_| StoreError::InvalidStoredUuid("installation_id"))?;
        let (source_id, generation, source_key, selected_car_id, materialised_car_id): (
            String,
            i64,
            String,
            Option<i64>,
            Option<i64>,
        ) = connection
            .query_row(
                "SELECT v.source_id, s.generation, v.source_vehicle_key,
                            (SELECT car_id FROM car_settings WHERE vehicle_id = v.vehicle_id),
                            (SELECT car_id FROM materialised_cars WHERE vehicle_id = v.vehicle_id)
                     FROM vehicles v JOIN sources s ON s.source_id = v.source_id
                     WHERE v.vehicle_id = ?1",
                params![vehicle_id.to_string()],
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
            .map_err(StoreError::Query)?;
        let account_id = Uuid::parse_str(&source_id).map_err(|_| StoreError::InvalidSourceId)?;
        let generation =
            u64::try_from(generation).map_err(|_| StoreError::InvalidStoredSequence)?;
        let selected_car_id = selected_car_id
            .or(materialised_car_id)
            .or_else(|| {
                source_key
                    .strip_prefix("eid:")
                    .and_then(|value| value.parse().ok())
            })
            .or_else(|| {
                source_key
                    .strip_prefix("vid:")
                    .and_then(|value| value.parse().ok())
            })
            .or_else(|| source_key.parse().ok())
            .ok_or(StoreError::InvalidLifecycleCarId)?;
        Ok(ProjectionBinding {
            installation_id,
            account_id,
            vehicle_id,
            generation,
            selected_car_id,
        })
    }
}
