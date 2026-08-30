// SPDX-License-Identifier: AGPL-3.0-only

impl HubStore {
    /// Prepare one single-use pairing challenge without writing it. The CLI
    /// uses this phase to finish its QR/JSON presentation before activation.
    pub fn prepare_pairing(
        &self,
        label: &str,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<PairingInvitation, StoreError> {
        validate_identity("pairing label", label, MAX_PAIRING_LABEL_BYTES)?;
        validate_timestamp("pairing created_at_ms", created_at_ms)?;
        if expires_at_ms <= created_at_ms {
            return Err(StoreError::InvalidPairingExpiry);
        }

        let pairing_id = Uuid::new_v4();
        let secret = PairingSecret::generate()?;
        Ok(PairingInvitation {
            pairing_id,
            secret,
            created_at_ms,
            expires_at_ms,
        })
    }

    /// Persist a fully prepared invitation immediately before its local
    /// presentation. Only the secret digest crosses this boundary.
    pub fn persist_pairing(
        &self,
        label: &str,
        invitation: &PairingInvitation,
    ) -> Result<(), StoreError> {
        validate_identity("pairing label", label, MAX_PAIRING_LABEL_BYTES)?;
        validate_timestamp("pairing created_at_ms", invitation.created_at_ms)?;
        validate_timestamp("pairing expires_at_ms", invitation.expires_at_ms)?;
        if invitation.expires_at_ms <= invitation.created_at_ms {
            return Err(StoreError::InvalidPairingExpiry);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "INSERT INTO pairing_challenges \
                 (pairing_id, label, secret_sha256, created_at_ms, expires_at_ms) \
                VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    invitation.pairing_id.to_string(),
                    label,
                    invitation.secret.digest().as_slice(),
                    invitation.created_at_ms,
                    invitation.expires_at_ms,
                ],
            )
            .map_err(StoreError::CreatePairing)?;
        transaction.commit().map_err(StoreError::CreatePairing)?;
        Ok(())
    }

    /// Create and immediately persist an invitation for non-interactive
    /// callers that do not have a presentation boundary.
    pub fn create_pairing(
        &self,
        label: &str,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<PairingInvitation, StoreError> {
        let invitation = self.prepare_pairing(label, created_at_ms, expires_at_ms)?;
        self.persist_pairing(label, &invitation)?;
        Ok(invitation)
    }

    /// Revoke one invitation. Deleting a missing row is deliberately success,
    /// making cleanup safe to retry after an uncertain terminal write.
    pub fn revoke_pairing(&self, pairing_id: Uuid) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "DELETE FROM pairing_challenges WHERE pairing_id = ?1",
                params![pairing_id.to_string()],
            )
            .map_err(StoreError::RevokePairing)?;
        transaction.commit().map_err(StoreError::RevokePairing)?;
        Ok(())
    }

    /// Consume one valid pairing challenge and return the device bearer token.
    /// A failed or expired claim deliberately has one opaque outcome; callers
    /// cannot learn whether a challenge existed, expired, or had a bad secret.
    pub fn claim_pairing(
        &self,
        pairing_id: Uuid,
        secret: &str,
        device_name: &str,
        claimed_at_ms: i64,
    ) -> Result<PairedDeviceAccess, StoreError> {
        validate_identity("paired device name", device_name, MAX_DEVICE_NAME_BYTES)?;
        validate_timestamp("pairing claimed_at_ms", claimed_at_ms)?;
        let Some(secret_digest) = PairingSecret::digest_from_wire(secret) else {
            return Err(StoreError::PairingRejected);
        };

        // Reject random network proofs without ever joining the SQLite writer
        // queue. The immediate transaction below repeats this check before it
        // consumes the challenge, so a concurrent claim remains single-use.
        let read_only = self.open_read_only_connection()?;
        let challenge: Option<(Vec<u8>, i64)> = read_only
            .query_row(
                "SELECT secret_sha256, expires_at_ms FROM pairing_challenges WHERE pairing_id = ?1",
                params![pairing_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::ClaimPairing)?;
        let Some((stored_digest, expires_at_ms)) = challenge else {
            return Err(StoreError::PairingRejected);
        };
        let valid_digest: [u8; PAIRING_SECRET_BYTES] = stored_digest
            .try_into()
            .map_err(|_| StoreError::PairingRejected)?;
        if claimed_at_ms >= expires_at_ms || !constant_time_equal(&valid_digest, &secret_digest) {
            return Err(StoreError::PairingRejected);
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let challenge: Option<(Vec<u8>, i64)> = transaction
            .query_row(
                "SELECT secret_sha256, expires_at_ms FROM pairing_challenges WHERE pairing_id = ?1",
                params![pairing_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::ClaimPairing)?;
        let Some((stored_digest, expires_at_ms)) = challenge else {
            return Err(StoreError::PairingRejected);
        };
        let valid_digest: [u8; PAIRING_SECRET_BYTES] = stored_digest
            .try_into()
            .map_err(|_| StoreError::PairingRejected)?;
        if claimed_at_ms >= expires_at_ms || !constant_time_equal(&valid_digest, &secret_digest) {
            return Err(StoreError::PairingRejected);
        }

        let device_id = Uuid::new_v4();
        let access_token = DeviceAccessToken::generate()?;
        let expires_at_ms = claimed_at_ms.saturating_add(PAIRED_DEVICE_TOKEN_LIFETIME_MS);
        transaction
            .execute(
                "INSERT INTO paired_devices \
                 (device_id, display_name, token_sha256, created_at_ms, expires_at_ms, revoked_at_ms, last_authenticated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL)",
                params![
                    device_id.to_string(),
                    device_name,
                    access_token.digest().as_slice(),
                    claimed_at_ms,
                    expires_at_ms,
                ],
            )
            .map_err(StoreError::ClaimPairing)?;
        // Delete rather than mark claimed: raw pairing material and its digest
        // have no value once a device token exists.
        transaction
            .execute(
                "DELETE FROM pairing_challenges WHERE pairing_id = ?1",
                params![pairing_id.to_string()],
            )
            .map_err(StoreError::ClaimPairing)?;
        transaction.commit().map_err(StoreError::ClaimPairing)?;
        Ok(PairedDeviceAccess {
            device_id,
            access_token,
            expires_at_ms,
        })
    }

    /// Authenticate an already-paired device without logging or retaining the
    /// presented bearer value. The caller can use the returned public device
    /// identity for authorization decisions.
    pub fn authenticate_device(
        &self,
        access_token: &str,
    ) -> Result<Option<PairedDeviceRecord>, StoreError> {
        let now_ms = retired_lineage_clock_ms()?;
        self.authenticate_device_at(access_token, now_ms)
    }

    pub fn authenticate_device_at(
        &self,
        access_token: &str,
        now_ms: i64,
    ) -> Result<Option<PairedDeviceRecord>, StoreError> {
        let Some(token_digest) = DeviceAccessToken::digest_from_wire(access_token) else {
            return Ok(None);
        };
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT device_id, display_name, created_at_ms, expires_at_ms, revoked_at_ms, last_authenticated_at_ms \
                 FROM paired_devices
                  WHERE token_sha256 = ?1 AND revoked_at_ms IS NULL AND expires_at_ms > ?2",
                params![token_digest.as_slice(), now_ms],
                paired_device_from_row,
            )
            .optional()
            .map_err(StoreError::Query)
    }

    pub fn rotate_device(
        &self,
        access_token: &str,
        now_ms: i64,
    ) -> Result<PairedDeviceAccess, StoreError> {
        let Some(token_digest) = DeviceAccessToken::digest_from_wire(access_token) else {
            return Err(StoreError::PairingRejected);
        };
        // As with pairing claims, an invalid bearer must not acquire a write
        // lock. The mutation transaction repeats the lookup atomically.
        let read_only = self.open_read_only_connection()?;
        let plausible: bool = read_only
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM paired_devices
                     WHERE token_sha256 = ?1 AND revoked_at_ms IS NULL AND expires_at_ms > ?2
                 )",
                params![token_digest.as_slice(), now_ms],
                |row| row.get(0),
            )
            .map_err(StoreError::RotateDevice)?;
        if !plausible {
            return Err(StoreError::PairingRejected);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let device: Option<(Uuid, String)> = transaction
            .query_row(
                "SELECT device_id, display_name FROM paired_devices
                 WHERE token_sha256 = ?1 AND revoked_at_ms IS NULL AND expires_at_ms > ?2",
                params![token_digest.as_slice(), now_ms],
                |row| {
                    let id: String = row.get(0)?;
                    let id = Uuid::parse_str(&id).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok((id, row.get(1)?))
                },
            )
            .optional()
            .map_err(StoreError::RotateDevice)?;
        let Some((device_id, _display_name)) = device else {
            return Err(StoreError::PairingRejected);
        };
        let replacement = DeviceAccessToken::generate()?;
        let expires_at_ms = now_ms.saturating_add(PAIRED_DEVICE_TOKEN_LIFETIME_MS);
        let changed = transaction
            .execute(
                "UPDATE paired_devices
                 SET token_sha256 = ?1, expires_at_ms = ?2, last_authenticated_at_ms = ?3
                 WHERE device_id = ?4 AND token_sha256 = ?5
                   AND revoked_at_ms IS NULL AND expires_at_ms > ?3",
                params![
                    replacement.digest().as_slice(),
                    expires_at_ms,
                    now_ms,
                    device_id.to_string(),
                    token_digest.as_slice(),
                ],
            )
            .map_err(StoreError::RotateDevice)?;
        if changed != 1 {
            return Err(StoreError::PairingRejected);
        }
        transaction.commit().map_err(StoreError::RotateDevice)?;
        Ok(PairedDeviceAccess {
            device_id,
            access_token: replacement,
            expires_at_ms,
        })
    }

    pub fn list_paired_devices(&self) -> Result<Vec<PairedDeviceRecord>, StoreError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT device_id, display_name, created_at_ms, expires_at_ms, revoked_at_ms, last_authenticated_at_ms
                 FROM paired_devices ORDER BY created_at_ms ASC, device_id ASC",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map([], paired_device_from_row)
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)
    }

    pub fn revoke_device(&self, device_id: Uuid) -> Result<(), StoreError> {
        self.revoke_device_at(device_id, retired_lineage_clock_ms()?)
    }

    pub fn revoke_device_at(&self, device_id: Uuid, revoked_at_ms: i64) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let changed = transaction
            .execute(
                "UPDATE paired_devices SET revoked_at_ms = COALESCE(revoked_at_ms, ?1)
                 WHERE device_id = ?2",
                params![revoked_at_ms, device_id.to_string()],
            )
            .map_err(StoreError::RevokeDevice)?;
        if changed != 1 {
            return Err(StoreError::PairingRejected);
        }
        transaction.commit().map_err(StoreError::RevokeDevice)
    }

    /// Return the vehicles this Hub has published. Pairing currently grants a
    /// device access to this one owner-controlled Hub, not to arbitrary source
    /// databases or credentials.
    pub fn published_vehicles(&self) -> Result<Vec<PublishedVehicle>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT vehicle_id, display_name FROM vehicles \
                 WHERE EXISTS (SELECT 1 FROM sync_manifests \
                               WHERE sync_manifests.vehicle_id = vehicles.vehicle_id) \
                 ORDER BY last_seen_at_ms DESC, vehicle_id ASC",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map([], |row| {
                let value: String = row.get(0)?;
                let vehicle_id = Uuid::parse_str(&value).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok(PublishedVehicle {
                    vehicle_id,
                    display_name: row.get(1)?,
                })
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)
    }

    /// Return the stable Hub identity for a collector source, creating it the
    /// first time the caller presents this non-secret identity pair.
    ///
    /// `source_key` is an opaque stable identifier such as an account or
    /// migration installation id. It must never be a bearer token, URL with a
    /// password, or other secret.
    pub(crate) fn provision_teslamate_import_identity(
        &self,
        source: &SourceRecord,
        source_created: bool,
        identity_hint: &VehicleDescriptor,
        registered_at_ms: i64,
        expected_vehicle_id: Uuid,
    ) -> Result<(VehicleRecord, TeslaMateIdentityRegistrationCheckpoint), StoreError> {
        identity_hint.validate()?;
        if identity_hint.source_id != source.source_id {
            return Err(StoreError::VehicleIdentityConflict);
        }
        validate_timestamp("vehicle registered_at_ms", registered_at_ms)?;
        if expected_vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        ensure_source_exists(&transaction, source.source_id)?;
        let source_vehicle = find_vehicle(
            &transaction,
            source.source_id,
            &identity_hint.source_vehicle_key,
        )?;
        let identity_vehicle = find_identity_vehicle(&transaction, identity_hint)?
            .map(|vehicle_id| find_vehicle_by_id(&transaction, vehicle_id))
            .transpose()?
            .flatten();
        let (vehicle, vehicle_created) = match (source_vehicle, identity_vehicle) {
            (Some(source_vehicle), Some(identity_vehicle))
                if source_vehicle.vehicle_id == identity_vehicle.vehicle_id =>
            {
                (source_vehicle, false)
            }
            (Some(source_vehicle), Some(identity_vehicle)) => {
                // A prior crash may leave this TeslaMate-owned row before the
                // exported snapshot proves VIN/EID/VID. It has no aliases and
                // is never published. If a collector has since registered the
                // real identity, remove only the exact untouched placeholder
                // and bind this import to the collector-owned vehicle.
                let has_aliases: bool = transaction
                    .query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM vehicle_identity_aliases WHERE vehicle_id = ?1)",
                        params![source_vehicle.vehicle_id.to_string()],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::Query)?;
                if source_vehicle.vin.is_some()
                    || source_vehicle.display_name.is_some()
                    || has_aliases
                {
                    return Err(StoreError::VehicleIdentityConflict);
                }
                let deleted = transaction
                    .execute(
                        "DELETE FROM vehicles
                          WHERE vehicle_id = ?1 AND source_id = ?2 AND source_vehicle_key = ?3
                            AND vin IS NULL AND display_name IS NULL
                            AND created_at_ms = ?4 AND last_seen_at_ms = ?5",
                        params![
                            source_vehicle.vehicle_id.to_string(),
                            source_vehicle.source_id.to_string(),
                            source_vehicle.source_vehicle_key,
                            source_vehicle.created_at_ms,
                            source_vehicle.last_seen_at_ms,
                        ],
                    )
                    .map_err(StoreError::RegisterVehicle)?;
                if deleted != 1 {
                    return Err(StoreError::VehicleIdentityConflict);
                }
                (identity_vehicle, false)
            }
            (Some(source_vehicle), None) => {
                if source_vehicle.vehicle_id != expected_vehicle_id {
                    return Err(StoreError::VehicleIdentityMismatch {
                        expected: expected_vehicle_id,
                        actual: source_vehicle.vehicle_id,
                    });
                }
                (source_vehicle, false)
            }
            (None, Some(identity_vehicle)) => (identity_vehicle, false),
            (None, None) => {
                transaction
                    .execute(
                        "INSERT INTO vehicles
                            (vehicle_id, source_id, source_vehicle_key, vin, display_name,
                             created_at_ms, last_seen_at_ms)
                         VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?4)",
                        params![
                            expected_vehicle_id.to_string(),
                            source.source_id.to_string(),
                            identity_hint.source_vehicle_key,
                            registered_at_ms,
                        ],
                    )
                    .map_err(StoreError::RegisterVehicle)?;
                (
                    VehicleRecord {
                        vehicle_id: expected_vehicle_id,
                        source_id: source.source_id,
                        source_vehicle_key: identity_hint.source_vehicle_key.clone(),
                        vin: None,
                        display_name: None,
                        created_at_ms: registered_at_ms,
                        last_seen_at_ms: registered_at_ms,
                    },
                    true,
                )
            }
        };
        transaction.commit().map_err(StoreError::RegisterVehicle)?;
        let checkpoint = TeslaMateIdentityRegistrationCheckpoint {
            source: source.clone(),
            source_created,
            vehicle: vehicle.clone(),
            vehicle_created,
        };
        Ok((vehicle, checkpoint))
    }

    pub(crate) fn rollback_teslamate_identity_registration(
        &self,
        checkpoint: &TeslaMateIdentityRegistrationCheckpoint,
    ) -> Result<(), StoreError> {
        if checkpoint.vehicle.vehicle_id.is_nil() || checkpoint.source.source_id.is_nil() {
            return Err(StoreError::InvalidVehicleIdentity);
        }
        if !checkpoint.vehicle_created && !checkpoint.source_created {
            return Ok(());
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if checkpoint.vehicle_created {
            let current = find_vehicle_by_id(&transaction, checkpoint.vehicle.vehicle_id)?;
            let has_aliases: bool = transaction
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM vehicle_identity_aliases WHERE vehicle_id = ?1)",
                    params![checkpoint.vehicle.vehicle_id.to_string()],
                    |row| row.get(0),
                )
                .map_err(StoreError::Query)?;
            if current.as_ref() != Some(&checkpoint.vehicle) || has_aliases {
                return Err(StoreError::VehicleIdentityConflict);
            }
            let deleted = transaction
                .execute(
                    "DELETE FROM vehicles WHERE vehicle_id = ?1",
                    params![checkpoint.vehicle.vehicle_id.to_string()],
                )
                .map_err(StoreError::RegisterVehicle)?;
            if deleted != 1 {
                return Err(StoreError::VehicleIdentityConflict);
            }
        }
        if checkpoint.source_created {
            let descriptor = SourceDescriptor::new(
                checkpoint.source.kind.clone(),
                checkpoint.source.key.clone(),
            );
            if find_source(&transaction, &descriptor)?.as_ref() != Some(&checkpoint.source) {
                return Err(StoreError::VehicleIdentityConflict);
            }
            let deleted_identity = transaction
                .execute(
                    "DELETE FROM source_identities
                      WHERE source_id = ?1 AND source_kind = ?2 AND source_key = ?3",
                    params![
                        checkpoint.source.source_id.to_string(),
                        checkpoint.source.kind,
                        checkpoint.source.key,
                    ],
                )
                .map_err(StoreError::RegisterSource)?;
            if deleted_identity != 1 {
                return Err(StoreError::InvalidSourceId);
            }
            let deleted = transaction
                .execute(
                    "DELETE FROM sources
                      WHERE source_id = ?1 AND source_kind = ?2 AND generation = ?3
                        AND created_at_ms = ?4",
                    params![
                        checkpoint.source.source_id.to_string(),
                        checkpoint.source.kind,
                        i64::try_from(checkpoint.source.generation)
                            .map_err(|_| StoreError::InvalidStoredGeneration)?,
                        checkpoint.source.created_at_ms,
                    ],
                )
                .map_err(StoreError::RegisterSource)?;
            if deleted != 1 {
                return Err(StoreError::InvalidSourceId);
            }
        }
        transaction.commit().map_err(StoreError::RegisterVehicle)
    }

    pub(crate) fn register_teslamate_import_source(
        &self,
        descriptor: &SourceDescriptor,
        created_at_ms: i64,
    ) -> Result<(SourceRecord, bool), StoreError> {
        self.register_source_with_creation_state(descriptor, created_at_ms)
    }

    pub fn register_source(
        &self,
        descriptor: &SourceDescriptor,
        created_at_ms: i64,
    ) -> Result<SourceRecord, StoreError> {
        self.register_source_with_creation_state(descriptor, created_at_ms)
            .map(|(source, _)| source)
    }

    fn register_source_with_creation_state(
        &self,
        descriptor: &SourceDescriptor,
        created_at_ms: i64,
    ) -> Result<(SourceRecord, bool), StoreError> {
        descriptor.validate()?;
        validate_timestamp("source created_at_ms", created_at_ms)?;

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if let Some(source) = find_source(&transaction, descriptor)? {
            transaction.commit().map_err(StoreError::RegisterSource)?;
            return Ok((source, false));
        }

        let source_id = Uuid::new_v4();
        transaction
            .execute(
                "INSERT INTO sources (source_id, source_kind, generation, created_at_ms) \
                 VALUES (?1, ?2, 1, ?3)",
                params![source_id.to_string(), descriptor.kind, created_at_ms,],
            )
            .map_err(StoreError::RegisterSource)?;
        transaction
            .execute(
                "INSERT INTO source_identities (source_id, source_kind, source_key) \
                 VALUES (?1, ?2, ?3)",
                params![source_id.to_string(), descriptor.kind, descriptor.key],
            )
            .map_err(StoreError::RegisterSource)?;
        transaction.commit().map_err(StoreError::RegisterSource)?;

        Ok((
            SourceRecord {
                source_id,
                kind: descriptor.kind.clone(),
                key: descriptor.key.clone(),
                generation: 1,
                created_at_ms,
            },
            true,
        ))
    }

    /// Return the stable Hub vehicle identity for one source-owned vehicle.
    /// Re-registering the same source key only refreshes non-identity display
    /// metadata; it can never create a second local vehicle id.
    pub fn register_vehicle(
        &self,
        descriptor: &VehicleDescriptor,
        registered_at_ms: i64,
    ) -> Result<VehicleRecord, StoreError> {
        self.register_vehicle_internal(descriptor, registered_at_ms, None)
    }

    /// Register one source-owned vehicle with an expected stable UUID. This
    /// is for non-Fleet sources such as TeslaMate, where the source identity
    /// and VIN/EID deterministically define the app-facing vehicle identity.
    pub fn register_vehicle_with_id(
        &self,
        descriptor: &VehicleDescriptor,
        registered_at_ms: i64,
        vehicle_id: Uuid,
    ) -> Result<VehicleRecord, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        self.register_vehicle_internal(descriptor, registered_at_ms, Some(vehicle_id))
    }

    fn register_vehicle_internal(
        &self,
        descriptor: &VehicleDescriptor,
        registered_at_ms: i64,
        expected_vehicle_id: Option<Uuid>,
    ) -> Result<VehicleRecord, StoreError> {
        descriptor.validate()?;
        validate_timestamp("vehicle registered_at_ms", registered_at_ms)?;

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        ensure_source_exists(&transaction, descriptor.source_id)?;

        let source_vehicle = find_vehicle(
            &transaction,
            descriptor.source_id,
            &descriptor.source_vehicle_key,
        )?;
        let had_source_vehicle = source_vehicle.is_some();
        let identity_vehicle = find_identity_vehicle(&transaction, descriptor)?;
        let identity_record = match identity_vehicle {
            Some(vehicle_id) => find_vehicle_by_id(&transaction, vehicle_id)?,
            None => None,
        };
        if source_vehicle.is_some()
            && identity_vehicle.is_some()
            && source_vehicle.as_ref().map(|v| v.vehicle_id) != identity_vehicle
        {
            return Err(StoreError::VehicleIdentityConflict);
        }
        if let Some(mut vehicle) = source_vehicle.or(identity_record) {
            if let Some(vin) = &descriptor.vin
                && let Some(existing) = vehicle.vin.as_ref()
                && existing != vin
            {
                return Err(StoreError::VehicleIdentityConflict);
            }
            if had_source_vehicle
                && let Some(expected) = expected_vehicle_id
                && expected != vehicle.vehicle_id
            {
                return Err(StoreError::VehicleIdentityMismatch {
                    expected,
                    actual: vehicle.vehicle_id,
                });
            }
            transaction
                .execute(
                    "UPDATE vehicles \
                     SET vin = COALESCE(?1, vin), \
                         display_name = COALESCE(?2, display_name), \
                         last_seen_at_ms = MAX(last_seen_at_ms, ?3) \
                     WHERE vehicle_id = ?4",
                    params![
                        descriptor.vin,
                        descriptor.display_name,
                        registered_at_ms,
                        vehicle.vehicle_id.to_string(),
                    ],
                )
                .map_err(StoreError::RegisterVehicle)?;
            register_vehicle_aliases(&transaction, vehicle.vehicle_id, descriptor)?;
            vehicle.source_id = descriptor.source_id;
            vehicle.source_vehicle_key = descriptor.source_vehicle_key.clone();
            vehicle.vin = descriptor.vin.clone().or(vehicle.vin);
            vehicle.display_name = descriptor.display_name.clone().or(vehicle.display_name);
            vehicle.last_seen_at_ms = vehicle.last_seen_at_ms.max(registered_at_ms);
            transaction.commit().map_err(StoreError::RegisterVehicle)?;
            return Ok(vehicle);
        }

        let vehicle_id = expected_vehicle_id.unwrap_or_else(Uuid::new_v4);
        transaction
            .execute(
                "INSERT INTO vehicles \
                 (vehicle_id, source_id, source_vehicle_key, vin, display_name, created_at_ms, last_seen_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![
                    vehicle_id.to_string(),
                    descriptor.source_id.to_string(),
                    descriptor.source_vehicle_key,
                    descriptor.vin,
                    descriptor.display_name,
                    registered_at_ms,
                ],
            )
            .map_err(StoreError::RegisterVehicle)?;
        let vehicle = VehicleRecord {
            vehicle_id,
            source_id: descriptor.source_id,
            source_vehicle_key: descriptor.source_vehicle_key.clone(),
            vin: descriptor.vin.clone(),
            display_name: descriptor.display_name.clone(),
            created_at_ms: registered_at_ms,
            last_seen_at_ms: registered_at_ms,
        };
        register_vehicle_aliases(&transaction, vehicle.vehicle_id, descriptor)?;
        transaction.commit().map_err(StoreError::RegisterVehicle)?;

        Ok(vehicle)
    }

    pub fn cached_address(
        &self,
        point: crate::location::Wgs84Point,
    ) -> Result<Option<AddressCacheRecord>, StoreError> {
        let key = address_lookup_key(point);
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT a.osm_type, a.osm_id, a.display_name, a.name,
                        a.latitude, a.longitude, a.house_number, a.road,
                        a.neighbourhood, a.city, a.county, a.postcode,
                        a.state, a.state_district, a.country, a.raw_json,
                        l.latitude, l.longitude, l.looked_up_at_ms
                 FROM address_lookup_cache l
                 JOIN address_cache a
                   ON a.osm_type = l.osm_type AND a.osm_id = l.osm_id
                 WHERE l.lookup_key = ?1",
                params![key],
                |row| {
                    Ok(AddressCacheRecord {
                        osm_type: row.get(0)?,
                        osm_id: row.get(1)?,
                        display_name: row.get(2)?,
                        name: row.get(3)?,
                        latitude: row.get(4)?,
                        longitude: row.get(5)?,
                        house_number: row.get(6)?,
                        road: row.get(7)?,
                        neighbourhood: row.get(8)?,
                        city: row.get(9)?,
                        county: row.get(10)?,
                        postcode: row.get(11)?,
                        state: row.get(12)?,
                        state_district: row.get(13)?,
                        country: row.get(14)?,
                        raw_json: row.get(15)?,
                        lookup_latitude: row.get(16)?,
                        lookup_longitude: row.get(17)?,
                        looked_up_at_ms: row.get(18)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Query)
    }

    pub fn source_vehicle_key(&self, vehicle_id: Uuid) -> Result<Option<String>, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT COALESCE(
                    (SELECT source_vehicle_key FROM vehicle_identity_aliases a
                     JOIN sources s ON s.source_id = a.source_id
                     WHERE a.vehicle_id = ?1 AND s.source_kind = 'owner_api_compat'
                     ORDER BY a.alias_kind = 'tesla_eid' DESC LIMIT 1),
                    (SELECT source_vehicle_key FROM vehicles WHERE vehicle_id = ?1)
                )",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)
    }

    /// Capture the latest durable raw observation for one source car without
    /// reading or returning its payload. The source-car mapping is accepted
    /// only when it resolves to exactly one Hub vehicle.
    pub fn observation_watermark(
        &self,
        source_car_id: i64,
    ) -> Result<ObservationWatermark, ObservationVerificationError> {
        let target = self.resolve_observation_target(source_car_id)?;
        self.observation_watermark_for_target(source_car_id, target)
    }

    /// Capture the latest durable observation for an exact Hub vehicle. This
    /// avoids ambiguity when separate imported vehicles reuse the same
    /// pack-local TeslaMate car id.
    pub fn observation_watermark_for_vehicle(
        &self,
        vehicle_id: Uuid,
        source_car_id: i64,
    ) -> Result<ObservationWatermark, ObservationVerificationError> {
        require_positive_db(source_car_id, "source car id")
            .map_err(|_| ObservationVerificationError::InvalidSourceCarId)?;
        let connection = self.open_read_only_connection()?;
        let source_id = connection
            .query_row(
                "SELECT source_id FROM vehicles WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)?
            .map(|source_id| parse_stored_uuid("observation source", &source_id))
            .transpose()?
            .ok_or(ObservationVerificationError::NoVehicleMapping)?;
        self.observation_watermark_for_target(
            source_car_id,
            ObservationTarget {
                vehicle_id,
                source_id,
            },
        )
    }

    fn observation_watermark_for_target(
        &self,
        source_car_id: i64,
        target: ObservationTarget,
    ) -> Result<ObservationWatermark, ObservationVerificationError> {
        let connection = self.open_read_only_connection()?;
        let latest = latest_observation_metadata(&connection, target.vehicle_id, None)?;
        Ok(ObservationWatermark {
            source_car_id,
            source_id: target.source_id,
            vehicle_id: target.vehicle_id,
            observation_id: latest
                .as_ref()
                .map_or(0, |observation| observation.observation_id),
            observed_at_ms: latest
                .as_ref()
                .map(|observation| observation.observed_at_ms),
            received_at_ms: latest
                .as_ref()
                .map(|observation| observation.received_at_ms),
        })
    }

    /// Verify that at least one raw observation for the selected source car
    /// has a strictly greater durable observation id than the supplied
    /// watermark. Only metadata is read and returned.
    pub fn verify_observation_after(
        &self,
        source_car_id: i64,
        after_observation_id: i64,
    ) -> Result<ObservationVerification, ObservationVerificationError> {
        if after_observation_id < 0 {
            return Err(ObservationVerificationError::InvalidWatermark);
        }
        let target = self.resolve_observation_target(source_car_id)?;
        let connection = self.open_read_only_connection()?;
        let latest = latest_observation_metadata(
            &connection,
            target.vehicle_id,
            Some(after_observation_id),
        )?;
        Ok(ObservationVerification {
            source_car_id,
            source_id: target.source_id,
            vehicle_id: target.vehicle_id,
            after_observation_id,
            latest_observation_id: latest
                .as_ref()
                .map(|observation| observation.observation_id),
            latest_observed_at_ms: latest
                .as_ref()
                .map(|observation| observation.observed_at_ms),
            latest_received_at_ms: latest
                .as_ref()
                .map(|observation| observation.received_at_ms),
        })
    }

    /// Capture the highest durable outbound-request receipt id. A caller must
    /// capture this before starting a proof window, then pass it to
    /// `verify_no_wake_after` after the collection attempt has finished.
    pub fn outbound_request_watermark(&self) -> Result<OutboundRequestWatermark, StoreError> {
        let connection = self.open_read_only_connection()?;
        let receipt_id = connection
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM outbound_request_receipts",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        Ok(OutboundRequestWatermark { receipt_id })
    }

    /// Bounded, redacted stream health aggregate for operator diagnostics.
    /// Reads only typed receipt metadata; no URL, token, payload, or free-form
    /// provider error is stored or returned.
    pub fn stream_audit_summary_since(
        &self,
        since_ms: i64,
    ) -> Result<StreamAuditSummary, StoreError> {
        if since_ms < 0 {
            return Err(StoreError::InvalidStreamAuditWindow);
        }
        let connection = self.open_read_only_connection()?;
        let outbound: (i64, i64, i64, i64, i64, i64, i64, i64, Option<i64>, Option<i64>) =
            connection
                .query_row(
                    "SELECT
                        COUNT(*) FILTER (WHERE operation = 'stream_connect'),
                        COUNT(*) FILTER (WHERE operation = 'stream_connect' AND outcome = 'success'),
                        COUNT(*) FILTER (WHERE operation = 'stream_subscribe'),
                        COUNT(*) FILTER (WHERE operation = 'stream_subscribe' AND outcome = 'success'),
                        COUNT(*) FILTER (WHERE outcome = 'transport_error'),
                        COUNT(*) FILTER (WHERE outcome = 'authentication_rejected'),
                        COUNT(*) FILTER (WHERE outcome = 'protocol_error'),
                        COUNT(*) FILTER (WHERE outcome = 'started'),
                        MAX(CASE WHEN operation = 'stream_subscribe' AND outcome = 'success'
                                 THEN completed_at_ms END),
                        MAX(CASE WHEN outcome IN (
                                'transport_error', 'authentication_rejected', 'protocol_error'
                            ) THEN completed_at_ms END)
                     FROM outbound_request_receipts
                     WHERE transport = 'stream' AND started_at_ms >= ?1",
                    params![since_ms],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                            row.get(9)?,
                        ))
                    },
                )
                .map_err(StoreError::OutboundRequestReceipt)?;
        let sessions: (i64, i64, i64, i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*),
                        COUNT(*) FILTER (WHERE outcome = 'started'),
                        COUNT(*) FILTER (WHERE outcome = 'orderly_shutdown'),
                        COUNT(*) FILTER (WHERE outcome = 'transport_ended'),
                        COUNT(*) FILTER (WHERE outcome = 'failed'),
                        COUNT(*) FILTER (WHERE outcome = 'cancelled_before_subscription')
                   FROM stream_session_receipts WHERE started_at_ms >= ?1",
                params![since_ms],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map_err(StoreError::StreamSessionReceipt)?;
        let count = |value: i64| u64::try_from(value).map_err(|_| StoreError::InvalidStoredCount);
        Ok(StreamAuditSummary {
            since_ms,
            connect_attempts: count(outbound.0)?,
            successful_connects: count(outbound.1)?,
            subscribe_attempts: count(outbound.2)?,
            successful_subscriptions: count(outbound.3)?,
            transport_errors: count(outbound.4)?,
            authentication_rejections: count(outbound.5)?,
            protocol_errors: count(outbound.6)?,
            unresolved_attempts: count(outbound.7)?,
            last_subscription_success_at_ms: outbound.8,
            last_failure_at_ms: outbound.9,
            sessions: count(sessions.0)?,
            unresolved_sessions: count(sessions.1)?,
            orderly_shutdowns: count(sessions.2)?,
            transport_ended_sessions: count(sessions.3)?,
            failed_sessions: count(sessions.4)?,
            cancelled_before_subscription_sessions: count(sessions.5)?,
        })
    }

    /// Persist an outbound-request attempt before the caller performs network
    /// I/O. This API deliberately accepts only typed classifications and
    /// numeric metadata: URLs, headers, tokens, bodies, response payloads, and
    /// arbitrary error strings cannot be written to the request ledger.
    pub fn begin_outbound_request(
        &self,
        request: &OutboundRequestStart,
    ) -> Result<OutboundRequestReceiptId, StoreError> {
        request.validate()?;
        let started_at_ms = outbound_request_clock_ms()?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        ensure_outbound_request_capacity(&transaction)?;
        transaction
            .execute(
                "INSERT INTO outbound_request_receipts(
                    correlation_id, started_at_ms, vehicle_tesla_id, transport,
                    operation, safety_class, precondition, outcome
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'started')",
                params![
                    request.correlation_id.to_string(),
                    started_at_ms,
                    request.vehicle_tesla_id,
                    request.transport.as_str(),
                    request.operation.as_str(),
                    request.safety_class.as_str(),
                    request.precondition.as_str(),
                ],
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        let receipt_id = transaction.last_insert_rowid();
        transaction
            .commit()
            .map_err(StoreError::OutboundRequestReceipt)?;
        Ok(OutboundRequestReceiptId(receipt_id))
    }

    /// Complete a previously durable request attempt in a separate SQLite
    /// transaction. Every retry must use a new `begin_outbound_request` call;
    /// this method never overwrites an earlier terminal receipt.
    pub fn complete_outbound_request(
        &self,
        receipt_id: OutboundRequestReceiptId,
        completion: &OutboundRequestCompletion,
    ) -> Result<(), StoreError> {
        completion.validate()?;
        if receipt_id.0 <= 0 {
            return Err(StoreError::InvalidOutboundRequestReceiptId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let reserved_refresh: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM legacy_refresh_receipt_bindings
                     WHERE receipt_id = ?1
                ) OR EXISTS(
                    SELECT 1 FROM fleet_refresh_receipt_bindings
                     WHERE receipt_id = ?1
                )",
                params![receipt_id.0],
                |row| row.get(0),
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        if reserved_refresh {
            return Err(StoreError::ReservedLegacyRefreshReceipt);
        }
        let started_at_ms: Option<i64> = transaction
            .query_row(
                "SELECT started_at_ms FROM outbound_request_receipts
                 WHERE id = ?1 AND outcome = 'started'",
                params![receipt_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::OutboundRequestReceipt)?;
        let started_at_ms = started_at_ms.ok_or(StoreError::OutboundRequestReceiptNotStarted)?;
        // Store-generated time governs terminal receipt age and duration. This
        // prevents a caller-controlled clock from expiring a receipt early or
        // holding retention indefinitely. Clamp a backwards wall-clock step to
        // the durable start timestamp rather than creating an invalid row.
        let completed_at_ms = outbound_request_clock_ms()?.max(started_at_ms);
        let duration_ms = completed_at_ms - started_at_ms;
        transaction
            .execute(
                "UPDATE outbound_request_receipts
                 SET completed_at_ms = ?2, duration_ms = ?3, outcome = ?4,
                     http_status = ?5, retry_after_seconds = ?6
                 WHERE id = ?1 AND outcome = 'started'",
                params![
                    receipt_id.0,
                    completed_at_ms,
                    duration_ms,
                    completion.outcome.as_str(),
                    completion.http_status,
                    completion
                        .retry_after_seconds
                        .map(i64::try_from)
                        .transpose()
                        .map_err(|_| StoreError::InvalidOutboundRequestRetryAfter)?
                ],
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        // Retention cleanup only ever removes terminal rows older than the
        // store-clock 30-day cutoff. It never deletes in-window or unresolved
        // receipts merely to meet the capacity bound.
        prune_expired_outbound_request_receipts(&transaction)?;
        transaction
            .commit()
            .map_err(StoreError::OutboundRequestReceipt)
    }

    /// Start a stream-session attempt. A process crash or task abort
    /// deliberately leaves this row
    /// unresolved. Normal code paths terminalize it explicitly, distinguishing
    /// an orderly unsubscribe from cancellation, transport loss, or failure.
    pub fn begin_stream_session(
        &self,
        correlation_id: Uuid,
        vehicle_tesla_id: i64,
    ) -> Result<StreamSessionReceiptId, StoreError> {
        if correlation_id.is_nil() {
            return Err(StoreError::NilOutboundRequestCorrelationId);
        }
        if vehicle_tesla_id <= 0 {
            return Err(StoreError::InvalidOutboundRequestVehicleId);
        }
        let started_at_ms = outbound_request_clock_ms()?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        ensure_stream_session_capacity(&transaction)?;
        transaction
            .execute(
                "INSERT INTO stream_session_receipts(
                    correlation_id, vehicle_tesla_id, started_at_ms, outcome
                 ) VALUES (?1, ?2, ?3, 'started')",
                params![correlation_id.to_string(), vehicle_tesla_id, started_at_ms],
            )
            .map_err(StoreError::StreamSessionReceipt)?;
        let receipt_id = transaction.last_insert_rowid();
        transaction
            .commit()
            .map_err(StoreError::StreamSessionReceipt)?;
        Ok(StreamSessionReceiptId(receipt_id))
    }

    /// Complete a session only after its explicit unsubscribe control request
    /// has itself completed successfully under the same correlation and car.
    pub fn complete_stream_session_orderly(
        &self,
        session_id: StreamSessionReceiptId,
        unsubscribe_receipt_id: OutboundRequestReceiptId,
    ) -> Result<(), StoreError> {
        if session_id.0 <= 0 || unsubscribe_receipt_id.0 <= 0 {
            return Err(StoreError::InvalidStreamSessionReceiptId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let session: Option<(i64, String, i64)> = transaction
            .query_row(
                "SELECT started_at_ms, correlation_id, vehicle_tesla_id
                 FROM stream_session_receipts WHERE id = ?1 AND outcome = 'started'",
                params![session_id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::StreamSessionReceipt)?;
        let (started_at_ms, correlation_id, vehicle_tesla_id) =
            session.ok_or(StoreError::StreamSessionReceiptNotStarted)?;
        // A receipt from an earlier supervisor attempt under the same
        // correlation/car is not evidence that this session shut down
        // cleanly. The control request must both start and finish after this
        // exact session began; any later session, including one that already
        // completed, makes this session non-terminal. Callers therefore fail
        // closed rather than attaching an unsubscribe to the wrong attempt.
        let unsubscribe_ok: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM outbound_request_receipts
                 WHERE id = ?1 AND correlation_id = ?2 AND vehicle_tesla_id = ?3
                   AND transport = 'stream' AND operation = 'stream_unsubscribe'
                   AND outcome = 'success'
                   AND started_at_ms >= ?4 AND completed_at_ms >= ?4
                   AND NOT EXISTS (
                       SELECT 1 FROM stream_session_receipts AS newer
                       WHERE newer.correlation_id = ?2
                         AND newer.vehicle_tesla_id = ?3
                         AND newer.id <> ?5
                         AND (newer.started_at_ms > ?4
                              OR (newer.started_at_ms = ?4 AND newer.id > ?5))
                   )",
                params![
                    unsubscribe_receipt_id.0,
                    correlation_id,
                    vehicle_tesla_id,
                    started_at_ms,
                    session_id.0,
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::StreamSessionReceipt)?;
        if unsubscribe_ok.is_none() {
            return Err(StoreError::StreamSessionUnsubscribeNotCompleted);
        }
        let completed_at_ms = outbound_request_clock_ms()?.max(started_at_ms);
        transaction
            .execute(
                "UPDATE stream_session_receipts
                 SET completed_at_ms = ?2, duration_ms = ?3, outcome = 'orderly_shutdown',
                     unsubscribe_receipt_id = ?4
                 WHERE id = ?1 AND outcome = 'started'",
                params![
                    session_id.0,
                    completed_at_ms,
                    completed_at_ms - started_at_ms,
                    unsubscribe_receipt_id.0,
                ],
            )
            .map_err(StoreError::StreamSessionReceipt)?;
        prune_expired_stream_session_receipts(&transaction)?;
        transaction
            .commit()
            .map_err(StoreError::StreamSessionReceipt)
    }

    /// Resolve a supervisor lifetime that ended without an active subscribed
    /// socket to unsubscribe. This is not an orderly-unsubscribe receipt and
    /// cannot be confused with one: the terminal outcome is explicit and the
    /// unsubscribe reference must remain NULL. A process crash still leaves
    /// `started`, preserving the crash evidence used by no-wake verification.
    pub fn complete_stream_session_terminal(
        &self,
        session_id: StreamSessionReceiptId,
        outcome: StreamSessionTerminalOutcome,
    ) -> Result<(), StoreError> {
        if session_id.0 <= 0 {
            return Err(StoreError::InvalidStreamSessionReceiptId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let started_at_ms: Option<i64> = transaction
            .query_row(
                "SELECT started_at_ms FROM stream_session_receipts
                 WHERE id = ?1 AND outcome = 'started'",
                params![session_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::StreamSessionReceipt)?;
        let started_at_ms = started_at_ms.ok_or(StoreError::StreamSessionReceiptNotStarted)?;
        let completed_at_ms = outbound_request_clock_ms()?.max(started_at_ms);
        let updated = transaction
            .execute(
                "UPDATE stream_session_receipts
                 SET completed_at_ms = ?2, duration_ms = ?3, outcome = ?4
                 WHERE id = ?1 AND outcome = 'started'",
                params![
                    session_id.0,
                    completed_at_ms,
                    completed_at_ms - started_at_ms,
                    outcome.as_str(),
                ],
            )
            .map_err(StoreError::StreamSessionReceipt)?;
        if updated != 1 {
            return Err(StoreError::StreamSessionReceiptNotStarted);
        }
        prune_expired_stream_session_receipts(&transaction)?;
        transaction
            .commit()
            .map_err(StoreError::StreamSessionReceipt)
    }

    /// Return bounded, redacted receipt metadata for one correlation after a
    /// captured watermark. This is intentionally the only public receipt read
    /// API; it cannot return a request URL, headers, bodies, or error text
    /// because none are persisted.
    pub fn outbound_request_receipts_after(
        &self,
        after_receipt_id: i64,
        correlation_id: Uuid,
        limit: u32,
    ) -> Result<Vec<OutboundRequestReceipt>, StoreError> {
        if after_receipt_id < 0 {
            return Err(StoreError::InvalidOutboundRequestWatermark);
        }
        if limit == 0 || limit > MAX_OUTBOUND_REQUEST_QUERY_LIMIT {
            return Err(StoreError::InvalidOutboundRequestQueryLimit {
                actual: limit,
                maximum: MAX_OUTBOUND_REQUEST_QUERY_LIMIT,
            });
        }
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, correlation_id, started_at_ms, completed_at_ms, duration_ms,
                        vehicle_tesla_id, transport, operation, safety_class,
                        precondition, outcome, http_status, retry_after_seconds
                 FROM outbound_request_receipts
                 WHERE id > ?1 AND correlation_id = ?2
                 ORDER BY id ASC LIMIT ?3",
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        let rows = statement
            .query_map(
                params![
                    after_receipt_id,
                    correlation_id.to_string(),
                    i64::from(limit)
                ],
                receipt_from_row,
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        rows.map(|row| row.map_err(StoreError::OutboundRequestReceipt))
            .collect()
    }

    /// Verify a bounded, correlation-scoped no-wake audit window. Empty audit
    /// windows are intentionally not proof: until network clients emit receipt
    /// rows, a verifier must fail closed rather than treating absence of data as
    /// evidence of safe collection.
    pub fn verify_no_wake_after(
        &self,
        after_receipt_id: i64,
        correlation_id: Uuid,
        observation: Option<(i64, i64)>,
    ) -> Result<NoWakeVerification, NoWakeVerificationError> {
        if after_receipt_id < 0 {
            return Err(NoWakeVerificationError::InvalidAuditWatermark);
        }
        let connection = self.open_read_only_connection()?;
        let (matching_receipts, unresolved_receipts, direct_wake_receipts, conditional_without_power_receipts) = connection
            .query_row(
                "SELECT
                    COUNT(*),
                    COALESCE(SUM(CASE WHEN outcome = 'started' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN safety_class = 'direct_wake_command' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN operation = 'vehicle_data'
                                  AND (precondition <> 'stream_power_confirmed'
                                       OR safety_class <> 'conditional_read')
                             THEN 1 ELSE 0 END), 0)
                 FROM outbound_request_receipts
                 WHERE id > ?1 AND correlation_id = ?2",
                params![after_receipt_id, correlation_id.to_string()],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                )),
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        let unresolved_stream_sessions: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM stream_session_receipts
                 WHERE correlation_id = ?1 AND outcome = 'started'",
                params![correlation_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::StreamSessionReceipt)?;
        let observation = match observation {
            Some((source_car_id, watermark)) => {
                Some(self.verify_observation_after(source_car_id, watermark)?)
            }
            None => None,
        };
        Ok(NoWakeVerification {
            after_receipt_id,
            correlation_id,
            matching_receipts,
            unresolved_receipts,
            unresolved_stream_sessions,
            direct_wake_receipts,
            conditional_without_power_receipts,
            observation,
        })
    }

    fn resolve_observation_target(
        &self,
        source_car_id: i64,
    ) -> Result<ObservationTarget, ObservationVerificationError> {
        require_positive_db(source_car_id, "source car id")
            .map_err(|_| ObservationVerificationError::InvalidSourceCarId)?;
        let connection = self.open_read_only_connection()?;
        let mut statement = connection
            .prepare(
                "SELECT DISTINCT vehicles.vehicle_id, vehicles.source_id
                 FROM vehicles
                 WHERE vehicles.vehicle_id IN (
                    SELECT vehicle_id FROM materialised_cars WHERE car_id = ?1
                    UNION
                    SELECT vehicle_id FROM vehicle_lifecycle_state WHERE car_id = ?1
                    UNION
                    SELECT vehicle_id FROM car_settings WHERE car_id = ?1
                 )
                 ORDER BY vehicles.vehicle_id",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![source_car_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(StoreError::Query)?;
        let mut targets = Vec::new();
        for row in rows {
            let (vehicle_id, source_id) = row.map_err(StoreError::Query)?;
            targets.push(ObservationTarget {
                vehicle_id: parse_stored_uuid("observation vehicle", &vehicle_id)?,
                source_id: parse_stored_uuid("observation source", &source_id)?,
            });
        }
        match targets.as_slice() {
            [] => Err(ObservationVerificationError::NoVehicleMapping),
            [target] => Ok(*target),
            _ => Err(ObservationVerificationError::AmbiguousVehicleMapping),
        }
    }
}
