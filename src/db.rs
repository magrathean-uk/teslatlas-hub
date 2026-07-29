use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::protocol::{Sha256Digest, SyncManifest, TransportPack};

pub const APPLICATION_ID: i32 = 0x5441_4855; // TAHU
pub const SCHEMA_VERSION: i32 = 5;
pub const VENDORED_SQLITE_VERSION: &str = "3.53.4";

/// Hard upper bound for one persisted source response. A collector must split
/// high-volume telemetry into individual observations rather than retaining an
/// unbounded response in memory or in the Hub database.
pub const MAX_RAW_OBSERVATION_BYTES: usize = 256 * 1024;

/// The read API is deliberately capped so callers cannot accidentally turn a
/// history query into an all-memory transfer.
pub const MAX_OBSERVATION_QUERY_LIMIT: u32 = 10_000;
const MAX_SOURCE_KIND_BYTES: usize = 64;
const MAX_SOURCE_KEY_BYTES: usize = 256;
const MAX_VEHICLE_KEY_BYTES: usize = 256;
const MAX_VIN_BYTES: usize = 32;
const MAX_DISPLAY_NAME_BYTES: usize = 256;
const MAX_PAIRING_LABEL_BYTES: usize = 128;
const MAX_DEVICE_NAME_BYTES: usize = 128;
const PAIRING_SECRET_BYTES: usize = 32;
const ACCESS_TOKEN_BYTES: usize = 32;
const INSTALLATION_ID_KEY: &str = "installation_id";

#[derive(Debug, Clone)]
pub struct HubStore {
    database_path: PathBuf,
    packs_dir: PathBuf,
}

/// One opaque, single-use pairing invitation. The secret is intentionally not
/// `Debug` or `Display`; it is safe only for a local terminal or a QR payload.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingInvitation {
    pub pairing_id: Uuid,
    secret: PairingSecret,
    pub expires_at_ms: i64,
}

impl PairingInvitation {
    pub fn secret(&self) -> &str {
        self.secret.as_wire()
    }
}

impl std::fmt::Debug for PairingInvitation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingInvitation")
            .field("pairing_id", &self.pairing_id)
            .field("secret", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct PairingSecret(String);

impl PairingSecret {
    fn generate() -> Self {
        Self(hex::encode(random_secret_bytes()))
    }

    fn as_wire(&self) -> &str {
        &self.0
    }

    fn digest(&self) -> [u8; PAIRING_SECRET_BYTES] {
        sha256_bytes(self.0.as_bytes())
    }

    fn digest_from_wire(value: &str) -> Option<[u8; PAIRING_SECRET_BYTES]> {
        digest_valid_wire_secret(value)
    }
}

/// A paired device's bearer token. It is returned once at claim time and is
/// stored only as a hash in the Hub database.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceAccessToken(String);

impl DeviceAccessToken {
    fn generate() -> Self {
        Self(hex::encode(random_secret_bytes()))
    }

    pub fn as_bearer(&self) -> &str {
        &self.0
    }

    fn digest(&self) -> [u8; ACCESS_TOKEN_BYTES] {
        sha256_bytes(self.0.as_bytes())
    }

    fn digest_from_wire(value: &str) -> Option<[u8; ACCESS_TOKEN_BYTES]> {
        digest_valid_wire_secret(value)
    }
}

impl std::fmt::Debug for DeviceAccessToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DeviceAccessToken([redacted])")
    }
}

/// The only credential-bearing result of a successful pairing claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedDeviceAccess {
    pub device_id: Uuid,
    pub access_token: DeviceAccessToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedDeviceRecord {
    pub device_id: Uuid,
    pub display_name: String,
    pub created_at_ms: i64,
    pub last_authenticated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PublishedVehicle {
    pub vehicle_id: Uuid,
    pub display_name: Option<String>,
}

impl HubStore {
    pub fn initialize(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir).map_err(StoreError::CreateDataDir)?;
        let packs_dir = data_dir.join("packs");
        fs::create_dir_all(&packs_dir).map_err(StoreError::CreatePacksDir)?;

        let store = Self {
            database_path: data_dir.join("hub.sqlite"),
            packs_dir,
        };
        let connection = store.open()?;
        migrate(&connection)?;
        ensure_installation_id(&connection)?;
        Ok(store)
    }

    pub fn open(&self) -> Result<Connection, StoreError> {
        let connection = Connection::open_with_flags(
            &self.database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::Open)?;
        configure(&connection)?;
        Ok(connection)
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub fn packs_dir(&self) -> &Path {
        &self.packs_dir
    }

    /// Private, disposable local capture area. TeslaMate source snapshots are
    /// never written into the Hub catalogue database.
    pub fn imports_dir(&self) -> PathBuf {
        self.database_path
            .parent()
            .expect("Hub database path always has a data directory")
            .join("imports")
    }

    pub fn quick_check(&self) -> Result<(), StoreError> {
        let connection = self.open()?;
        let result: String = connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(StoreError::Query)?;
        if result == "ok" {
            Ok(())
        } else {
            Err(StoreError::Integrity(result))
        }
    }

    pub fn sqlite_version(&self) -> Result<String, StoreError> {
        let connection = self.open()?;
        connection
            .query_row("SELECT sqlite_version()", [], |row| row.get(0))
            .map_err(StoreError::Query)
    }

    /// Stable random identity of this Hub installation. It never comes from a
    /// remote source and survives package upgrades and restarts.
    pub fn installation_id(&self) -> Result<Uuid, StoreError> {
        let connection = self.open()?;
        ensure_installation_id(&connection)
    }

    pub fn publish_manifest(&self, manifest: &SyncManifest) -> Result<(), StoreError> {
        manifest.validate().map_err(StoreError::Manifest)?;
        let payload = serde_json::to_vec(manifest).map_err(StoreError::SerializeManifest)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "INSERT INTO sync_manifests (snapshot_id, vehicle_id, head_sequence, manifest_json) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(snapshot_id) DO UPDATE SET \
                 vehicle_id = excluded.vehicle_id, \
                 head_sequence = excluded.head_sequence, \
                 manifest_json = excluded.manifest_json",
                params![
                    manifest.snapshot_id.to_string(),
                    manifest.vehicle_id.to_string(),
                    i64::try_from(manifest.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    payload,
                ],
            )
            .map_err(StoreError::PublishManifest)?;
        transaction
            .execute(
                "DELETE FROM sync_packs WHERE snapshot_id = ?1",
                params![manifest.snapshot_id.to_string()],
            )
            .map_err(StoreError::PublishManifest)?;
        for pack in &manifest.chunks {
            transaction
                .execute(
                    "INSERT INTO sync_packs \
                     (sha256, snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        pack.sha256.to_string(),
                        manifest.snapshot_id.to_string(),
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
        transaction.commit().map_err(StoreError::PublishManifest)
    }

    pub fn manifest_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<SyncManifest>, StoreError> {
        let connection = self.open()?;
        let payload = connection
            .query_row(
                "SELECT manifest_json FROM sync_manifests \
                 WHERE vehicle_id = ?1 ORDER BY head_sequence DESC LIMIT 1",
                params![vehicle_id.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        payload.map(decode_manifest).transpose()
    }

    /// Allocate the next full-snapshot marker for one Hub vehicle. Full
    /// snapshots replace the app mirror atomically, but their marker must
    /// still rise so the latest catalog entry is unambiguous.
    pub fn next_full_snapshot_sequence(&self, vehicle_id: Uuid) -> Result<u64, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let maximum: Option<i64> = connection
            .query_row(
                "SELECT MAX(head_sequence) FROM sync_manifests WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        match maximum {
            None => Ok(1),
            Some(sequence) => u64::try_from(sequence)
                .ok()
                .and_then(|sequence| sequence.checked_add(1))
                .ok_or(StoreError::SequenceExhausted),
        }
    }

    pub fn pack_for_digest(&self, digest: Sha256Digest) -> Result<Option<StoredPack>, StoreError> {
        let connection = self.open()?;
        let entry = connection
            .query_row(
                "SELECT relative_path, compressed_bytes FROM sync_packs WHERE sha256 = ?1",
                params![digest.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((relative_path, compressed_bytes)) = entry else {
            return Ok(None);
        };
        let compressed_bytes =
            u64::try_from(compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?;
        if relative_path != TransportPack::canonical_relative_path(digest) {
            return Err(StoreError::UnsafeStoredPackPath);
        }
        Ok(Some(StoredPack {
            digest,
            compressed_bytes,
            path: self
                .packs_dir
                .join("sha256")
                .join(format!("{digest}.sqlite.zst")),
        }))
    }

    /// Create a single-use, short-lived pairing challenge. Only a SHA-256
    /// digest is persisted. The raw value is returned once to the local
    /// administrator so it can travel over an out-of-band pairing channel.
    pub fn create_pairing(
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
        let secret = PairingSecret::generate();
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
                    pairing_id.to_string(),
                    label,
                    secret.digest().as_slice(),
                    created_at_ms,
                    expires_at_ms,
                ],
            )
            .map_err(StoreError::CreatePairing)?;
        transaction.commit().map_err(StoreError::CreatePairing)?;
        Ok(PairingInvitation {
            pairing_id,
            secret,
            expires_at_ms,
        })
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
        let access_token = DeviceAccessToken::generate();
        transaction
            .execute(
                "INSERT INTO paired_devices \
                 (device_id, display_name, token_sha256, created_at_ms, last_authenticated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                params![
                    device_id.to_string(),
                    device_name,
                    access_token.digest().as_slice(),
                    claimed_at_ms,
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
        })
    }

    /// Authenticate an already-paired device without logging or retaining the
    /// presented bearer value. The caller can use the returned public device
    /// identity for authorization decisions.
    pub fn authenticate_device(
        &self,
        access_token: &str,
    ) -> Result<Option<PairedDeviceRecord>, StoreError> {
        let Some(token_digest) = DeviceAccessToken::digest_from_wire(access_token) else {
            return Ok(None);
        };
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT device_id, display_name, created_at_ms, last_authenticated_at_ms \
                 FROM paired_devices WHERE token_sha256 = ?1",
                params![token_digest.as_slice()],
                paired_device_from_row,
            )
            .optional()
            .map_err(StoreError::Query)
    }

    /// Return the vehicles this Hub has published. Pairing currently grants a
    /// device access to this one owner-controlled Hub, not to arbitrary source
    /// databases or credentials.
    pub fn published_vehicles(&self) -> Result<Vec<PublishedVehicle>, StoreError> {
        let connection = self.open()?;
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
    pub fn register_source(
        &self,
        descriptor: &SourceDescriptor,
        created_at_ms: i64,
    ) -> Result<SourceRecord, StoreError> {
        descriptor.validate()?;
        validate_timestamp("source created_at_ms", created_at_ms)?;

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if let Some(source) = find_source(&transaction, descriptor)? {
            transaction.commit().map_err(StoreError::RegisterSource)?;
            return Ok(source);
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

        Ok(SourceRecord {
            source_id,
            kind: descriptor.kind.clone(),
            key: descriptor.key.clone(),
            generation: 1,
            created_at_ms,
        })
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

        if let Some(mut vehicle) = find_vehicle(
            &transaction,
            descriptor.source_id,
            &descriptor.source_vehicle_key,
        )? {
            if let Some(expected) = expected_vehicle_id
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
        transaction.commit().map_err(StoreError::RegisterVehicle)?;

        Ok(VehicleRecord {
            vehicle_id,
            source_id: descriptor.source_id,
            source_vehicle_key: descriptor.source_vehicle_key.clone(),
            vin: descriptor.vin.clone(),
            display_name: descriptor.display_name.clone(),
            created_at_ms: registered_at_ms,
            last_seen_at_ms: registered_at_ms,
        })
    }

    /// Append exactly one bounded raw telemetry snapshot. The stored hash is
    /// calculated from the canonical JSON bytes that are written to SQLite.
    /// A collector retry for the same source, vehicle, observation time, and
    /// payload returns the original row without creating a duplicate.
    pub fn append_observation(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
    ) -> Result<AppendObservation, StoreError> {
        input.validate()?;
        validate_timestamp("observation received_at_ms", received_at_ms)?;
        let payload_json =
            serde_json::to_vec(&input.payload).map_err(StoreError::SerializeObservation)?;
        if payload_json.len() > MAX_RAW_OBSERVATION_BYTES {
            return Err(StoreError::ObservationTooLarge {
                actual: payload_json.len(),
                maximum: MAX_RAW_OBSERVATION_BYTES,
            });
        }
        let payload_sha256 = Sha256Digest::of_bytes(&payload_json);
        let payload_json =
            String::from_utf8(payload_json).expect("serde_json always serializes valid UTF-8");

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        ensure_vehicle_belongs_to_source(&transaction, input.vehicle_id, input.source_id)?;
        let inserted = transaction
            .execute(
                "INSERT INTO raw_observations \
                 (source_id, vehicle_id, observed_at_ms, received_at_ms, payload_sha256, payload_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(source_id, vehicle_id, observed_at_ms, payload_sha256) DO NOTHING",
                params![
                    input.source_id.to_string(),
                    input.vehicle_id.to_string(),
                    input.observed_at_ms,
                    received_at_ms,
                    payload_sha256.as_bytes().as_slice(),
                    payload_json,
                ],
            )
            .map_err(StoreError::AppendObservation)?
            == 1;

        let observation = find_observation(
            &transaction,
            input.source_id,
            input.vehicle_id,
            input.observed_at_ms,
            payload_sha256,
        )?
        .ok_or(StoreError::ObservationMissingAfterInsert)?;
        transaction
            .commit()
            .map_err(StoreError::AppendObservation)?;
        Ok(AppendObservation {
            observation,
            inserted,
        })
    }

    /// Read a bounded, time-ordered raw observation page for a single stable
    /// Hub vehicle identity.
    pub fn observations_for_vehicle(
        &self,
        vehicle_id: Uuid,
        query: ObservationQuery,
    ) -> Result<Vec<ObservationRecord>, StoreError> {
        query.validate()?;
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms, \
                        payload_sha256, payload_json \
                 FROM raw_observations \
                 WHERE vehicle_id = ?1 \
                   AND (?2 IS NULL OR observed_at_ms >= ?2) \
                   AND (?3 IS NULL OR observed_at_ms < ?3) \
                 ORDER BY observed_at_ms ASC, observation_id ASC \
                 LIMIT ?4",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    query.from_observed_at_ms,
                    query.until_observed_at_ms,
                    i64::from(query.limit),
                ],
                observation_from_row,
            )
            .map_err(StoreError::Query)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)
    }

    /// Load durable open-session state for crash-safe lifecycle recovery.
    pub fn load_lifecycle_state(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<LifecycleStateRecord>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT vehicle_id, car_id, last_observation_id, open_session_json, \
                        quarantined, updated_at_ms \
                 FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| {
                    let value: String = row.get(0)?;
                    let vehicle_id = Uuid::parse_str(&value).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok(LifecycleStateRecord {
                        vehicle_id,
                        car_id: row.get(1)?,
                        last_observation_id: row.get(2)?,
                        open_session_json: row.get(3)?,
                        quarantined: row.get::<_, i64>(4)? != 0,
                        updated_at_ms: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Query)
    }

    /// Persist open-session state and append newly completed history rows.
    pub fn commit_lifecycle_delta(&self, commit: &LifecycleCommit<'_>) -> Result<(), StoreError> {
        if commit.vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if commit.car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }
        if commit.last_observation_id < 0 {
            return Err(StoreError::InvalidLifecycleCursor);
        }
        validate_timestamp("lifecycle updated_at_ms", commit.updated_at_ms)?;
        if commit.open_session_json.len() < 2 || commit.open_session_json.len() > 65_536 {
            return Err(StoreError::InvalidLifecycleSession);
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;

        transaction
            .execute(
                "INSERT INTO vehicle_lifecycle_state(
                    vehicle_id, car_id, last_observation_id, open_session_json,
                    quarantined, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    car_id = excluded.car_id,
                    last_observation_id = excluded.last_observation_id,
                    open_session_json = excluded.open_session_json,
                    quarantined = excluded.quarantined,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    commit.vehicle_id.to_string(),
                    commit.car_id,
                    commit.last_observation_id,
                    commit.open_session_json,
                    i64::from(commit.quarantined),
                    commit.updated_at_ms,
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;

        for drive in &commit.delta.drives {
            let drive_json =
                serde_json::to_string(drive).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO materialised_drives(
                        vehicle_id, drive_id, car_id, drive_json
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        commit.vehicle_id.to_string(),
                        drive.id,
                        commit.car_id,
                        drive_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        for position in &commit.delta.positions {
            let position_json =
                serde_json::to_string(position).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO materialised_positions(
                        vehicle_id, position_id, drive_id, car_id, position_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        commit.vehicle_id.to_string(),
                        position.id,
                        position.drive_id,
                        commit.car_id,
                        position_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        for charge in &commit.delta.charges {
            let charge_json =
                serde_json::to_string(charge).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO materialised_charges(
                        vehicle_id, charge_id, car_id, charge_json
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        commit.vehicle_id.to_string(),
                        charge.id,
                        commit.car_id,
                        charge_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        for sample in &commit.delta.charge_samples {
            let sample_json =
                serde_json::to_string(sample).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO materialised_charge_samples(
                        vehicle_id, sample_id, charge_id, sample_json
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        commit.vehicle_id.to_string(),
                        sample.id,
                        sample.charge_process_id,
                        sample_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }

        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    /// Load completed history used when publishing a phone snapshot.
    pub fn materialised_history(
        &self,
        vehicle_id: Uuid,
    ) -> Result<MaterialisedHistory, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let vehicle_key = vehicle_id.to_string();

        let drives = load_json_rows(
            &connection,
            "SELECT drive_json FROM materialised_drives WHERE vehicle_id = ?1 ORDER BY drive_id ASC",
            &vehicle_key,
        )?;
        let positions = load_json_rows(
            &connection,
            "SELECT position_json FROM materialised_positions WHERE vehicle_id = ?1 ORDER BY position_id ASC",
            &vehicle_key,
        )?;
        let charges = load_json_rows(
            &connection,
            "SELECT charge_json FROM materialised_charges WHERE vehicle_id = ?1 ORDER BY charge_id ASC",
            &vehicle_key,
        )?;
        let charge_samples = load_json_rows(
            &connection,
            "SELECT sample_json FROM materialised_charge_samples WHERE vehicle_id = ?1 ORDER BY sample_id ASC",
            &vehicle_key,
        )?;
        Ok(MaterialisedHistory {
            drives,
            positions,
            charges,
            charge_samples,
        })
    }
}

fn load_json_rows<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    vehicle_id: &str,
) -> Result<Vec<T>, StoreError> {
    let mut statement = connection.prepare(sql).map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id], |row| row.get::<_, String>(0))
        .map_err(StoreError::Query)?;
    let mut values = Vec::new();
    for row in rows {
        let json = row.map_err(StoreError::Query)?;
        values.push(serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?);
    }
    Ok(values)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleStateRecord {
    pub vehicle_id: Uuid,
    pub car_id: i64,
    pub last_observation_id: i64,
    pub open_session_json: Vec<u8>,
    pub quarantined: bool,
    pub updated_at_ms: i64,
}

/// One transactional lifecycle write: open-session snapshot plus completed rows.
#[derive(Debug, Clone)]
pub struct LifecycleCommit<'a> {
    pub vehicle_id: Uuid,
    pub car_id: i64,
    pub open_session_json: &'a [u8],
    pub last_observation_id: i64,
    pub quarantined: bool,
    pub updated_at_ms: i64,
    pub delta: &'a crate::lifecycle::LifecycleDelta,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MaterialisedHistory {
    pub drives: Vec<crate::hub_pack::ProjectionDrive>,
    pub positions: Vec<crate::hub_pack::ProjectionPosition>,
    pub charges: Vec<crate::hub_pack::ProjectionCharge>,
    pub charge_samples: Vec<crate::hub_pack::ProjectionChargeSample>,
}

/// Non-secret source identity presented by an independent collector. The Hub
/// persists a generated UUID for this pair so restarts never change identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDescriptor {
    pub kind: String,
    pub key: String,
}

impl SourceDescriptor {
    pub fn new(kind: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            key: key.into(),
        }
    }

    fn validate(&self) -> Result<(), StoreError> {
        validate_identity("source kind", &self.kind, MAX_SOURCE_KIND_BYTES)?;
        if !self.kind.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        }) {
            return Err(StoreError::InvalidSourceKind);
        }
        validate_identity("source key", &self.key, MAX_SOURCE_KEY_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub source_id: Uuid,
    pub kind: String,
    pub key: String,
    pub generation: u64,
    pub created_at_ms: i64,
}

/// Source-owned stable vehicle identity and optional mutable display fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleDescriptor {
    pub source_id: Uuid,
    pub source_vehicle_key: String,
    pub vin: Option<String>,
    pub display_name: Option<String>,
}

impl VehicleDescriptor {
    pub fn new(source_id: Uuid, source_vehicle_key: impl Into<String>) -> Self {
        Self {
            source_id,
            source_vehicle_key: source_vehicle_key.into(),
            vin: None,
            display_name: None,
        }
    }

    fn validate(&self) -> Result<(), StoreError> {
        if self.source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        validate_identity(
            "source vehicle key",
            &self.source_vehicle_key,
            MAX_VEHICLE_KEY_BYTES,
        )?;
        if let Some(vin) = &self.vin {
            validate_identity("vehicle VIN", vin, MAX_VIN_BYTES)?;
        }
        if let Some(display_name) = &self.display_name {
            validate_identity("vehicle display name", display_name, MAX_DISPLAY_NAME_BYTES)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleRecord {
    pub vehicle_id: Uuid,
    pub source_id: Uuid,
    pub source_vehicle_key: String,
    pub vin: Option<String>,
    pub display_name: Option<String>,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
}

/// One collector-provided raw source response. The Hub accepts JSON objects
/// only; a response batch belongs as independent observations, not an array.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservationInput {
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub observed_at_ms: i64,
    pub payload: Value,
}

impl ObservationInput {
    fn validate(&self) -> Result<(), StoreError> {
        if self.source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if self.vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        validate_timestamp("observed_at_ms", self.observed_at_ms)?;
        if !self.payload.is_object() {
            return Err(StoreError::ObservationMustBeObject);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservationRecord {
    pub observation_id: i64,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub observed_at_ms: i64,
    pub received_at_ms: i64,
    pub payload_sha256: Sha256Digest,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppendObservation {
    pub observation: ObservationRecord,
    pub inserted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationQuery {
    pub from_observed_at_ms: Option<i64>,
    pub until_observed_at_ms: Option<i64>,
    pub limit: u32,
}

impl ObservationQuery {
    pub const fn from_start(limit: u32) -> Self {
        Self {
            from_observed_at_ms: None,
            until_observed_at_ms: None,
            limit,
        }
    }

    fn validate(self) -> Result<(), StoreError> {
        if self.limit == 0 || self.limit > MAX_OBSERVATION_QUERY_LIMIT {
            return Err(StoreError::InvalidObservationQueryLimit {
                actual: self.limit,
                maximum: MAX_OBSERVATION_QUERY_LIMIT,
            });
        }
        if let Some(timestamp) = self.from_observed_at_ms {
            validate_timestamp("observation query lower bound", timestamp)?;
        }
        if let Some(timestamp) = self.until_observed_at_ms {
            validate_timestamp("observation query upper bound", timestamp)?;
        }
        if let (Some(from), Some(until)) = (self.from_observed_at_ms, self.until_observed_at_ms)
            && from >= until
        {
            return Err(StoreError::InvalidObservationQueryRange);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct StoredPack {
    pub digest: Sha256Digest,
    pub compressed_bytes: u64,
    pub path: PathBuf,
}

fn decode_manifest(payload: Vec<u8>) -> Result<SyncManifest, StoreError> {
    let manifest: SyncManifest =
        serde_json::from_slice(&payload).map_err(StoreError::DeserializeManifest)?;
    manifest.validate().map_err(StoreError::Manifest)?;
    Ok(manifest)
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
    let stored_source_id = transaction
        .query_row(
            "SELECT source_id FROM vehicles WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    let Some(stored_source_id) = stored_source_id else {
        return Err(StoreError::UnknownVehicle(vehicle_id));
    };
    if stored_source_id != source_id.to_string() {
        return Err(StoreError::VehicleSourceMismatch {
            vehicle_id,
            source_id,
        });
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
        last_authenticated_at_ms: row.get(3)?,
    })
}

fn random_secret_bytes() -> [u8; PAIRING_SECRET_BYTES] {
    // UUID v4 uses the operating system CSPRNG. Two UUIDs provide 244 random
    // bits after their version/variant markers; hashing them yields a fixed
    // 32-byte opaque credential without depending on another RNG wrapper.
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    sha256_bytes(&[first.as_bytes().as_slice(), second.as_bytes().as_slice()].concat())
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
    // The wire secret is hex text; avoid accepting alternate encodings or
    // silently truncating malformed inputs before hashing them.
    let decoded = hex::decode(value).ok()?;
    let _: [u8; PAIRING_SECRET_BYTES] = decoded.try_into().ok()?;
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

fn migrate(connection: &Connection) -> Result<(), StoreError> {
    let mut version = schema_version(connection)?;
    if version == 0 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS hub_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sources (
                    source_id TEXT PRIMARY KEY NOT NULL,
                    source_kind TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    created_at_ms INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_ledger (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    sequence INTEGER NOT NULL CHECK (sequence >= 1),
                    entity_kind TEXT NOT NULL,
                    entity_key TEXT NOT NULL,
                    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
                    committed_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (source_id, sequence, entity_kind, entity_key)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_ledger_source_sequence
                    ON sync_ledger(source_id, sequence);
                PRAGMA user_version = 1;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 1;
    }

    if version == 1 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sync_manifests (
                    snapshot_id TEXT PRIMARY KEY NOT NULL,
                    vehicle_id TEXT NOT NULL,
                    head_sequence INTEGER NOT NULL CHECK (head_sequence >= 0),
                    manifest_json BLOB NOT NULL
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_manifests_vehicle_head
                    ON sync_manifests(vehicle_id, head_sequence DESC);
                CREATE TABLE IF NOT EXISTS sync_packs (
                    sha256 TEXT PRIMARY KEY NOT NULL,
                    snapshot_id TEXT NOT NULL REFERENCES sync_manifests(snapshot_id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL CHECK (compressed_bytes > 0),
                    uncompressed_bytes INTEGER NOT NULL CHECK (uncompressed_bytes >= 100),
                    UNIQUE(snapshot_id, ordinal)
                ) STRICT;
                PRAGMA user_version = 2;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 2;
    }

    if version == 2 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS hub_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;
                -- The pre-v3 source table stays intact: it already anchors
                -- sync sequence history. This companion table gives collectors
                -- a stable, non-secret external identity without rewriting it.
                CREATE TABLE IF NOT EXISTS source_identities (
                    source_id TEXT PRIMARY KEY NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_kind TEXT NOT NULL,
                    source_key TEXT NOT NULL,
                    UNIQUE(source_kind, source_key),
                    CHECK(length(CAST(source_kind AS BLOB)) BETWEEN 1 AND 64),
                    CHECK(length(CAST(source_key AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS vehicles (
                    vehicle_id TEXT PRIMARY KEY NOT NULL,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_vehicle_key TEXT NOT NULL,
                    vin TEXT,
                    display_name TEXT,
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    last_seen_at_ms INTEGER NOT NULL CHECK(last_seen_at_ms >= created_at_ms),
                    UNIQUE(source_id, source_vehicle_key),
                    CHECK(length(CAST(source_vehicle_key AS BLOB)) BETWEEN 1 AND 256),
                    CHECK(vin IS NULL OR length(CAST(vin AS BLOB)) BETWEEN 1 AND 32),
                    CHECK(display_name IS NULL OR length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS vehicles_source_id
                    ON vehicles(source_id);
                CREATE TABLE IF NOT EXISTS raw_observations (
                    observation_id INTEGER PRIMARY KEY,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
                    received_at_ms INTEGER NOT NULL CHECK(received_at_ms >= 0),
                    payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256) = 32),
                    payload_json TEXT NOT NULL CHECK(json_valid(payload_json))
                        CHECK(length(CAST(payload_json AS BLOB)) <= 262144),
                    UNIQUE(source_id, vehicle_id, observed_at_ms, payload_sha256)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS raw_observations_vehicle_observed
                    ON raw_observations(vehicle_id, observed_at_ms, observation_id);
                CREATE TRIGGER IF NOT EXISTS raw_observations_match_vehicle_source
                BEFORE INSERT ON raw_observations
                FOR EACH ROW
                WHEN (SELECT source_id FROM vehicles WHERE vehicle_id = NEW.vehicle_id)
                     != NEW.source_id
                BEGIN
                    SELECT RAISE(ABORT, 'raw observation source and vehicle mismatch');
                END;
                CREATE TRIGGER IF NOT EXISTS raw_observations_append_only_update
                BEFORE UPDATE ON raw_observations
                FOR EACH ROW
                BEGIN
                    SELECT RAISE(ABORT, 'raw observations are append-only');
                END;
                CREATE TRIGGER IF NOT EXISTS raw_observations_append_only_delete
                BEFORE DELETE ON raw_observations
                FOR EACH ROW
                BEGIN
                    SELECT RAISE(ABORT, 'raw observations are append-only');
                END;
                PRAGMA user_version = 3;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 3;
    }

    if version == 3 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS pairing_challenges (
                    pairing_id TEXT PRIMARY KEY NOT NULL,
                    label TEXT NOT NULL,
                    secret_sha256 BLOB NOT NULL CHECK(length(secret_sha256) = 32),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > created_at_ms),
                    CHECK(length(CAST(label AS BLOB)) BETWEEN 1 AND 128)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS pairing_challenges_expiry
                    ON pairing_challenges(expires_at_ms);
                CREATE TABLE IF NOT EXISTS paired_devices (
                    device_id TEXT PRIMARY KEY NOT NULL,
                    display_name TEXT NOT NULL,
                    token_sha256 BLOB NOT NULL UNIQUE CHECK(length(token_sha256) = 32),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    last_authenticated_at_ms INTEGER,
                    CHECK(last_authenticated_at_ms IS NULL OR last_authenticated_at_ms >= created_at_ms),
                    CHECK(length(CAST(display_name AS BLOB)) BETWEEN 1 AND 128)
                ) STRICT;
                PRAGMA user_version = 4;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 4;
    }

    if version == 4 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS vehicle_lifecycle_state (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    last_observation_id INTEGER NOT NULL CHECK(last_observation_id >= 0),
                    open_session_json BLOB NOT NULL
                        CHECK(length(open_session_json) BETWEEN 2 AND 65536),
                    quarantined INTEGER NOT NULL DEFAULT 0 CHECK(quarantined IN (0, 1)),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS materialised_drives (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    drive_json TEXT NOT NULL CHECK(json_valid(drive_json)),
                    PRIMARY KEY (vehicle_id, drive_id)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS materialised_positions (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    position_id INTEGER NOT NULL CHECK(position_id > 0),
                    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    position_json TEXT NOT NULL CHECK(json_valid(position_json)),
                    PRIMARY KEY (vehicle_id, position_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS materialised_positions_drive
                    ON materialised_positions(vehicle_id, drive_id);
                CREATE TABLE IF NOT EXISTS materialised_charges (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    charge_id INTEGER NOT NULL CHECK(charge_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    charge_json TEXT NOT NULL CHECK(json_valid(charge_json)),
                    PRIMARY KEY (vehicle_id, charge_id)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS materialised_charge_samples (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    sample_id INTEGER NOT NULL CHECK(sample_id > 0),
                    charge_id INTEGER NOT NULL CHECK(charge_id > 0),
                    sample_json TEXT NOT NULL CHECK(json_valid(sample_json)),
                    PRIMARY KEY (vehicle_id, sample_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS materialised_charge_samples_charge
                    ON materialised_charge_samples(vehicle_id, charge_id);
                PRAGMA user_version = 5;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 5;
    }

    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(StoreError::UnsupportedSchema(version))
    }
}

fn schema_version(connection: &Connection) -> Result<i32, StoreError> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(StoreError::Query)
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("cannot create data directory: {0}")]
    CreateDataDir(std::io::Error),
    #[error("cannot create packs directory: {0}")]
    CreatePacksDir(std::io::Error),
    #[error("cannot open hub database: {0}")]
    Open(rusqlite::Error),
    #[error("cannot configure hub database: {0}")]
    Configure(rusqlite::Error),
    #[error("cannot migrate hub database: {0}")]
    Migrate(rusqlite::Error),
    #[error("database query failed: {0}")]
    Query(rusqlite::Error),
    #[error("cannot begin local transaction: {0}")]
    Begin(rusqlite::Error),
    #[error("cannot publish sync manifest: {0}")]
    PublishManifest(rusqlite::Error),
    #[error("cannot register source: {0}")]
    RegisterSource(rusqlite::Error),
    #[error("cannot register vehicle: {0}")]
    RegisterVehicle(rusqlite::Error),
    #[error("cannot create pairing invitation: {0}")]
    CreatePairing(rusqlite::Error),
    #[error("cannot claim pairing invitation: {0}")]
    ClaimPairing(rusqlite::Error),
    #[error("cannot append raw observation: {0}")]
    AppendObservation(rusqlite::Error),
    #[error("cannot initialise Hub installation identity: {0}")]
    InstallationIdentity(rusqlite::Error),
    #[error("cannot serialize sync manifest: {0}")]
    SerializeManifest(serde_json::Error),
    #[error("cannot deserialize sync manifest: {0}")]
    DeserializeManifest(serde_json::Error),
    #[error("cannot serialize raw observation: {0}")]
    SerializeObservation(serde_json::Error),
    #[error("invalid sync manifest: {0}")]
    Manifest(crate::protocol::ProtocolError),
    #[error("sync sequence does not fit SQLite signed integer")]
    SequenceTooLarge,
    #[error("sync sequence is exhausted")]
    SequenceExhausted,
    #[error("pack size does not fit SQLite signed integer")]
    PackSizeTooLarge,
    #[error("stored pack path is not canonical")]
    UnsafeStoredPackPath,
    #[error("{0} must not be empty")]
    EmptyIdentity(&'static str),
    #[error("{field} is {actual} bytes; maximum is {maximum}")]
    IdentityTooLong {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("{0} must not contain control characters")]
    IdentityControlCharacter(&'static str),
    #[error("source kind must be lowercase ASCII letters, digits, hyphens, or underscores")]
    InvalidSourceKind,
    #[error("{0} must be an epoch timestamp in milliseconds")]
    NegativeTimestamp(&'static str),
    #[error("pairing invitation expiry must be later than its creation time")]
    InvalidPairingExpiry,
    #[error("pairing invitation was rejected")]
    PairingRejected,
    #[error("source id must not be nil")]
    NilSourceId,
    #[error("vehicle id must not be nil")]
    NilVehicleId,
    #[error("raw observation payload must be a JSON object")]
    ObservationMustBeObject,
    #[error("raw observation is {actual} bytes; maximum is {maximum}")]
    ObservationTooLarge { actual: usize, maximum: usize },
    #[error("raw observation is missing after a successful insert")]
    ObservationMissingAfterInsert,
    #[error("raw observation query limit {actual} must be between 1 and {maximum}")]
    InvalidObservationQueryLimit { actual: u32, maximum: u32 },
    #[error("raw observation query time range is empty or reversed")]
    InvalidObservationQueryRange,
    #[error("unknown source {0}")]
    UnknownSource(Uuid),
    #[error("unknown vehicle {0}")]
    UnknownVehicle(Uuid),
    #[error("vehicle {vehicle_id} does not belong to source {source_id}")]
    VehicleSourceMismatch { vehicle_id: Uuid, source_id: Uuid },
    #[error("stored vehicle identity {actual} differs from expected identity {expected}")]
    VehicleIdentityMismatch { expected: Uuid, actual: Uuid },
    #[error("stored {0} is not a valid UUID")]
    InvalidStoredUuid(&'static str),
    #[error("stored source generation is invalid")]
    InvalidStoredGeneration,
    #[error("unsupported hub schema version {0}")]
    UnsupportedSchema(i32),
    #[error("database integrity check failed: {0}")]
    Integrity(String),
    #[error("lifecycle car id must be positive")]
    InvalidLifecycleCarId,
    #[error("lifecycle observation cursor is invalid")]
    InvalidLifecycleCursor,
    #[error("lifecycle open-session payload is invalid")]
    InvalidLifecycleSession,
    #[error("cannot write lifecycle history: {0}")]
    LifecycleWrite(rusqlite::Error),
    #[error("cannot serialize lifecycle history row: {0}")]
    SerializeLifecycleRow(serde_json::Error),
    #[error("cannot deserialize lifecycle history row: {0}")]
    DeserializeLifecycleRow(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        CursorClaims, CursorKey, MirrorTable, OpaqueCursor, PackCompression, PackFormat,
        ProtocolVersion, SchemaVersion, SequenceRange, TransferMode,
    };

    #[test]
    fn initializes_a_checked_wal_database() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        store.quick_check().expect("database passes quick check");
        assert!(store.database_path().exists());
        assert!(store.packs_dir().is_dir());

        let connection = store.open().expect("reopen store");
        let journal: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode");
        assert_eq!(journal, "wal");
        let application_id: i32 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .expect("application id");
        assert_eq!(application_id, APPLICATION_ID);
        assert_eq!(
            schema_version(&connection).expect("schema version"),
            SCHEMA_VERSION
        );
        assert_eq!(
            store.sqlite_version().expect("SQLite version"),
            VENDORED_SQLITE_VERSION
        );
        assert!(!store.installation_id().expect("installation ID").is_nil());
    }

    #[test]
    fn publishes_and_loads_a_canonical_manifest_catalog() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let manifest = test_manifest();

        store.publish_manifest(&manifest).expect("publish manifest");
        let loaded = store
            .manifest_for_vehicle(manifest.vehicle_id)
            .expect("load manifest")
            .expect("manifest exists");
        assert_eq!(loaded, manifest);

        let pack = store
            .pack_for_digest(manifest.chunks[0].sha256)
            .expect("load pack")
            .expect("pack exists");
        assert_eq!(pack.compressed_bytes, manifest.chunks[0].compressed_bytes);
        assert!(pack.path.starts_with(store.packs_dir()));
    }

    #[test]
    fn source_and_vehicle_ids_are_stable_across_re_registration() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let descriptor = SourceDescriptor::new("tesla_owner_api", "account-opaque-id");
        let source = store
            .register_source(&descriptor, 1_000)
            .expect("source registers");
        let same_source = store
            .register_source(&descriptor, 2_000)
            .expect("source re-registers");
        assert_eq!(source, same_source);
        assert_eq!(source.created_at_ms, 1_000);

        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor {
                    source_id: source.source_id,
                    source_vehicle_key: "vehicle-fleet-id".into(),
                    vin: Some("5YJTESTVIN1234567".into()),
                    display_name: Some("Road car".into()),
                },
                3_000,
            )
            .expect("vehicle registers");
        let same_vehicle = store
            .register_vehicle(
                &VehicleDescriptor {
                    source_id: source.source_id,
                    source_vehicle_key: "vehicle-fleet-id".into(),
                    vin: None,
                    display_name: Some("Renamed road car".into()),
                },
                4_000,
            )
            .expect("vehicle re-registers");
        assert_eq!(same_vehicle.vehicle_id, vehicle.vehicle_id);
        assert_eq!(same_vehicle.created_at_ms, 3_000);
        assert_eq!(same_vehicle.last_seen_at_ms, 4_000);
        assert_eq!(same_vehicle.vin.as_deref(), Some("5YJTESTVIN1234567"));
        assert_eq!(
            same_vehicle.display_name.as_deref(),
            Some("Renamed road car")
        );
    }

    #[test]
    fn accepts_a_deterministic_vehicle_id_and_allocates_snapshot_markers() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let source = store
            .register_source(&SourceDescriptor::new("teslamate", "test-source"), 1_000)
            .expect("source registers");
        let expected_vehicle_id = Uuid::from_u128(7);
        let descriptor = VehicleDescriptor {
            source_id: source.source_id,
            source_vehicle_key: "vin:5YJTESTVIN1234567".into(),
            vin: Some("5YJTESTVIN1234567".into()),
            display_name: Some("Road car".into()),
        };
        let vehicle = store
            .register_vehicle_with_id(&descriptor, 2_000, expected_vehicle_id)
            .expect("vehicle registers");
        assert_eq!(vehicle.vehicle_id, expected_vehicle_id);
        assert_eq!(
            store
                .next_full_snapshot_sequence(vehicle.vehicle_id)
                .expect("first marker"),
            1
        );

        let conflicting = store
            .register_vehicle_with_id(&descriptor, 3_000, Uuid::from_u128(8))
            .expect_err("different stable identity must fail");
        assert!(matches!(
            conflicting,
            StoreError::VehicleIdentityMismatch { .. }
        ));
    }

    #[test]
    fn pairing_is_single_use_and_persists_only_token_hashes() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let invitation = store
            .create_pairing("iPhone", 1_000, 61_000)
            .expect("pairing creates");
        assert!(format!("{invitation:?}").contains("[redacted]"));

        let access = store
            .claim_pairing(
                invitation.pairing_id,
                invitation.secret(),
                "Bolyki iPhone",
                2_000,
            )
            .expect("claim succeeds");
        assert_eq!(
            format!("{:?}", access.access_token),
            "DeviceAccessToken([redacted])"
        );
        let authenticated = store
            .authenticate_device(access.access_token.as_bearer())
            .expect("device lookup")
            .expect("device exists");
        assert_eq!(authenticated.device_id, access.device_id);
        assert_eq!(authenticated.display_name, "Bolyki iPhone");
        assert!(
            store
                .claim_pairing(
                    invitation.pairing_id,
                    invitation.secret(),
                    "Second phone",
                    3_000,
                )
                .is_err()
        );

        let connection = store.open().expect("open database");
        let challenge_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| {
                row.get(0)
            })
            .expect("challenge count");
        assert_eq!(challenge_count, 0);
        let stored_token_hash: Vec<u8> = connection
            .query_row("SELECT token_sha256 FROM paired_devices", [], |row| {
                row.get(0)
            })
            .expect("token digest");
        assert_ne!(
            stored_token_hash,
            access.access_token.as_bearer().as_bytes()
        );
    }

    #[test]
    fn pairing_claims_fail_closed_when_expired_or_malformed() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store initializes");
        let invitation = store
            .create_pairing("iPad", 1_000, 2_000)
            .expect("pairing creates");
        assert!(matches!(
            store.claim_pairing(invitation.pairing_id, "not-a-token", "iPad", 1_500),
            Err(StoreError::PairingRejected)
        ));
        assert!(matches!(
            store.claim_pairing(invitation.pairing_id, invitation.secret(), "iPad", 2_000),
            Err(StoreError::PairingRejected)
        ));
        assert!(
            store
                .authenticate_device("not-a-token")
                .expect("malformed token lookup")
                .is_none()
        );
    }

    #[test]
    fn appends_canonical_json_once_and_retries_idempotently() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let (source, vehicle) = test_registered_vehicle(&store);
        let input = ObservationInput {
            source_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            observed_at_ms: 10_000,
            payload: serde_json::json!({"speed": 0, "battery_level": 80}),
        };

        let first = store
            .append_observation(&input, 10_010)
            .expect("first observation");
        let retry = store
            .append_observation(&input, 99_999)
            .expect("idempotent retry");
        assert!(first.inserted);
        assert!(!retry.inserted);
        assert_eq!(retry.observation, first.observation);
        assert_eq!(first.observation.received_at_ms, 10_010);
        let canonical = serde_json::to_vec(&input.payload).expect("JSON serializes");
        assert_eq!(
            first.observation.payload_sha256,
            Sha256Digest::of_bytes(&canonical)
        );

        let connection = store.open().expect("open database");
        assert!(
            connection
                .execute(
                    "UPDATE raw_observations SET payload_json = '{}' WHERE observation_id = ?1",
                    params![first.observation.observation_id],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "DELETE FROM raw_observations WHERE observation_id = ?1",
                    params![first.observation.observation_id],
                )
                .is_err()
        );

        let observations = store
            .observations_for_vehicle(vehicle.vehicle_id, ObservationQuery::from_start(10))
            .expect("read observations");
        assert_eq!(observations, vec![first.observation]);
    }

    #[test]
    fn observations_are_time_ordered_and_query_is_bounded() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let (source, vehicle) = test_registered_vehicle(&store);
        for (observed_at_ms, value) in [(3_000, 3), (1_000, 1), (2_000, 2)] {
            store
                .append_observation(
                    &ObservationInput {
                        source_id: source.source_id,
                        vehicle_id: vehicle.vehicle_id,
                        observed_at_ms,
                        payload: serde_json::json!({"value": value}),
                    },
                    observed_at_ms + 1,
                )
                .expect("append observation");
        }
        let first_two = store
            .observations_for_vehicle(vehicle.vehicle_id, ObservationQuery::from_start(2))
            .expect("bounded page");
        assert_eq!(
            first_two
                .iter()
                .map(|row| row.observed_at_ms)
                .collect::<Vec<_>>(),
            vec![1_000, 2_000]
        );
        let filtered = store
            .observations_for_vehicle(
                vehicle.vehicle_id,
                ObservationQuery {
                    from_observed_at_ms: Some(2_000),
                    until_observed_at_ms: Some(3_000),
                    limit: 10,
                },
            )
            .expect("time query");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].observed_at_ms, 2_000);

        let error = store
            .observations_for_vehicle(
                vehicle.vehicle_id,
                ObservationQuery::from_start(MAX_OBSERVATION_QUERY_LIMIT + 1),
            )
            .expect_err("over-large query rejected");
        assert!(matches!(
            error,
            StoreError::InvalidObservationQueryLimit { .. }
        ));
    }

    #[test]
    fn rejects_wrong_source_non_object_and_oversized_observations() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let (source, vehicle) = test_registered_vehicle(&store);
        let other_source = store
            .register_source(
                &SourceDescriptor::new("teslamate_import", "migration-a"),
                1_001,
            )
            .expect("second source");
        let mismatch = store
            .append_observation(
                &ObservationInput {
                    source_id: other_source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: 2_000,
                    payload: serde_json::json!({"status": "online"}),
                },
                2_001,
            )
            .expect_err("vehicle cannot be written by another source");
        assert!(matches!(mismatch, StoreError::VehicleSourceMismatch { .. }));

        let non_object = store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: 2_000,
                    payload: serde_json::json!(["a response batch is not one observation"]),
                },
                2_001,
            )
            .expect_err("array rejected");
        assert!(matches!(non_object, StoreError::ObservationMustBeObject));

        let oversized = store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: 2_000,
                    payload: serde_json::json!({"blob": "x".repeat(MAX_RAW_OBSERVATION_BYTES)}),
                },
                2_001,
            )
            .expect_err("oversized response rejected before database mutation");
        assert!(matches!(oversized, StoreError::ObservationTooLarge { .. }));
        assert!(
            store
                .observations_for_vehicle(vehicle.vehicle_id, ObservationQuery::from_start(10))
                .expect("read observation history")
                .is_empty()
        );
    }

    #[test]
    fn upgrades_a_v2_database_without_losing_existing_tables() {
        let temp = tempfile::tempdir().expect("temp directory");
        let database_path = temp.path().join("hub.sqlite");
        let legacy_source_id = Uuid::new_v4();
        let connection = Connection::open(&database_path).expect("open v2 database");
        connection
            .execute_batch(
                "
                CREATE TABLE sources (
                    source_id TEXT PRIMARY KEY NOT NULL,
                    source_kind TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    created_at_ms INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE sync_manifests (
                    snapshot_id TEXT PRIMARY KEY NOT NULL,
                    vehicle_id TEXT NOT NULL,
                    head_sequence INTEGER NOT NULL CHECK (head_sequence >= 0),
                    manifest_json BLOB NOT NULL
                ) STRICT;
                CREATE TABLE sync_packs (
                    sha256 TEXT PRIMARY KEY NOT NULL,
                    snapshot_id TEXT NOT NULL REFERENCES sync_manifests(snapshot_id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL CHECK (compressed_bytes > 0),
                    uncompressed_bytes INTEGER NOT NULL CHECK (uncompressed_bytes >= 100),
                    UNIQUE(snapshot_id, ordinal)
                ) STRICT;
                PRAGMA user_version = 2;
                ",
            )
            .expect("make v2 schema");
        connection
            .execute(
                "INSERT INTO sources (source_id, source_kind, generation, created_at_ms) \
                 VALUES (?1, 'legacy', 1, 1)",
                params![legacy_source_id.to_string()],
            )
            .expect("legacy source");
        drop(connection);

        let store = HubStore::initialize(temp.path()).expect("migrate v2 store");
        let migrated = store.open().expect("open migrated store");
        assert_eq!(
            schema_version(&migrated).expect("schema version"),
            SCHEMA_VERSION
        );
        let legacy_count: i64 = migrated
            .query_row("SELECT COUNT(*) FROM sources", [], |row| row.get(0))
            .expect("legacy source preserved");
        assert_eq!(legacy_count, 1);
        let raw_table_count: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'raw_observations'",
                [],
                |row| row.get(0),
            )
            .expect("raw table exists");
        assert_eq!(raw_table_count, 1);
    }

    #[test]
    fn upgrades_a_v1_database_through_v2_and_v3() {
        let temp = tempfile::tempdir().expect("temp directory");
        let database_path = temp.path().join("hub.sqlite");
        let connection = Connection::open(&database_path).expect("open v1 database");
        connection
            .execute_batch(
                "
                CREATE TABLE hub_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;
                CREATE TABLE sources (
                    source_id TEXT PRIMARY KEY NOT NULL,
                    source_kind TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    created_at_ms INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE sync_ledger (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    sequence INTEGER NOT NULL CHECK (sequence >= 1),
                    entity_kind TEXT NOT NULL,
                    entity_key TEXT NOT NULL,
                    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
                    committed_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (source_id, sequence, entity_kind, entity_key)
                ) STRICT;
                PRAGMA user_version = 1;
                ",
            )
            .expect("make v1 schema");
        drop(connection);

        let store = HubStore::initialize(temp.path()).expect("migrate v1 store");
        let migrated = store.open().expect("open migrated store");
        assert_eq!(
            schema_version(&migrated).expect("schema version"),
            SCHEMA_VERSION
        );
        for table in [
            "sync_manifests",
            "sync_packs",
            "source_identities",
            "vehicles",
            "raw_observations",
        ] {
            let found: i64 = migrated
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .expect("migrated table query");
            assert_eq!(found, 1, "missing table {table}");
        }
    }

    fn test_registered_vehicle(store: &HubStore) -> (SourceRecord, VehicleRecord) {
        let source = store
            .register_source(
                &SourceDescriptor::new("tesla_owner_api", "account-test"),
                1_000,
            )
            .expect("source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "vehicle-test"),
                1_001,
            )
            .expect("vehicle");
        (source, vehicle)
    }

    fn test_manifest() -> SyncManifest {
        let installation_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let vehicle_id = Uuid::new_v4();
        let digest = Sha256Digest::of_bytes(b"catalog test pack");
        let cursor = OpaqueCursor::issue(
            &CursorKey::from_bytes([7; 32]),
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: SchemaVersion { major: 1, minor: 0 },
                installation_id,
                account_id,
                vehicle_id,
                generation: 1,
                sequence: 9,
            },
        )
        .expect("cursor");
        let pack = TransportPack {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            schema: SchemaVersion { major: 1, minor: 0 },
            format: PackFormat::SqliteTransport,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(digest),
            sha256: digest,
            compressed_bytes: 100,
            uncompressed_bytes: 100,
            row_count: 1,
            sequence: SequenceRange {
                from_exclusive: 9,
                to_inclusive: 9,
            },
            tables: vec![MirrorTable::Vehicle],
        };
        SyncManifest {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: SchemaVersion { major: 1, minor: 0 },
            installation_id,
            account_id,
            vehicle_id,
            generation: 1,
            snapshot_id: pack.snapshot_id,
            mode: TransferMode::FullSnapshot,
            base_sequence: 9,
            head_sequence: 9,
            chunk_count: 1,
            total_compressed_bytes: pack.compressed_bytes,
            total_uncompressed_bytes: pack.uncompressed_bytes,
            total_rows: pack.row_count,
            chunks: vec![pack],
            terminal_cursor: cursor,
        }
    }
}
