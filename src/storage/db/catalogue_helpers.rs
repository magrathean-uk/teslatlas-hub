// SPDX-License-Identifier: AGPL-3.0-only

/// Wire validity is not sufficient to publish a pack. Schema 2.2 deliberately
/// remains protocol-recognized while its Hub writer, catalogue, and receiver
/// are incomplete, so all catalogue entry points share this fail-closed gate.
fn validate_manifest_for_catalogue(manifest: &SyncManifest) -> Result<(), StoreError> {
    manifest.validate().map_err(StoreError::Manifest)?;
    validate_schema_for_catalogue(manifest.schema)?;
    Ok(())
}

fn validate_schema_for_catalogue(schema: crate::protocol::SchemaVersion) -> Result<(), StoreError> {
    match schema.support() {
        Some(
            SchemaSupport::GenericTransport
            | SchemaSupport::TypedHubProjection
            | SchemaSupport::FullSnapshotOnlyHubProjection,
        ) => Ok(()),
        None => Err(StoreError::SchemaPublicationUnavailable(schema)),
    }
}

/// A catalogue row may describe the legacy single-manifest shape, the
/// additive lineage envelope, or one persisted lineage successor.  All three
/// are immutable evidence for an active pack, so pack serving must validate
/// the actual envelope rather than assuming the legacy shape.
fn validate_catalogued_pack_manifest(payload: &[u8]) -> Result<(), StoreError> {
    match serde_json::from_slice::<SyncManifest>(payload) {
        Ok(manifest) => validate_manifest_for_catalogue(&manifest),
        Err(sync_error) => match serde_json::from_slice::<LineageManifestV2>(payload) {
            Ok(lineage) => {
                lineage
                    .validate_with_limits(ProtocolLimits::default())
                    .map_err(StoreError::Manifest)?;
                validate_schema_for_catalogue(lineage.schema)
            }
            Err(_) => {
                let delta: LineageDelta = serde_json::from_slice(payload)
                    .map_err(|_| StoreError::DeserializeManifest(sync_error))?;
                delta
                    .pack
                    .validate(ProtocolLimits::default())
                    .map_err(StoreError::Manifest)?;
                if delta.from_sequence >= delta.to_sequence
                    || delta.pack_digest != delta.pack.sha256
                    || delta.pack.sequence
                        != (SequenceRange {
                            from_exclusive: delta.from_sequence,
                            to_inclusive: delta.to_sequence,
                        })
                    || delta.chain_digest
                        != canonical_delta_chain_digest(
                            delta.parent_chain_digest,
                            delta.pack.sha256,
                        )
                {
                    return Err(StoreError::LineageCatalogConflict);
                }
                validate_schema_for_catalogue(delta.pack.schema)
            }
        },
    }
}

fn decode_manifest(payload: Vec<u8>) -> Result<SyncManifest, StoreError> {
    let manifest: SyncManifest =
        serde_json::from_slice(&payload).map_err(StoreError::DeserializeManifest)?;
    validate_manifest_for_catalogue(&manifest)?;
    Ok(manifest)
}

fn verify_transport_pack_catalogue_binding(
    catalogue: &HashMap<String, (String, i64, String, i64, i64)>,
    pack: &TransportPack,
) -> Result<(), StoreError> {
    let expected = (
        pack.snapshot_id.to_string(),
        i64::from(pack.ordinal),
        pack.relative_path.clone(),
        i64::try_from(pack.compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
        i64::try_from(pack.uncompressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
    );
    if catalogue.get(&pack.sha256.to_string()) == Some(&expected) {
        Ok(())
    } else {
        Err(StoreError::LineageCatalogConflict)
    }
}

fn validate_identity(
    field: &'static str,
    value: &str,
    maximum_bytes: usize,
) -> Result<(), StoreError> {
    if value.is_empty() {
        return Err(StoreError::EmptyIdentity(field));
    }
    if value.len() > maximum_bytes {
        return Err(StoreError::IdentityTooLong {
            field,
            actual: value.len(),
            maximum: maximum_bytes,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(StoreError::IdentityControlCharacter(field));
    }
    Ok(())
}

fn validate_timestamp(field: &'static str, timestamp_ms: i64) -> Result<(), StoreError> {
    if timestamp_ms < 0 {
        return Err(StoreError::NegativeTimestamp(field));
    }
    Ok(())
}

fn supervised_collector_lease_deadline(now_ms: i64) -> Result<i64, StoreError> {
    now_ms
        .checked_add(SUPERVISED_COLLECTOR_LEASE_MS)
        .ok_or(StoreError::SupervisedCollectorClockOverflow)
}

fn find_source(
    transaction: &rusqlite::Transaction<'_>,
    descriptor: &SourceDescriptor,
) -> Result<Option<SourceRecord>, StoreError> {
    let row = transaction
        .query_row(
            "SELECT sources.source_id, sources.source_kind, source_identities.source_key, \
                    sources.generation, sources.created_at_ms \
             FROM sources \
             JOIN source_identities USING (source_id) \
             WHERE source_identities.source_kind = ?1 AND source_identities.source_key = ?2",
            params![descriptor.kind, descriptor.key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::Query)?;
    row.map(source_from_columns).transpose()
}

fn source_from_columns(
    columns: (String, String, String, i64, i64),
) -> Result<SourceRecord, StoreError> {
    let (source_id, kind, key, generation, created_at_ms) = columns;
    Ok(SourceRecord {
        source_id: parse_stored_uuid("source_id", &source_id)?,
        kind,
        key,
        generation: u64::try_from(generation).map_err(|_| StoreError::InvalidStoredGeneration)?,
        created_at_ms,
    })
}

fn ensure_source_exists(
    transaction: &rusqlite::Transaction<'_>,
    source_id: Uuid,
) -> Result<(), StoreError> {
    let found = transaction
        .query_row(
            "SELECT 1 FROM sources WHERE source_id = ?1",
            params![source_id.to_string()],
            |_| Ok(()),
        )
        .optional()
        .map_err(StoreError::Query)?;
    found.ok_or(StoreError::UnknownSource(source_id))
}

fn find_vehicle(
    transaction: &rusqlite::Transaction<'_>,
    source_id: Uuid,
    source_vehicle_key: &str,
) -> Result<Option<VehicleRecord>, StoreError> {
    let row = transaction
        .query_row(
            "SELECT vehicle_id, source_id, source_vehicle_key, vin, display_name, \
                    created_at_ms, last_seen_at_ms \
             FROM vehicles \
             WHERE source_id = ?1 AND source_vehicle_key = ?2",
            params![source_id.to_string(), source_vehicle_key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::Query)?;
    row.map(vehicle_from_columns).transpose()
}

fn find_vehicle_by_id(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<Option<VehicleRecord>, StoreError> {
    let row = transaction
        .query_row(
            "SELECT vehicle_id, source_id, source_vehicle_key, vin, display_name,
                    created_at_ms, last_seen_at_ms
             FROM vehicles WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::Query)?;
    row.map(vehicle_from_columns).transpose()
}

fn find_identity_vehicle(
    transaction: &rusqlite::Transaction<'_>,
    descriptor: &VehicleDescriptor,
) -> Result<Option<Uuid>, StoreError> {
    let mut strong = Vec::new();
    let mut secondary = Vec::new();
    let mut statement = transaction
        .prepare("SELECT alias_kind, vehicle_id FROM vehicle_identity_aliases WHERE alias_kind IN ('tesla_eid', 'tesla_vid', 'vin') AND alias_value = ?1")
        .map_err(StoreError::Query)?;
    let mut find = |kind: &str, value: String| -> Result<(), StoreError> {
        let mut rows = statement.query(params![value]).map_err(StoreError::Query)?;
        while let Some(row) = rows.next().map_err(StoreError::Query)? {
            let found_kind: String = row.get(0).map_err(StoreError::Query)?;
            let id = parse_stored_uuid(
                "vehicle_id",
                &row.get::<_, String>(1).map_err(StoreError::Query)?,
            )?;
            if found_kind == kind && !strong.contains(&id) && !secondary.contains(&id) {
                if kind == "tesla_vid" {
                    secondary.push(id);
                } else {
                    strong.push(id);
                }
            }
        }
        Ok(())
    };
    if let Some(eid) = descriptor.tesla_eid {
        find("tesla_eid", eid.to_string())?;
    }
    if let Some(vin) = &descriptor.vin {
        find("vin", vin.clone())?;
    }
    if let Some(vid) = descriptor.tesla_vid {
        find("tesla_vid", vid.to_string())?;
    }
    if strong.len() > 1 || (!strong.is_empty() && secondary.iter().any(|id| !strong.contains(id))) {
        return Err(StoreError::VehicleIdentityConflict);
    }
    if strong.len() == 1 {
        return Ok(strong.into_iter().next());
    }
    if descriptor.tesla_eid.is_some() || descriptor.vin.is_some() {
        return Ok(None);
    }
    if secondary.len() > 1 {
        return Err(StoreError::VehicleIdentityConflict);
    }
    Ok(secondary.into_iter().next())
}

fn register_vehicle_aliases(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    descriptor: &VehicleDescriptor,
) -> Result<(), StoreError> {
    // A VIN match is the durable car identity when Tesla changes the EID
    // exposed by a provider. Keep exactly one current EID alias so commands
    // and settings cannot select an arbitrary historical value.
    if let (Some(vin), Some(eid)) = (&descriptor.vin, descriptor.tesla_eid) {
        let stored_vin: Option<String> = transaction
            .query_row(
                "SELECT vin FROM vehicles WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?
            .flatten();
        if stored_vin.as_deref() == Some(vin.as_str()) {
            transaction
                .execute(
                    "DELETE FROM vehicle_identity_aliases
                      WHERE vehicle_id = ?1 AND alias_kind = 'tesla_eid'
                        AND alias_value <> ?2",
                    params![vehicle_id.to_string(), eid.to_string()],
                )
                .map_err(StoreError::RegisterVehicle)?;
        }
    }
    let mut aliases = vec![(
        "source_key",
        format!("{}:{}", descriptor.source_id, descriptor.source_vehicle_key),
    )];
    if let Some(eid) = descriptor.tesla_eid {
        aliases.push(("tesla_eid", eid.to_string()));
    }
    if let Some(vin) = &descriptor.vin {
        aliases.push(("vin", vin.clone()));
    }
    if let Some(vid) = descriptor.tesla_vid {
        aliases.push(("tesla_vid", vid.to_string()));
    }
    for (kind, value) in aliases {
        let conflict: Option<String> = transaction
            .query_row(
                "SELECT vehicle_id FROM vehicle_identity_aliases WHERE alias_kind = ?1 AND alias_value = ?2",
                params![kind, value], |row| row.get(0),
            ).optional().map_err(StoreError::Query)?;
        if let Some(existing) = conflict
            && existing != vehicle_id.to_string()
        {
            if kind == "tesla_vid" && (descriptor.tesla_eid.is_some() || descriptor.vin.is_some()) {
                continue;
            }
            return Err(StoreError::VehicleIdentityConflict);
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO vehicle_identity_aliases
             (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
             VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    kind,
                    value,
                    vehicle_id.to_string(),
                    descriptor.source_id.to_string(),
                    descriptor.source_vehicle_key
                ],
            )
            .map_err(StoreError::RegisterVehicle)?;
    }
    Ok(())
}

fn vehicle_from_columns(
    columns: (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
        i64,
    ),
) -> Result<VehicleRecord, StoreError> {
    let (
        vehicle_id,
        source_id,
        source_vehicle_key,
        vin,
        display_name,
        created_at_ms,
        last_seen_at_ms,
    ) = columns;
    Ok(VehicleRecord {
        vehicle_id: parse_stored_uuid("vehicle_id", &vehicle_id)?,
        source_id: parse_stored_uuid("source_id", &source_id)?,
        source_vehicle_key,
        vin,
        display_name,
        created_at_ms,
        last_seen_at_ms,
    })
}

fn ensure_vehicle_belongs_to_source(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
) -> Result<(), StoreError> {
    let belongs: Option<i64> = transaction
        .query_row(
            "SELECT 1 FROM vehicle_identity_aliases WHERE vehicle_id = ?1 AND source_id = ?2",
            params![vehicle_id.to_string(), source_id.to_string()],
            |_| Ok(1),
        )
        .optional()
        .map_err(StoreError::Query)?;
    if belongs.is_none() {
        let exists: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM vehicles WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |_| Ok(1),
            )
            .optional()
            .map_err(StoreError::Query)?;
        return if exists.is_some() {
            Err(StoreError::VehicleSourceMismatch {
                vehicle_id,
                source_id,
            })
        } else {
            Err(StoreError::UnknownVehicle(vehicle_id))
        };
    }
    Ok(())
}

fn find_observation(
    transaction: &rusqlite::Transaction<'_>,
    source_id: Uuid,
    vehicle_id: Uuid,
    observed_at_ms: i64,
    payload_sha256: Sha256Digest,
) -> Result<Option<ObservationRecord>, StoreError> {
    transaction
        .query_row(
            "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms, \
                    payload_sha256, payload_json \
             FROM raw_observations \
             WHERE source_id = ?1 AND vehicle_id = ?2 AND observed_at_ms = ?3 \
               AND payload_sha256 = ?4",
            params![
                source_id.to_string(),
                vehicle_id.to_string(),
                observed_at_ms,
                payload_sha256.as_bytes().as_slice(),
            ],
            observation_from_row,
        )
        .optional()
        .map_err(StoreError::Query)
}

fn observation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ObservationRecord> {
    use rusqlite::types::Type;

    let source_id: String = row.get(1)?;
    let vehicle_id: String = row.get(2)?;
    let payload_sha256: Vec<u8> = row.get(5)?;
    let payload_json: String = row.get(6)?;
    let source_id = Uuid::parse_str(&source_id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(error))
    })?;
    let vehicle_id = Uuid::parse_str(&vehicle_id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
    })?;
    let digest: [u8; 32] = payload_sha256.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            Type::Blob,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stored SHA-256 digest does not have 32 bytes",
            )),
        )
    })?;
    let payload = serde_json::from_str(&payload_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, Type::Text, Box::new(error))
    })?;
    Ok(ObservationRecord {
        observation_id: row.get(0)?,
        source_id,
        vehicle_id,
        observed_at_ms: row.get(3)?,
        received_at_ms: row.get(4)?,
        payload_sha256: Sha256Digest::from_bytes(digest),
        payload,
    })
}

fn paired_device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PairedDeviceRecord> {
    use rusqlite::types::Type;

    let device_id: String = row.get(0)?;
    let device_id = Uuid::parse_str(&device_id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error))
    })?;
    Ok(PairedDeviceRecord {
        device_id,
        display_name: row.get(1)?,
        created_at_ms: row.get(2)?,
        expires_at_ms: row.get(3)?,
        revoked_at_ms: row.get(4)?,
        last_authenticated_at_ms: row.get(5)?,
    })
}

fn random_secret_wire() -> Result<String, StoreError> {
    let mut bytes = Zeroizing::new([0_u8; PAIRING_SECRET_BYTES]);
    getrandom::fill(&mut *bytes).map_err(|_| StoreError::EntropyUnavailable)?;
    Ok(hex::encode(bytes.as_slice()))
}

fn sha256_bytes(value: &[u8]) -> [u8; PAIRING_SECRET_BYTES] {
    Sha256::digest(value).into()
}

fn digest_valid_wire_secret(value: &str) -> Option<[u8; PAIRING_SECRET_BYTES]> {
    if value.len() != PAIRING_SECRET_BYTES * 2
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    // Length plus the ASCII-hex predicate fully validates the wire shape.
    // Avoid decoding a second credential-equivalent byte buffer just to
    // validate text that is hashed exactly as received.
    Some(sha256_bytes(value.as_bytes()))
}

fn constant_time_equal(
    left: &[u8; PAIRING_SECRET_BYTES],
    right: &[u8; PAIRING_SECRET_BYTES],
) -> bool {
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn parse_stored_uuid(field: &'static str, value: &str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(value).map_err(|_| StoreError::InvalidStoredUuid(field))
}

fn ensure_installation_id(connection: &Connection) -> Result<Uuid, StoreError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(StoreError::Begin)?;
    let existing: Option<String> = transaction
        .query_row(
            "SELECT value FROM hub_metadata WHERE key = ?1",
            params![INSTALLATION_ID_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::InstallationIdentity)?;
    let value = match existing {
        Some(value) => value,
        None => {
            let value = Uuid::new_v4().to_string();
            transaction
                .execute(
                    "INSERT INTO hub_metadata (key, value) VALUES (?1, ?2)",
                    params![INSTALLATION_ID_KEY, value],
                )
                .map_err(StoreError::InstallationIdentity)?;
            value
        }
    };
    transaction
        .commit()
        .map_err(StoreError::InstallationIdentity)?;
    parse_stored_uuid("installation_id", &value)
}

fn configure(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = FULL;
            PRAGMA foreign_keys = ON;
            PRAGMA trusted_schema = OFF;
            PRAGMA busy_timeout = 5000;
            PRAGMA application_id = 1413564501;
            ",
        )
        .map_err(StoreError::Configure)
}

fn configure_read_only(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute_batch(
            "
            PRAGMA query_only = ON;
            PRAGMA foreign_keys = ON;
            PRAGMA trusted_schema = OFF;
            PRAGMA busy_timeout = 5000;
            ",
        )
        .map_err(StoreError::Configure)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatestObservationMetadata {
    pub observation_id: i64,
    pub observed_at_ms: i64,
    pub received_at_ms: i64,
}

fn latest_observation_metadata(
    connection: &Connection,
    vehicle_id: Uuid,
    after_observation_id: Option<i64>,
) -> Result<Option<LatestObservationMetadata>, StoreError> {
    connection
        .query_row(
            "SELECT observation_id, observed_at_ms, received_at_ms
             FROM (
                 SELECT observation_id, vehicle_id, observed_at_ms, received_at_ms
                   FROM raw_observations
                 UNION ALL
                 SELECT observation_id, vehicle_id, observed_at_ms, received_at_ms
                   FROM current_observations
             )
             WHERE vehicle_id = ?1
               AND (?2 IS NULL OR observation_id > ?2)
             ORDER BY observation_id DESC LIMIT 1",
            params![vehicle_id.to_string(), after_observation_id],
            |row| {
                Ok(LatestObservationMetadata {
                    observation_id: row.get(0)?,
                    observed_at_ms: row.get(1)?,
                    received_at_ms: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::Query)
}
