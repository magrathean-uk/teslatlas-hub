use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::sync::{Arc, Mutex};

use rustix::fs::{FlockOperation, flock};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, backup::Backup,
    params,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    hub_pack::{
        ProjectionCar, ProjectionCarSettings, ProjectionCarSettingsPatch, ProjectionDelta,
        ProjectionDeltaEntity, ProjectionDrive, ProjectionPackError, ProjectionPosition,
        ProjectionSnapshot, ProjectionPackWriter,
        ProjectionBinding, ProjectionPackRequest, ProjectionTombstone,
    },
    mqtt::{MqttPublication, MqttProjectError, MqttQos, MqttSummary, project_summary},
    protocol::{
        CursorKey, LineageBase, LineageCapability, LineageDelta, LineageManifestV2,
        OpaqueCursor, Sha256Digest, SyncManifest, TransportPack, SequenceRange,
        LINEAGE_PROTOCOL_V2,
    },
    teslamate_projection::TeslaMateOpenSession,
};

pub const APPLICATION_ID: i32 = 0x5441_4855; // TAHU
pub const SCHEMA_VERSION: i32 = 33;
pub const VENDORED_SQLITE_VERSION: &str = "3.53.4";

/// Hard upper bound for one persisted source response. A collector must split
/// high-volume telemetry into individual observations rather than retaining an
/// unbounded response in memory or in the Hub database.
pub const MAX_RAW_OBSERVATION_BYTES: usize = 256 * 1024;

/// The read API is deliberately capped so callers cannot accidentally turn a
/// history query into an all-memory transfer.
pub const MAX_OBSERVATION_QUERY_LIMIT: u32 = 10_000;
/// Request-ledger reads are metadata-only and bounded independently from raw
/// observation reads so proof commands cannot accidentally load an unbounded
/// audit history into memory.
pub const MAX_OUTBOUND_REQUEST_QUERY_LIMIT: u32 = 10_000;
/// Completed request receipts are eligible for normal retention cleanup after
/// this period. Unresolved `started` receipts are never deleted automatically.
pub const OUTBOUND_REQUEST_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
/// The ledger rejects a new request before network I/O when completed-receipt
/// cleanup cannot make room below this bound without deleting an unresolved row.
pub const MAX_OUTBOUND_REQUEST_RECEIPTS: i64 = 100_000;
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
const PUBLICATION_GATE_RETRY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamObservationResult {
    Committed { observation_id: i64 },
    IgnoredDuplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFaultPoint {
    RawInsert,
    LifecycleWrite,
    WatermarkUpdate,
    Commit,
}

#[cfg(test)]
impl StreamFaultPoint {
    const fn label(self) -> &'static str {
        match self {
            Self::RawInsert => "raw_insert",
            Self::LifecycleWrite => "lifecycle_write",
            Self::WatermarkUpdate => "watermark_update",
            Self::Commit => "commit",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HubStore {
    database_path: PathBuf,
    packs_dir: PathBuf,
    publication_lock_path: PathBuf,
    #[cfg(test)]
    stream_fault: Arc<Mutex<Option<StreamFaultPoint>>>,
}

/// Process-wide, advisory writer gate for workflows that mutate local
/// lifecycle state and then publish a full snapshot. The file descriptor owns
/// the lock and releases it automatically when the outer workflow returns.
///
/// This is intentionally acquired only by outer publication workflows. Lower
/// level catalogue and sequence methods remain ungated so one workflow cannot
/// re-enter the lock while it is already building packs.
#[derive(Debug)]
pub(crate) struct PublicationGate {
    _file: File,
}

#[derive(Debug, Clone)]
pub struct MqttSummaryRevision {
    pub vehicle_id: Uuid,
    pub revision: i64,
    pub publications: Vec<MqttPublication>,
    pub healthy_clear_delivered: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct MqttDeliveryClaim {
    pub vehicle_id: Uuid,
    pub field: String,
    pub publication: MqttPublication,
    pub fingerprint: String,
    pub revision: i64,
    pub startup_clear: bool,
}

impl HubStore {
    pub fn initialize(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir).map_err(StoreError::CreateDataDir)?;
        fs::set_permissions(data_dir, fs::Permissions::from_mode(0o700))
            .map_err(StoreError::ProtectDataDir)?;
        let packs_dir = data_dir.join("packs");
        fs::create_dir_all(&packs_dir).map_err(StoreError::CreatePacksDir)?;
        fs::set_permissions(&packs_dir, fs::Permissions::from_mode(0o700))
            .map_err(StoreError::ProtectPacksDir)?;

        let store = Self {
            database_path: data_dir.join("hub.sqlite"),
            packs_dir,
            publication_lock_path: data_dir.join(".publication.lock"),
            #[cfg(test)]
            stream_fault: Arc::new(Mutex::new(None)),
        };
        let connection = store.open()?;
        migrate(&connection)?;
        ensure_installation_id(&connection)?;
        // Never discard a staged import while another process owns its
        // publication workflow. A busy gate makes startup retryable instead.
        let _publication_gate = store.try_acquire_publication_gate()?;
        cleanup_abandoned_import_generations(&connection)?;
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

    /// Open an existing Hub catalogue without creating directories, migrating
    /// schema, changing pragmas that write to SQLite, or otherwise mutating
    /// Hub state. Callers must handle an absent or stale database explicitly.
    pub fn open_read_only(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref();
        let store = Self {
            database_path: data_dir.join("hub.sqlite"),
            packs_dir: data_dir.join("packs"),
            publication_lock_path: data_dir.join(".publication.lock"),
            #[cfg(test)]
            stream_fault: Arc::new(Mutex::new(None)),
        };
        let _connection = store.open_read_only_connection()?;
        Ok(store)
    }

    fn open_read_only_connection(&self) -> Result<Connection, StoreError> {
        let connection = Connection::open_with_flags(
            &self.database_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::Open)?;
        configure_read_only(&connection)?;
        Ok(connection)
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

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
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.publication_lock_path)
            .map_err(StoreError::OpenPublicationGate)?;
        fs::set_permissions(&self.publication_lock_path, fs::Permissions::from_mode(0o600))
            .map_err(StoreError::ProtectPublicationGate)?;
        Ok(file)
    }

    /// Store the latest typed MQTT projection and coalesce delivery work per
    /// field. This is deliberately separate from export outbox state.
    pub fn enqueue_mqtt_summary(
        &self,
        namespace: Option<&str>,
        summary: &MqttSummary,
        enabled: bool,
        updated_at_ms: i64,
    ) -> Result<Option<i64>, StoreError> {
        if !enabled {
            return Ok(None);
        }
        if summary.vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        validate_timestamp("MQTT summary updated_at_ms", updated_at_ms)?;
        let publications = project_summary(namespace, summary).map_err(StoreError::MqttProjection)?;
        let encoded = serde_json::to_string(&publications).map_err(StoreError::SerializeMqtt)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let vehicle_key = summary.vehicle_id.to_string();
        let previous: Option<(i64, bool)> = transaction
            .query_row(
                "SELECT revision, healthy_clear_delivered
                 FROM mqtt_summary_revisions WHERE vehicle_id = ?1",
                params![vehicle_key],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()
            .map_err(StoreError::MqttDelivery)?;
        let revision = previous
            .map(|(revision, _)| revision)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(StoreError::MqttRevisionExhausted)?;
        transaction
            .execute(
                "INSERT INTO mqtt_summary_revisions(
                    vehicle_id, revision, fields_json, healthy_clear_delivered, updated_at_ms
                 ) VALUES (?1, ?2, ?3, COALESCE((SELECT healthy_clear_delivered
                    FROM mqtt_summary_revisions WHERE vehicle_id = ?1), 0), ?4)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    revision = excluded.revision,
                    fields_json = excluded.fields_json,
                    updated_at_ms = excluded.updated_at_ms",
                params![vehicle_key, revision, encoded, updated_at_ms],
            )
            .map_err(StoreError::MqttDelivery)?;
        let healthy_clear_delivered = previous.is_some_and(|(_, delivered)| delivered);
        sync_mqtt_publications_in_transaction(
            &transaction,
            summary.vehicle_id,
            revision,
            &publications,
            healthy_clear_delivered,
        )?;
        transaction.commit().map_err(StoreError::MqttDelivery)?;
        Ok(Some(revision))
    }

    pub fn load_mqtt_summary(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<MqttSummaryRevision>, StoreError> {
        let connection = self.open()?;
        connection
            .query_row(
                "SELECT revision, fields_json, healthy_clear_delivered, updated_at_ms
                 FROM mqtt_summary_revisions WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| {
                    let encoded: String = row.get(1)?;
                    let publications = serde_json::from_str(&encoded)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?;
                    Ok(MqttSummaryRevision {
                        vehicle_id,
                        revision: row.get(0)?,
                        publications,
                        healthy_clear_delivered: row.get::<_, i64>(2)? != 0,
                        updated_at_ms: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::MqttDelivery)
    }

    /// Claim at most ten independent fields. A caller may deliver this batch
    /// concurrently; no broker work is performed while SQLite is locked.
    pub fn claim_mqtt_deliveries(
        &self,
        now_ms: i64,
        maximum: usize,
    ) -> Result<Vec<MqttDeliveryClaim>, StoreError> {
        validate_timestamp("MQTT claim now_ms", now_ms)?;
        let maximum = maximum.min(crate::mqtt::MQTT_MAX_IN_FLIGHT);
        if maximum == 0 {
            return Ok(Vec::new());
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let mut statement = transaction
            .prepare(
                "SELECT vehicle_id, field, topic, payload, fingerprint, qos, retain,
                        pending_revision, phase
                 FROM mqtt_delivery_state
                 WHERE pending = 1 AND claimed_until_ms <= ?1
                 ORDER BY CASE WHEN phase = 1 THEN 0 ELSE 1 END,
                          pending_revision, vehicle_id, field
                 LIMIT ?2",
            )
            .map_err(StoreError::MqttDelivery)?;
        let rows = statement
            .query_map(params![now_ms, i64::try_from(maximum).unwrap_or(10)], |row| {
                let vehicle: String = row.get(0)?;
                let vehicle_id = Uuid::parse_str(&vehicle)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                let qos: i64 = row.get(5)?;
                if qos != i64::from(crate::mqtt::MQTT_QOS_AT_LEAST_ONCE) {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                Ok(MqttDeliveryClaim {
                    vehicle_id,
                    field: row.get(1)?,
                    publication: MqttPublication {
                        topic: row.get(2)?,
                        payload: row.get(3)?,
                        qos: MqttQos::AtLeastOnce,
                        retain: row.get::<_, i64>(6)? != 0,
                    },
                    fingerprint: row.get(4)?,
                    revision: row.get(7)?,
                    startup_clear: row.get::<_, i64>(8)? != 0,
                })
            })
            .map_err(StoreError::MqttDelivery)?;
        let claims: Result<Vec<_>, _> = rows.collect();
        drop(statement);
        let mut claims = claims.map_err(StoreError::MqttDelivery)?;
        let lease_until_ms = now_ms.saturating_add(60_000);
        for claim in &claims {
            transaction
                .execute(
                    "UPDATE mqtt_delivery_state
                     SET claimed_until_ms = ?1, attempts = attempts + 1
                     WHERE vehicle_id = ?2 AND field = ?3 AND fingerprint = ?4
                       AND pending = 1",
                    params![lease_until_ms, claim.vehicle_id.to_string(), claim.field, claim.fingerprint],
                )
                .map_err(StoreError::MqttDelivery)?;
        }
        transaction.commit().map_err(StoreError::MqttDelivery)?;
        claims.shrink_to_fit();
        Ok(claims)
    }

    pub fn complete_mqtt_delivery(&self, claim: &MqttDeliveryClaim) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let changed = transaction
            .execute(
                "UPDATE mqtt_delivery_state
                 SET delivered_fingerprint = fingerprint, pending = 0,
                     claimed_until_ms = 0, last_error = NULL
                 WHERE vehicle_id = ?1 AND field = ?2 AND fingerprint = ?3
                   AND pending = 1",
                params![claim.vehicle_id.to_string(), claim.field, claim.fingerprint],
            )
            .map_err(StoreError::MqttDelivery)?;
        if changed != 0 && claim.startup_clear {
            transaction
                .execute(
                    "UPDATE mqtt_summary_revisions
                     SET healthy_clear_delivered = 1
                     WHERE vehicle_id = ?1",
                    params![claim.vehicle_id.to_string()],
                )
                .map_err(StoreError::MqttDelivery)?;
            let (revision, encoded): (i64, String) = transaction
                .query_row(
                    "SELECT revision, fields_json FROM mqtt_summary_revisions
                     WHERE vehicle_id = ?1",
                    params![claim.vehicle_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(StoreError::MqttDelivery)?;
            let publications: Vec<MqttPublication> =
                serde_json::from_str(&encoded).map_err(StoreError::DeserializeMqtt)?;
            sync_mqtt_publications_in_transaction(
                &transaction,
                claim.vehicle_id,
                revision,
                &publications,
                true,
            )?;
        }
        transaction.commit().map_err(StoreError::MqttDelivery)
    }

    pub fn fail_mqtt_delivery(
        &self,
        claim: &MqttDeliveryClaim,
        error: &str,
    ) -> Result<(), StoreError> {
        let safe_error = if error.is_empty() || error.len() > 128 || error.chars().any(char::is_control) {
            "publisher_failed"
        } else {
            error
        };
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE mqtt_delivery_state
                 SET claimed_until_ms = 0, last_error = ?1
                 WHERE vehicle_id = ?2 AND field = ?3 AND fingerprint = ?4
                   AND pending = 1",
                params![safe_error, claim.vehicle_id.to_string(), claim.field, claim.fingerprint],
            )
            .map_err(StoreError::MqttDelivery)?;
        Ok(())
    }

    /// Fail the publication that lost transport and immediately release the
    /// later claims from the same ordered batch. They were never submitted to
    /// MQTT, so retaining their lease would delay their at-least-once retry.
    pub fn fail_mqtt_delivery_and_release_following(
        &self,
        claim: &MqttDeliveryClaim,
        following: &[MqttDeliveryClaim],
        error: &str,
    ) -> Result<(), StoreError> {
        let safe_error = if error.is_empty() || error.len() > 128 || error.chars().any(char::is_control) {
            "publisher_failed"
        } else {
            error
        };
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "UPDATE mqtt_delivery_state
                 SET claimed_until_ms = 0, last_error = ?1
                 WHERE vehicle_id = ?2 AND field = ?3 AND fingerprint = ?4
                   AND pending = 1",
                params![safe_error, claim.vehicle_id.to_string(), claim.field, claim.fingerprint],
            )
            .map_err(StoreError::MqttDelivery)?;
        for later_claim in following {
            transaction
                .execute(
                    "UPDATE mqtt_delivery_state
                     SET claimed_until_ms = 0
                     WHERE vehicle_id = ?1 AND field = ?2 AND fingerprint = ?3
                       AND pending = 1",
                    params![
                        later_claim.vehicle_id.to_string(),
                        later_claim.field,
                        later_claim.fingerprint,
                    ],
                )
                .map_err(StoreError::MqttDelivery)?;
        }
        transaction.commit().map_err(StoreError::MqttDelivery)
    }

    pub fn upsert_car_settings(
        &self,
        vehicle_id: Uuid,
        car_id: i64,
        settings: &ProjectionCarSettings,
    ) -> Result<(), StoreError> {
        let payload = serde_json::to_string(settings).map_err(StoreError::SerializeLifecycleRow)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
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
        let current_car: Option<String> = transaction
            .query_row(
                "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if let Some(current_car) = current_car {
            let mut car: ProjectionCar = serde_json::from_str(&current_car)
                .map_err(StoreError::DeserializeLifecycleRow)?;
            car.settings = settings.clone();
            let car_json = serde_json::to_string(&car)
                .map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_cars SET car_json = ?1 WHERE vehicle_id = ?2",
                    params![car_json, vehicle_id.to_string()],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        record_sync_mutation_in_transaction(
            &transaction,
            vehicle_id,
            "car_setting",
            car_id,
            car_id,
            "upsert",
            &payload,
        )?;
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
        let Some(suspend_min) = crate::hub_pack::teslamate_suspend_min_default(
            model,
            trim_badging,
            marketing_name,
        ) else {
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
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(destination);
                Err(StoreError::Backup(error))
            }
        }
    }

    /// Create a complete Hub-owned restore directory. The catalogue is copied
    /// first through SQLite's online backup API; immutable packs are then
    /// copied from the exact referenced set in that copied catalogue.
    pub fn backup_to(&self, destination: &Path) -> Result<(), StoreError> {
        if destination.exists() {
            return Err(StoreError::BackupDestinationExists(
                destination.to_path_buf(),
            ));
        }
        fs::create_dir(destination).map_err(StoreError::CreateBackupDirectory)?;
        let result = self.backup_to_created_directory(destination);
        if result.is_err() {
            let _ = fs::remove_dir_all(destination);
        }
        result
    }

    fn backup_to_created_directory(&self, destination: &Path) -> Result<(), StoreError> {
        let catalogue = destination.join("hub.sqlite");
        self.backup_catalogue_to(&catalogue)?;
        let copied_catalogue = Connection::open_with_flags(
            &catalogue,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::Open)?;
        let mut statement = copied_catalogue
            .prepare("SELECT DISTINCT sha256, compressed_bytes FROM sync_packs ORDER BY sha256")
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(StoreError::Query)?;
        let packs = destination.join("packs").join("sha256");
        fs::create_dir_all(&packs).map_err(StoreError::CreateBackupDirectory)?;
        for row in rows {
            let (sha256, expected_bytes) = row.map_err(StoreError::Query)?;
            let expected_bytes =
                u64::try_from(expected_bytes).map_err(|_| StoreError::PackSizeTooLarge)?;
            if !is_sha256_hex(&sha256) {
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
        Ok(())
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

    /// Return success only when the catalogue is intact and no lifecycle state
    /// awaits deterministic reconstruction. A quarantine is intentionally not
    /// hidden by ordinary service readiness.
    pub fn readiness_check(&self) -> Result<(), StoreError> {
        self.quick_check()?;
        let connection = self.open()?;
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

    /// Perform the operator-facing integrity gate. Unlike the fast readiness
    /// path, this hashes every currently referenced immutable pack.
    pub fn catalogue_check(&self) -> Result<(), StoreError> {
        self.readiness_check()?;
        self.verify_referenced_packs()
    }

    fn verify_referenced_packs(&self) -> Result<(), StoreError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT sha256, relative_path, compressed_bytes \
                 FROM sync_packs ORDER BY sha256",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(StoreError::Query)?;

        for row in rows {
            let (sha256, relative_path, compressed_bytes) = row.map_err(StoreError::Query)?;
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
        let snapshot_id = manifest.snapshot_id.to_string();
        let vehicle_id = manifest.vehicle_id.to_string();
        let head_sequence =
            i64::try_from(manifest.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current = transaction
            .query_row(
                "SELECT snapshot_id, head_sequence FROM sync_manifests \
                 WHERE vehicle_id = ?1 ORDER BY head_sequence DESC LIMIT 1",
                params![vehicle_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if let Some((current_snapshot_id, current_sequence)) = current {
            let current_sequence =
                u64::try_from(current_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
            if current_sequence > manifest.head_sequence
                || (current_sequence == manifest.head_sequence
                    && current_snapshot_id != snapshot_id)
            {
                return Err(StoreError::StaleManifest {
                    vehicle_id: manifest.vehicle_id,
                    attempted: manifest.head_sequence,
                    current: current_sequence,
                });
            }
        }
        transaction
            .execute(
                "INSERT INTO sync_manifests (snapshot_id, vehicle_id, head_sequence, manifest_json) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(snapshot_id) DO UPDATE SET \
                 vehicle_id = excluded.vehicle_id, \
                 head_sequence = excluded.head_sequence, \
                 manifest_json = excluded.manifest_json",
                params![
                    snapshot_id.as_str(),
                    vehicle_id.as_str(),
                    head_sequence,
                    payload,
                ],
            )
            .map_err(StoreError::PublishManifest)?;
        transaction
            .execute(
                "DELETE FROM sync_packs WHERE snapshot_id = ?1",
                params![snapshot_id.as_str()],
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
                        snapshot_id.as_str(),
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
        if manifest.schema == crate::hub_pack::HUB_PROJECTION_SCHEMA_V2
            && !manifest.chunks.is_empty()
        {
            let base_digest = manifest.chunks[0].sha256.to_string();
            let packs_json = serde_json::to_vec(&manifest.chunks)
                .map_err(StoreError::SerializeManifest)?;
            transaction
                .execute(
                    "INSERT INTO sync_bases(
                        vehicle_id, snapshot_id, base_sequence, base_digest, packs_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(vehicle_id) DO NOTHING",
                    params![
                        vehicle_id.as_str(),
                        snapshot_id.as_str(),
                        head_sequence,
                        base_digest,
                        packs_json,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
            transaction
                .execute(
                    "INSERT INTO sync_heads(
                        vehicle_id, base_snapshot_id, head_sequence, head_digest,
                        terminal_cursor
                     ) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(vehicle_id) DO NOTHING",
                    params![
                        vehicle_id.as_str(),
                        snapshot_id.as_str(),
                        head_sequence,
                        base_digest,
                        serde_json::to_string(&manifest.terminal_cursor)
                            .map_err(StoreError::SerializeManifest)?,
                    ],
                )
                .map_err(StoreError::LineageCatalog)?;
            transaction
                .execute(
                    "UPDATE sync_mutations SET published = 1, claimed_until_ms = 0
                     WHERE vehicle_id = ?1 AND published = 0",
                    params![vehicle_id.as_str()],
                )
                .map_err(StoreError::LineageCatalog)?;
        }
        transaction.commit().map_err(StoreError::PublishManifest)
    }

    pub fn claim_export_outbox(&self, now_ms: i64) -> Result<Option<ExportOutboxClaim>, StoreError> {
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
        let vehicle_id = Uuid::parse_str(&vehicle).map_err(|_| StoreError::InvalidStoredUuid("export vehicle"))?;
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
        }))
    }

    pub fn complete_export_outbox(&self, claim: &ExportOutboxClaim) -> Result<(), StoreError> {
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE export_outbox SET claimed_until_ms = 0, attempts = 0,
                    next_attempt_ms = 0, last_error = NULL
                 WHERE vehicle_id = ?1 AND dirty_revision <= ?2",
                params![claim.vehicle_id.to_string(), claim.dirty_revision],
            )
            .map_err(StoreError::Query)?;
        Ok(())
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
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE export_outbox
                 SET claimed_until_ms = 0, next_attempt_ms = ?1, last_error = ?2
                 WHERE vehicle_id = ?3 AND dirty_revision = ?4",
                params![now_ms.saturating_add(delay), safe_error, claim.vehicle_id.to_string(), claim.dirty_revision],
            )
            .map_err(StoreError::Query)?;
        Ok(())
    }

    pub fn release_export_outbox(&self, claim: &ExportOutboxClaim) -> Result<(), StoreError> {
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE export_outbox SET claimed_until_ms = 0
                 WHERE vehicle_id = ?1 AND dirty_revision = ?2",
                params![claim.vehicle_id.to_string(), claim.dirty_revision],
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
            .query(params![vehicle_key.as_str(), first_revision, maximum as i64])
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

    pub fn v2_projection_binding(&self, vehicle_id: Uuid) -> Result<ProjectionBinding, StoreError> {
        let installation_id = self.installation_id()?;
        let connection = self.open()?;
        let (source_id, generation, source_key, selected_car_id): (String, i64, String, Option<i64>) =
            connection
                .query_row(
                    "SELECT v.source_id, s.generation, v.source_vehicle_key,
                            (SELECT car_id FROM car_settings WHERE vehicle_id = v.vehicle_id)
                     FROM vehicles v JOIN sources s ON s.source_id = v.source_id
                     WHERE v.vehicle_id = ?1",
                    params![vehicle_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(StoreError::Query)?;
        let account_id = Uuid::parse_str(&source_id).map_err(|_| StoreError::InvalidSourceId)?;
        let generation = u64::try_from(generation).map_err(|_| StoreError::InvalidStoredSequence)?;
        let selected_car_id = selected_car_id
            .or_else(|| source_key.strip_prefix("eid:").and_then(|value| value.parse().ok()))
            .or_else(|| source_key.strip_prefix("vid:").and_then(|value| value.parse().ok()))
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
        ordered.sort_by_key(|mutation| (mutation.revision, mutation.entity.clone(), mutation.entity_id));
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
            let entity = parse_sync_entity(&mutation.entity)
                .ok_or_else(|| StoreError::SyncMutation(format!("unknown entity {}", mutation.entity)))?;
            if mutation.operation == "tombstone" {
                delta.tombstones.push(ProjectionTombstone {
                    entity,
                    id: mutation.entity_id,
                    car_id: mutation.car_id,
                });
                continue;
            }
            if mutation.operation != "upsert" {
                return Err(StoreError::SyncMutation("invalid mutation operation".into()));
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
        terminal_cursor: &str,
    ) -> Result<(), StoreError> {
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
        if head_sequence == i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?
            && head_digest == delta.chain_digest.to_string()
            && existing_delta.as_ref().is_some_and(|(chain, pack)| {
                chain == &delta.chain_digest.to_string() && pack == &delta.pack_digest.to_string()
            })
        {
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
        if head_sequence != i64::try_from(delta.from_sequence).map_err(|_| StoreError::SequenceTooLarge)?
            || head_digest != delta.parent_chain_digest.to_string()
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        let pack_json = serde_json::to_vec(delta).map_err(StoreError::SerializeManifest)?;
        transaction
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
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
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
                    params![delta.pack.snapshot_id.to_string(), i64::from(delta.pack.ordinal)],
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
        transaction
            .execute(
                "UPDATE sync_heads SET head_sequence = ?1, head_digest = ?2,
                        terminal_cursor = ?3
                 WHERE vehicle_id = ?4 AND head_sequence = ?5
                   AND head_digest = ?6",
                params![
                    i64::try_from(delta.to_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
                    delta.chain_digest.to_string(),
                    terminal_cursor,
                    vehicle_key.as_str(),
                    head_sequence,
                    head_digest,
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
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

    pub fn manifest_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<SyncManifest>, StoreError> {
        let connection = self.open()?;
        let payload = connection
            .query_row(
                "SELECT manifest_json FROM sync_manifests \
                 WHERE vehicle_id = ?1
                   AND json_extract(manifest_json, '$.mode') = 'full_snapshot'
                 ORDER BY head_sequence DESC LIMIT 1",
                params![vehicle_id.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        payload.map(decode_manifest).transpose()
    }

    /// Load the exact manifest atomically associated with a source snapshot
    /// fingerprint. Legacy unbound fingerprints deliberately return `None`.
    pub fn manifest_for_snapshot_fingerprint(
        &self,
        vehicle_id: Uuid,
        fingerprint: Sha256Digest,
    ) -> Result<Option<SyncManifest>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let payload = connection
            .query_row(
                "SELECT manifests.manifest_json
                 FROM snapshot_fingerprints AS fingerprints
                 JOIN sync_manifests AS manifests
                   ON manifests.snapshot_id = fingerprints.snapshot_id
                  AND manifests.vehicle_id = fingerprints.vehicle_id
                  AND manifests.head_sequence = fingerprints.head_sequence
                 WHERE fingerprints.vehicle_id = ?1
                   AND fingerprints.fingerprint_sha256 = ?2",
                params![vehicle_id.to_string(), fingerprint.as_bytes().as_slice()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        payload.map(decode_manifest).transpose()
    }

    pub fn snapshot_fingerprint_is_current(
        &self,
        vehicle_id: Uuid,
        fingerprint: Sha256Digest,
    ) -> Result<bool, StoreError> {
        Ok(self
            .manifest_for_snapshot_fingerprint(vehicle_id, fingerprint)?
            .is_some())
    }

    /// Whether any historical manifest catalogue entry references this pack.
    /// Import cleanup uses this rather than only the current manifest because
    /// older snapshots remain valid recovery and sync inputs.
    pub(crate) fn pack_sha256_is_catalogued(&self, sha256: &str) -> Result<bool, StoreError> {
        self.open()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_packs WHERE sha256 = ?1)",
                params![sha256],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    pub fn record_snapshot_fingerprint(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
    ) -> Result<(), StoreError> {
        if manifest.vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
        transaction.commit().map_err(StoreError::PublishManifest)
    }

    /// Reserve a full-snapshot marker while the caller owns the publication
    /// gate. Other modules cannot reserve a marker without this token.
    pub(crate) fn reserve_next_full_snapshot_sequence(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
    ) -> Result<u64, StoreError> {
        self.next_full_snapshot_sequence_while_gated(vehicle_id)
    }

    /// Durably reserve the next full-snapshot marker for one Hub vehicle.
    ///
    /// Reservation happens before pack construction, so a failed unpublished
    /// build can leave a harmless gap. It cannot reuse a marker already handed
    /// to another process, keeping successful publications totally ordered.
    fn next_full_snapshot_sequence_while_gated(&self, vehicle_id: Uuid) -> Result<u64, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        // The counter records reservations, while the catalogue can also be
        // advanced by a live publisher. The caller holds the publication gate.
        let next_counter: Option<i64> = transaction
            .query_row(
                "SELECT next_sequence FROM vehicle_snapshot_sequences WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let catalog_head: Option<i64> = transaction
            .query_row(
                "SELECT MAX(head_sequence) FROM sync_manifests WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let reserved = catalog_head
            .unwrap_or(0)
            .max(next_counter.unwrap_or(1).saturating_sub(1))
            .checked_add(1)
            .ok_or(StoreError::SequenceExhausted)?;
        let next_sequence = reserved.checked_add(1).ok_or(StoreError::SequenceExhausted)?;
        transaction
            .execute(
                "INSERT INTO vehicle_snapshot_sequences (vehicle_id, next_sequence)
                 VALUES (?1, ?2)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    next_sequence = MAX(vehicle_snapshot_sequences.next_sequence, excluded.next_sequence)",
                params![vehicle_id.to_string(), next_sequence],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::Query)?;
        u64::try_from(reserved)
            .ok()
            .filter(|sequence| *sequence >= 1)
            .ok_or(StoreError::SequenceExhausted)
    }

    /// Make the pack catalogue, imported lifecycle recovery state, geofences,
    /// fingerprint, and staging cleanup visible in one SQLite transaction.
    /// Callers retain immutable pack chunks before this transaction starts.
    pub fn finalize_import_generation(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<(), StoreError> {
        if run_id.is_nil()
            || source_id.is_nil()
            || vehicle_id.is_nil()
            || car_id <= 0
            || manifest.vehicle_id != vehicle_id
        {
            return Err(StoreError::InvalidImportGeneration);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let (encoded, base_last_observation_id, base_updated_at_ms): (String, i64, i64) = transaction
            .query_row(
                "SELECT sessions.session_json, generations.base_last_observation_id,
                        generations.base_updated_at_ms
                 FROM import_generation_sessions AS sessions
                 JOIN import_generations AS generations USING(run_id)
                 WHERE generations.run_id = ?1 AND generations.source_id = ?2
                   AND generations.vehicle_id = ?3 AND generations.car_id = ?4
                   AND generations.status = 'staging'",
                params![run_id.to_string(), source_id.to_string(), vehicle_id.to_string(), car_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::ImportGeneration)?
            .ok_or(StoreError::ImportGenerationNotFound)?;
        let session = serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
        publish_manifest_in_transaction(&transaction, manifest)?;
        promote_imported_open_session_in_transaction(
            &transaction,
            source_id,
            vehicle_id,
            car_id,
            &session,
            updated_at_ms,
            Some((base_last_observation_id, base_updated_at_ms)),
        )?;
        upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
        record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
        if transaction
            .execute(
                "DELETE FROM import_generations WHERE run_id = ?1 AND status = 'staging'",
                params![run_id.to_string()],
            )
            .map_err(StoreError::ImportGeneration)?
            != 1
        {
            return Err(StoreError::ImportGenerationNotFound);
        }
        transaction.commit().map_err(StoreError::ImportGeneration)
    }

    /// Atomically catalogue a sealed import history snapshot and its source
    /// fingerprint. Callers retain immutable pack chunks before this call.
    pub fn finalize_import_snapshot(
        &self,
        manifest: &SyncManifest,
        fingerprint: Sha256Digest,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        publish_manifest_in_transaction(&transaction, manifest)?;
        upsert_geofences_in_transaction(&transaction, manifest.vehicle_id, geofences)?;
        record_snapshot_fingerprint_in_transaction(&transaction, manifest, fingerprint)?;
        transaction.commit().map_err(StoreError::PublishManifest)
    }

    /// Start an inactive import generation. Nothing in this generation is
    /// visible to lifecycle reads or published manifests.
    pub fn begin_import_generation(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        created_at_ms: i64,
    ) -> Result<Uuid, StoreError> {
        if source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        require_positive_db(car_id, "import car_id")?;
        validate_timestamp("import generation created_at_ms", created_at_ms)?;
        let run_id = Uuid::new_v4();
        let connection = self.open()?;
        let (base_last_observation_id, base_updated_at_ms): (i64, i64) = connection
            .query_row(
                "SELECT last_observation_id, updated_at_ms
                 FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::ImportGeneration)?
            .unwrap_or((0, 0));
        connection
            .execute(
                "INSERT INTO import_generations(
                    run_id, source_id, vehicle_id, car_id, status, created_at_ms,
                    base_last_observation_id, base_updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, 'staging', ?5, ?6, ?7)",
                params![
                    run_id.to_string(),
                    source_id.to_string(),
                    vehicle_id.to_string(),
                    car_id,
                    created_at_ms,
                    base_last_observation_id,
                    base_updated_at_ms
                ],
            )
            .map_err(StoreError::ImportGeneration)?;
        Ok(run_id)
    }

    /// Replace the inactive generation's open-session image. This is safe to
    /// call after each bounded source read; active lifecycle state is untouched.
    pub fn stage_import_generation_session(
        &self,
        run_id: Uuid,
        session: &TeslaMateOpenSession,
    ) -> Result<(), StoreError> {
        if run_id.is_nil() {
            return Err(StoreError::InvalidImportGeneration);
        }
        session
            .validate()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        let encoded = serde_json::to_string(session).map_err(StoreError::SerializeLifecycleRow)?;
        let connection = self.open()?;
        let updated = connection
            .execute(
                "INSERT INTO import_generation_sessions(run_id, session_json)
                 SELECT ?1, ?2 WHERE EXISTS(
                    SELECT 1 FROM import_generations
                    WHERE run_id = ?1 AND status = 'staging'
                 )
                 ON CONFLICT(run_id) DO UPDATE SET session_json = excluded.session_json",
                params![run_id.to_string(), encoded],
            )
            .map_err(StoreError::ImportGeneration)?;
        if updated == 0 {
            return Err(StoreError::ImportGenerationNotFound);
        }
        Ok(())
    }

    /// Promote the already validated inactive session into the existing
    /// lifecycle tables. The caller invokes this only after pack publication.
    pub fn promote_import_generation(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
    ) -> Result<OpenSessionSeedReport, StoreError> {
        let connection = self.open()?;
        let encoded: String = connection
            .query_row(
                "SELECT sessions.session_json
                 FROM import_generation_sessions AS sessions
                 JOIN import_generations AS generations USING(run_id)
                 WHERE run_id = ?1 AND source_id = ?2 AND vehicle_id = ?3
                   AND car_id = ?4 AND status = 'staging'",
                params![run_id.to_string(), source_id.to_string(), vehicle_id.to_string(), car_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::ImportGeneration)?
            .ok_or(StoreError::ImportGenerationNotFound)?;
        let session: TeslaMateOpenSession =
            serde_json::from_str(&encoded).map_err(|_| StoreError::InvalidLifecycleSession)?;
        let (base_last_observation_id, base_updated_at_ms): (i64, i64) = connection
            .query_row(
                "SELECT base_last_observation_id, base_updated_at_ms
                 FROM import_generations WHERE run_id = ?1 AND status = 'staging'",
                params![run_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(StoreError::ImportGeneration)?;
        let report = self.seed_imported_open_session_if_unchanged(
            source_id,
            vehicle_id,
            car_id,
            &session,
            updated_at_ms,
            base_last_observation_id,
            base_updated_at_ms,
        )?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "DELETE FROM import_generations WHERE run_id = ?1 AND status = 'staging'",
                params![run_id.to_string()],
            )
            .map_err(StoreError::ImportGeneration)?;
        transaction.commit().map_err(StoreError::ImportGeneration)?;
        Ok(report)
    }

    fn seed_imported_open_session_if_unchanged(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        session: &TeslaMateOpenSession,
        updated_at_ms: i64,
        expected_last_observation_id: i64,
        expected_updated_at_ms: i64,
    ) -> Result<OpenSessionSeedReport, StoreError> {
        let previous = self.load_lifecycle_state(vehicle_id)?;
        if previous.as_ref().is_some_and(|record| {
            record.last_observation_id != expected_last_observation_id
                || record.updated_at_ms != expected_updated_at_ms
        }) || (previous.is_none() && (expected_last_observation_id != 0 || expected_updated_at_ms != 0)) {
            return Err(StoreError::ImportGenerationConflict);
        }
        self.seed_imported_open_session_checked(
            source_id,
            vehicle_id,
            car_id,
            session,
            updated_at_ms,
            Some((expected_last_observation_id, expected_updated_at_ms)),
        )
    }

    pub fn abort_import_generation(&self, run_id: Uuid) -> Result<(), StoreError> {
        if run_id.is_nil() {
            return Ok(());
        }
        let connection = self.open()?;
        connection
            .execute(
                "DELETE FROM import_generations WHERE run_id = ?1",
                params![run_id.to_string()],
            )
            .map_err(StoreError::ImportGeneration)?;
        Ok(())
    }

    /// Commit a validated lineage only after every referenced immutable pack
    /// is present, size-correct, and hash-correct. The DB transaction never
    /// becomes visible before that verification completes.
    pub fn commit_lineage_catalog(
        &self,
        lineage: &LineageManifestV2,
    ) -> Result<(), StoreError> {
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
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
                    },
                )
                .optional()
                .map_err(StoreError::LineageCatalog)?;
            if let Some((snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes)) =
                existing_pack
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
            let sequence = u64::try_from(sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
            if sequence > lineage.head_sequence
                || (sequence == lineage.head_sequence
                    && digest != lineage.head_digest.to_string())
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
                            i64::try_from(sequence)
                                .map_err(|_| StoreError::SequenceTooLarge)?,
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

    fn verify_lineage_pack(&self, pack: &TransportPack) -> Result<(), StoreError> {
        let path = self
            .packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        let metadata = fs::symlink_metadata(&path).map_err(|_| StoreError::LineagePackNotReady)?;
        if !metadata.file_type().is_file() || metadata.len() != pack.compressed_bytes {
            return Err(StoreError::LineagePackNotReady);
        }
        if sha256_file_hex(&path)? != pack.sha256.to_string() {
            return Err(StoreError::LineagePackDigestMismatch);
        }
        Ok(())
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

    pub fn lineage_manifest_for_vehicle(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<LineageManifestV2>, StoreError> {
        let connection = self.open()?;
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
        let base_sequence = u64::try_from(base_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        let base_snapshot_id = Uuid::parse_str(&snapshot_id)
            .map_err(|_| StoreError::InvalidStoredUuid("lineage base snapshot"))?;
        let base_digest = base_digest
            .parse::<Sha256Digest>()
            .map_err(|_| StoreError::LineageCatalogConflict)?;
        let base_packs: Vec<TransportPack> = serde_json::from_slice(&packs_json)
            .map_err(StoreError::DeserializeManifest)?;
        if base_packs.is_empty() {
            return Err(StoreError::LineageCatalogConflict);
        }
        for pack in &base_packs {
            self.verify_lineage_pack(pack)?;
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
            let delta: LineageDelta = serde_json::from_slice(&json)
                .map_err(StoreError::DeserializeManifest)?;
            if delta.from_sequence != u64::try_from(from_sequence).unwrap_or(u64::MAX)
                || delta.to_sequence != u64::try_from(to_sequence).unwrap_or(u64::MAX)
                || delta.parent_chain_digest.to_string() != parent_chain_digest
                || delta.chain_digest.to_string() != chain_digest
                || delta.pack_digest.to_string() != pack_digest
            {
                return Err(StoreError::LineageCatalogConflict);
            }
            self.verify_lineage_pack(&delta.pack)?;
            deltas.push(delta);
        }
        drop(statement);

        let (head_base_snapshot, head_sequence, head_digest, terminal_cursor):
            (String, i64, String, String) = connection
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
        let terminal_cursor: OpaqueCursor = serde_json::from_str(&terminal_cursor)
            .map_err(StoreError::DeserializeManifest)?;
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

        let source_vehicle = find_vehicle(&transaction, descriptor.source_id, &descriptor.source_vehicle_key)?;
        let had_source_vehicle = source_vehicle.is_some();
        let identity_vehicle = find_identity_vehicle(&transaction, descriptor)?;
        let identity_record = match identity_vehicle {
            Some(vehicle_id) => find_vehicle_by_id(&transaction, vehicle_id)?,
            None => None,
        };
        if source_vehicle.is_some() && identity_vehicle.is_some()
            && source_vehicle.as_ref().map(|v| v.vehicle_id) != identity_vehicle
        {
            return Err(StoreError::VehicleIdentityConflict);
        }
        if let Some(mut vehicle) = source_vehicle.or(identity_record) {
            if let Some(vin) = &descriptor.vin {
                if let Some(existing) = vehicle.vin.as_ref()
                    && existing != vin
                {
                    return Err(StoreError::VehicleIdentityConflict);
                }
            }
            if had_source_vehicle && let Some(expected) = expected_vehicle_id
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
                        lookup_latitude: row.get(4)?,
                        lookup_longitude: row.get(5)?,
                        looked_up_at_ms: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Query)
    }

    pub fn source_vehicle_key(&self, vehicle_id: Uuid) -> Result<Option<String>, StoreError> {
        let connection = self.open()?;
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
        let connection = self.open_read_only_connection()?;
        let latest = latest_observation_metadata(&connection, target.vehicle_id, None)?;
        Ok(ObservationWatermark {
            source_car_id,
            source_id: target.source_id,
            vehicle_id: target.vehicle_id,
            observation_id: latest
                .as_ref()
                .map_or(0, |observation| observation.observation_id),
            observed_at_ms: latest.as_ref().map(|observation| observation.observed_at_ms),
            received_at_ms: latest.as_ref().map(|observation| observation.received_at_ms),
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
            latest_observation_id: latest.as_ref().map(|observation| observation.observation_id),
            latest_observed_at_ms: latest.as_ref().map(|observation| observation.observed_at_ms),
            latest_received_at_ms: latest.as_ref().map(|observation| observation.received_at_ms),
        })
    }

    /// Capture the highest durable outbound-request receipt id. A caller must
    /// capture this before starting a proof window, then pass it to
    /// `verify_no_wake_after` after the collection attempt has finished.
    pub fn outbound_request_watermark(&self) -> Result<OutboundRequestWatermark, StoreError> {
        let connection = self.open_read_only_connection()?;
        let receipt_id = connection
            .query_row("SELECT COALESCE(MAX(id), 0) FROM outbound_request_receipts", [], |row| {
                row.get(0)
            })
            .map_err(StoreError::OutboundRequestReceipt)?;
        Ok(OutboundRequestWatermark { receipt_id })
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
        transaction.commit().map_err(StoreError::OutboundRequestReceipt)?;
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
                 SET completed_at_ms = ?2, duration_ms = ?3, outcome = ?4, http_status = ?5
                 WHERE id = ?1 AND outcome = 'started'",
                params![
                    receipt_id.0,
                    completed_at_ms,
                    duration_ms,
                    completion.outcome.as_str(),
                    completion.http_status,
                ],
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        // Retention cleanup only ever removes terminal rows older than the
        // store-clock 30-day cutoff. It never deletes in-window or unresolved
        // receipts merely to meet the capacity bound.
        prune_expired_outbound_request_receipts(&transaction)?;
        transaction.commit().map_err(StoreError::OutboundRequestReceipt)
    }

    /// Begin one stream supervisor lifetime before its first connection
    /// attempt. A process crash or task abort deliberately leaves this row
    /// unresolved; only an orderly unsubscribe may complete it.
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
        transaction.commit().map_err(StoreError::StreamSessionReceipt)?;
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
        transaction.commit().map_err(StoreError::StreamSessionReceipt)
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
                        precondition, outcome, http_status
                 FROM outbound_request_receipts
                 WHERE id > ?1 AND correlation_id = ?2
                 ORDER BY id ASC LIMIT ?3",
            )
            .map_err(StoreError::OutboundRequestReceipt)?;
        let rows = statement
            .query_map(params![after_receipt_id, correlation_id.to_string(), i64::from(limit)], receipt_from_row)
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
                 WHERE correlation_id = ?2",
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
            Some((source_car_id, watermark)) => Some(self.verify_observation_after(source_car_id, watermark)?),
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
            [target] => Ok(target.clone()),
            _ => Err(ObservationVerificationError::AmbiguousVehicleMapping),
        }
    }

    pub fn put_address_cache(&self, record: &AddressCacheRecord) -> Result<(), StoreError> {
        validate_address_cache_record(record)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        transaction
            .execute(
                "INSERT INTO address_cache(osm_type, osm_id, display_name, name)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(osm_type, osm_id) DO UPDATE SET
                    display_name = excluded.display_name,
                    name = excluded.name",
                params![
                    record.osm_type,
                    record.osm_id,
                    record.display_name,
                    record.name,
                ],
            )
            .map_err(StoreError::AddressCacheWrite)?;
        transaction
            .execute(
                "INSERT INTO address_lookup_cache(
                    lookup_key, latitude, longitude, osm_type, osm_id, looked_up_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(lookup_key) DO UPDATE SET
                    latitude = excluded.latitude,
                    longitude = excluded.longitude,
                    osm_type = excluded.osm_type,
                    osm_id = excluded.osm_id,
                    looked_up_at_ms = excluded.looked_up_at_ms",
                params![
                    address_lookup_key(crate::location::Wgs84Point {
                        latitude: record.lookup_latitude,
                        longitude: record.lookup_longitude,
                    }),
                    record.lookup_latitude,
                    record.lookup_longitude,
                    record.osm_type,
                    record.osm_id,
                    record.looked_up_at_ms,
                ],
            )
            .map_err(StoreError::AddressCacheWrite)?;
        transaction.commit().map_err(StoreError::AddressCacheWrite)
    }

    pub fn claim_address_enrichment_job(
        &self,
        now_ms: i64,
    ) -> Result<Option<AddressEnrichmentJob>, StoreError> {
        validate_timestamp("address job now_ms", now_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let job = {
            let mut statement = transaction
                .prepare(
                    "SELECT job_key, vehicle_id, target_type, target_id, field,
                            latitude, longitude, attempts
                     FROM address_enrichment_jobs
                     WHERE (status IN ('pending', 'retry') AND next_attempt_ms <= ?1)
                        OR (status = 'running' AND lease_until_ms <= ?1)
                     ORDER BY next_attempt_ms ASC, job_key ASC LIMIT 1",
                )
                .map_err(StoreError::Query)?;
            statement
                .query_row(params![now_ms], |row| {
                    let vehicle_id: String = row.get(1)?;
                    Ok(AddressEnrichmentJob {
                        job_key: row.get(0)?,
                        vehicle_id: Uuid::parse_str(&vehicle_id).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                1,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        target_type: row.get(2)?,
                        target_id: row.get(3)?,
                        field: row.get(4)?,
                        latitude: row.get(5)?,
                        longitude: row.get(6)?,
                        attempts: row
                            .get::<_, i64>(7)?
                            .try_into()
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, i64::MAX))?,
                    })
                })
                .optional()
                .map_err(StoreError::Query)?
        };
        if let Some(job) = &job {
            transaction
                .execute(
                    "UPDATE address_enrichment_jobs
                     SET status = 'running', attempts = attempts + 1,
                         lease_until_ms = ?1
                     WHERE job_key = ?2",
                    params![now_ms.saturating_add(5 * 60 * 1000), job.job_key],
                )
                .map_err(StoreError::AddressEnrichmentWrite)?;
        }
        transaction
            .commit()
            .map_err(StoreError::AddressEnrichmentWrite)?;
        Ok(job.map(|mut job| {
            job.attempts = job.attempts.saturating_add(1);
            job
        }))
    }

    pub fn complete_address_enrichment(
        &self,
        job: &AddressEnrichmentJob,
        address: Option<&str>,
        now_ms: i64,
    ) -> Result<AddressEnrichmentCompletion, StoreError> {
        validate_timestamp("address completion now_ms", now_ms)?;
        if let Some(address) = address {
            if address.trim().is_empty()
                || address.len() > MAX_DISPLAY_NAME_BYTES
                || address.chars().any(char::is_control)
            {
                return Err(StoreError::InvalidAddressEnrichment);
            }
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let mut changed = false;
        if let Some(address) = address {
            let (table, json_column, id_column) = match job.target_type.as_str() {
                "drive" => ("materialised_drives", "drive_json", "drive_id"),
                "charge" => ("materialised_charges", "charge_json", "charge_id"),
                _ => return Err(StoreError::InvalidAddressEnrichment),
            };
            let select = format!(
                "SELECT {json_column}, car_id FROM {table} WHERE vehicle_id = ?1 AND {id_column} = ?2"
            );
            let current: Option<(String, i64)> = transaction
                .query_row(
                    &select,
                    params![job.vehicle_id.to_string(), job.target_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(StoreError::Query)?;
            if let Some((current, car_id)) = current {
                let mut value: Value =
                    serde_json::from_str(&current).map_err(StoreError::DeserializeLifecycleRow)?;
                let object = value
                    .as_object_mut()
                    .ok_or(StoreError::InvalidAddressEnrichment)?;
                if object.get(&job.field).and_then(Value::as_str).is_none() {
                    object.insert(job.field.clone(), Value::String(address.trim().to_owned()));
                    let updated =
                        serde_json::to_string(&value).map_err(StoreError::SerializeLifecycleRow)?;
                    let update = format!(
                        "UPDATE {table} SET {json_column} = ?1 WHERE vehicle_id = ?2 AND {id_column} = ?3"
                    );
                    transaction
                        .execute(
                            &update,
                            params![updated, job.vehicle_id.to_string(), job.target_id],
                        )
                        .map_err(StoreError::AddressEnrichmentWrite)?;
                    let entity = if job.target_type == "drive" {
                        "drive"
                    } else {
                        "charge"
                    };
                    record_sync_mutation_in_transaction(
                        &transaction,
                        job.vehicle_id,
                        entity,
                        job.target_id,
                        car_id,
                        "upsert",
                        &updated,
                    )?;
                    changed = true;
                }
            }
        }
        transaction
            .execute(
                "UPDATE address_enrichment_jobs
                 SET status = 'complete', completed_at_ms = ?1, lease_until_ms = 0,
                     last_error = NULL
                 WHERE job_key = ?2",
                params![now_ms, job.job_key],
            )
            .map_err(StoreError::AddressEnrichmentWrite)?;
        if changed {
            mark_export_dirty_in_transaction(&transaction, job.vehicle_id)?;
        }
        transaction
            .commit()
            .map_err(StoreError::AddressEnrichmentWrite)?;
        Ok(AddressEnrichmentCompletion {
            vehicle_id: job.vehicle_id,
            changed,
        })
    }

    pub fn retry_address_enrichment(
        &self,
        job: &AddressEnrichmentJob,
        error: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("address retry now_ms", now_ms)?;
        let delay_seconds = 5_u64
            .saturating_mul(1_u64 << job.attempts.min(14))
            .min(24 * 60 * 60);
        let delay_ms = i64::try_from(delay_seconds.saturating_mul(1000)).unwrap_or(i64::MAX);
        let bounded_error = error
            .chars()
            .filter(|character| !character.is_control())
            .take(256)
            .collect::<String>();
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE address_enrichment_jobs
                 SET status = 'retry', next_attempt_ms = ?1, lease_until_ms = 0,
                     last_error = ?2
                 WHERE job_key = ?3",
                params![now_ms.saturating_add(delay_ms), bounded_error, job.job_key],
            )
            .map_err(StoreError::AddressEnrichmentWrite)?;
        Ok(())
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
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let result = append_observation_in_transaction(&transaction, input, received_at_ms);
        if result.is_ok() {
            transaction
                .commit()
                .map_err(StoreError::AppendObservation)?;
        }
        result
    }

    pub(crate) fn accept_stream_observation_and_lifecycle(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
        car_id: i64,
    ) -> Result<StreamObservationResult, StoreError> {
        input.validate()?;
        validate_timestamp("observation received_at_ms", received_at_ms)?;
        if car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if !stream_timestamp_is_newer(&transaction, input.vehicle_id, input.observed_at_ms)? {
            transaction.commit().map_err(StoreError::LifecycleWrite)?;
            return Ok(StreamObservationResult::IgnoredDuplicate);
        }
        self.maybe_stream_fault(StreamFaultPoint::RawInsert)?;
        let appended = append_observation_in_transaction(&transaction, input, received_at_ms)?;

        self.maybe_stream_fault(StreamFaultPoint::LifecycleWrite)?;
        let existing = load_lifecycle_state_in_transaction(&transaction, input.vehicle_id)?;
        let mut state = match existing.as_ref() {
            Some(record) => crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                .unwrap_or_else(|_| {
                    let mut clean = crate::lifecycle::OpenSessionState::new();
                    clean.last_observation_id = record.last_observation_id;
                    clean
                }),
            None => crate::lifecycle::OpenSessionState::new(),
        };
        restore_lifecycle_open_children_in_transaction(&transaction, input.vehicle_id, &mut state)?;
        let observations = observations_after_id_in_transaction(
            &transaction,
            input.vehicle_id,
            state.last_observation_id,
            MAX_OBSERVATION_QUERY_LIMIT,
        )?;
        let mut delta = crate::lifecycle::LifecycleDelta::default();
        let mut quarantined = existing.as_ref().is_some_and(|record| record.quarantined);
        for observation in observations {
            let sample = crate::lifecycle::LifecycleSample {
                observation_id: observation.observation_id,
                observed_at_ms: observation.observed_at_ms,
                vehicle_state: observation_vehicle_state(&observation.payload),
                payload: observation.payload,
            };
            let step = crate::lifecycle::apply_sample(state, car_id, &sample)
                .map_err(StoreError::LifecycleProjection)?;
            state = step.state;
            quarantined |= step.quarantined;
            delta.drives.extend(step.delta.drives);
            delta.positions.extend(step.delta.positions);
            delta.charges.extend(step.delta.charges);
            delta.charge_samples.extend(step.delta.charge_samples);
            delta.states.extend(step.delta.states);
            delta.updates.extend(step.delta.updates);
            delta.open_drive_positions.extend(step.delta.open_drive_positions);
            delta.open_charge_samples.extend(step.delta.open_charge_samples);
        }
        if let Some(open) = state.open_drive.as_mut() {
            open.positions.clear();
        }
        if let Some(open) = state.open_charge.as_mut() {
            open.samples.clear();
        }
        let encoded = state
            .encode()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        Self::commit_lifecycle_delta_in_transaction(
            &transaction,
            &LifecycleCommit {
                vehicle_id: input.vehicle_id,
                car_id,
                open_session_json: &encoded,
                last_observation_id: state.last_observation_id,
                quarantined,
                updated_at_ms: received_at_ms,
                delta: &delta,
            },
        )?;
        self.maybe_stream_fault(StreamFaultPoint::WatermarkUpdate)?;
        accept_stream_timestamp_in_transaction(
            &transaction,
            input.vehicle_id,
            input.observed_at_ms,
        )?;
        self.maybe_stream_fault(StreamFaultPoint::Commit)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(StreamObservationResult::Committed {
            observation_id: appended.observation.observation_id,
        })
    }

    pub(crate) fn accept_owner_observation_and_lifecycle(
        &self,
        input: &ObservationInput,
        received_at_ms: i64,
        car_id: i64,
    ) -> Result<OwnerObservationResult, StoreError> {
        input.validate()?;
        validate_timestamp("observation received_at_ms", received_at_ms)?;
        if car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        self.maybe_stream_fault(StreamFaultPoint::RawInsert)?;
        let appended = append_observation_in_transaction(&transaction, input, received_at_ms)?;
        self.maybe_stream_fault(StreamFaultPoint::LifecycleWrite)?;
        let existing = load_lifecycle_state_in_transaction(&transaction, input.vehicle_id)?;
        let mut state = match existing.as_ref() {
            Some(record) => crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                .unwrap_or_else(|_| {
                    let mut clean = crate::lifecycle::OpenSessionState::new();
                    clean.last_observation_id = record.last_observation_id;
                    clean
                }),
            None => crate::lifecycle::OpenSessionState::new(),
        };
        restore_lifecycle_open_children_in_transaction(&transaction, input.vehicle_id, &mut state)?;
        let observations = observations_after_id_in_transaction(
            &transaction,
            input.vehicle_id,
            state.last_observation_id,
            MAX_OBSERVATION_QUERY_LIMIT,
        )?;
        let mut delta = crate::lifecycle::LifecycleDelta::default();
        let mut quarantined = existing.as_ref().is_some_and(|record| record.quarantined);
        for observation in observations {
            let sample = crate::lifecycle::LifecycleSample {
                observation_id: observation.observation_id,
                observed_at_ms: observation.observed_at_ms,
                vehicle_state: observation_vehicle_state(&observation.payload),
                payload: observation.payload,
            };
            let step = crate::lifecycle::apply_sample(state, car_id, &sample)
                .map_err(StoreError::LifecycleProjection)?;
            state = step.state;
            quarantined |= step.quarantined;
            delta.drives.extend(step.delta.drives);
            delta.positions.extend(step.delta.positions);
            delta.charges.extend(step.delta.charges);
            delta.charge_samples.extend(step.delta.charge_samples);
            delta.states.extend(step.delta.states);
            delta.updates.extend(step.delta.updates);
            delta.open_drive_positions.extend(step.delta.open_drive_positions);
            delta.open_charge_samples.extend(step.delta.open_charge_samples);
        }
        if let Some(open) = state.open_drive.as_mut() {
            open.positions.clear();
        }
        if let Some(open) = state.open_charge.as_mut() {
            open.samples.clear();
        }
        let encoded = state.encode().map_err(|_| StoreError::InvalidLifecycleSession)?;
        Self::commit_lifecycle_delta_in_transaction(
            &transaction,
            &LifecycleCommit {
                vehicle_id: input.vehicle_id,
                car_id,
                open_session_json: &encoded,
                last_observation_id: state.last_observation_id,
                quarantined,
                updated_at_ms: received_at_ms,
                delta: &delta,
            },
        )?;
        self.maybe_stream_fault(StreamFaultPoint::Commit)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(OwnerObservationResult {
            append: appended,
            drives_closed: delta.drives.len(),
            charges_closed: delta.charges.len(),
            positions_materialised: delta.positions.len(),
            charge_samples_materialised: delta.charge_samples.len(),
            lifecycle_quarantined: quarantined,
        })
    }

    /// Advance the durable watermark for stream telemetry. This is deliberately
    /// separate from Owner API observations: each source has its own ordering
    /// contract, and a stream frame must never block an Owner API response.
    pub fn accept_stream_timestamp(
        &self,
        vehicle_id: Uuid,
        timestamp_ms: i64,
    ) -> Result<bool, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        validate_timestamp("stream timestamp", timestamp_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let accepted = accept_stream_timestamp_in_transaction(&transaction, vehicle_id, timestamp_ms)?;
        transaction.commit().map_err(StoreError::Query)?;
        Ok(accepted)
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

    /// Read observations in durable insertion order after a lifecycle cursor.
    /// A lifecycle cursor is an observation ID, not a source timestamp.
    pub fn observations_after_id_for_vehicle(
        &self,
        vehicle_id: Uuid,
        after_observation_id: i64,
        limit: u32,
    ) -> Result<Vec<ObservationRecord>, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if after_observation_id < 0 {
            return Err(StoreError::InvalidLifecycleCursor);
        }
        if !(1..=MAX_OBSERVATION_QUERY_LIMIT).contains(&limit) {
            return Err(StoreError::InvalidObservationQueryLimit {
                actual: limit,
                maximum: MAX_OBSERVATION_QUERY_LIMIT,
            });
        }
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms, \
                        payload_sha256, payload_json \
                 FROM raw_observations \
                 WHERE vehicle_id = ?1 AND observation_id > ?2 \
                 ORDER BY observation_id ASC LIMIT ?3",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(
                params![
                    vehicle_id.to_string(),
                    after_observation_id,
                    i64::from(limit)
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

    /// Rehydrate provisional drive/charge children without placing them in
    /// the bounded lifecycle JSON document.
    pub fn restore_lifecycle_open_children(
        &self,
        vehicle_id: Uuid,
        state: &mut crate::lifecycle::OpenSessionState,
    ) -> Result<(), StoreError> {
        let connection = self.open()?;
        let vehicle = vehicle_id.to_string();
        let mut statement = connection
            .prepare(
                "SELECT domain, parent_source_row_id, row_json
                 FROM lifecycle_open_rows WHERE vehicle_id = ?1
                 ORDER BY source_row_id",
            )
            .map_err(StoreError::Query)?;
        if let Some(open) = state.open_drive.as_mut() {
            open.positions.clear();
        }
        if let Some(open) = state.open_charge.as_mut() {
            open.samples.clear();
        }
        let rows = statement
            .query_map(params![vehicle], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(StoreError::Query)?;
        for row in rows {
            let (domain, parent_id, json) = row.map_err(StoreError::Query)?;
            match domain.as_str() {
                "position" => {
                    let position: crate::hub_pack::ProjectionPosition =
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?;
                    if state
                        .open_drive
                        .as_ref()
                        .is_some_and(|open| Some(open.id) == parent_id)
                    {
                        state
                            .open_drive
                            .as_mut()
                            .expect("open drive")
                            .positions
                            .push(position);
                    }
                }
                "charge_sample" => {
                    let sample: crate::hub_pack::ProjectionChargeSample =
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?;
                    if state
                        .open_charge
                        .as_ref()
                        .is_some_and(|open| Some(open.id) == parent_id)
                    {
                        state
                            .open_charge
                            .as_mut()
                            .expect("open charge")
                            .samples
                            .push(sample);
                    }
                }
                _ => {}
            }
        }
        if let Some(open) = state.open_drive.as_mut() {
            if let Some(first) = open.positions.first() {
                open.start_latitude = Some(first.latitude);
                open.start_longitude = Some(first.longitude);
                open.start_soc = first.battery_level;
                open.start_rated_range_km = first.rated_battery_range_km;
            }
            open.outside_temp_sum = 0.0;
            open.outside_temp_count = 0;
            open.speed_max = None;
            for position in &open.positions {
                if let Some(value) = position.outside_temp {
                    open.outside_temp_sum += value;
                    open.outside_temp_count = open.outside_temp_count.saturating_add(1);
                }
                open.speed_max = match (open.speed_max, position.speed) {
                    (Some(current), Some(next)) => Some(current.max(next)),
                    (None, value) => value,
                    (current, None) => current,
                };
            }
        }
        Ok(())
    }

    /// Atomically retain an imported open-session snapshot outside the bounded
    /// lifecycle blob. Repeating the same source snapshot is a no-op.
    pub fn seed_imported_open_session(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        session: &TeslaMateOpenSession,
        updated_at_ms: i64,
    ) -> Result<OpenSessionSeedReport, StoreError> {
        self.seed_imported_open_session_checked(
            source_id, vehicle_id, car_id, session, updated_at_ms, None,
        )
    }

    fn seed_imported_open_session_checked(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        session: &TeslaMateOpenSession,
        updated_at_ms: i64,
        expected: Option<(i64, i64)>,
    ) -> Result<OpenSessionSeedReport, StoreError> {
        if source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        if car_id <= 0 {
            return Err(StoreError::InvalidLifecycleCarId);
        }
        validate_timestamp("open session updated_at_ms", updated_at_ms)?;
        session
            .validate()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;

        let previous = self.load_lifecycle_state(vehicle_id)?;
        let previous_state = previous
            .as_ref()
            .map(|record| {
                crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                    .map_err(|_| StoreError::InvalidLifecycleSession)
            })
            .transpose()?;
        let previous_open = self.load_imported_open_session(source_id, vehicle_id)?;
        let seeded = crate::lifecycle::seed_imported_open_session_state(
            source_id,
            session,
            previous_state.as_ref(),
        )
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
        let same_seed = previous_state
            .as_ref()
            .and_then(|state| state.imported_open.as_ref())
            .is_some_and(|refs| {
                refs.source_id == source_id.to_string()
                    && refs.drive_source_row_id == session.drive.as_ref().map(|row| row.id)
                    && refs.charge_source_row_id == session.charge.as_ref().map(|row| row.id)
                    && refs.state_source_row_id == session.state.as_ref().map(|row| row.id)
                    && refs.standalone_position_count == session.standalone_positions.len() as u64
            })
            && previous_open.as_ref().is_some_and(|old| old == session);
        if same_seed {
            return Ok(OpenSessionSeedReport {
                no_op: true,
                ..OpenSessionSeedReport::default()
            });
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if let Some((expected_last_observation_id, expected_updated_at_ms)) = expected {
            let actual: Option<(i64, i64)> = transaction
                .query_row(
                    "SELECT last_observation_id, updated_at_ms
                     FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                    params![vehicle_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(StoreError::LifecycleWrite)?;
            if actual != Some((expected_last_observation_id, expected_updated_at_ms)) {
                return Err(StoreError::ImportGenerationConflict);
            }
        }
        ensure_source_exists(&transaction, source_id)?;
        ensure_vehicle_source(&transaction, vehicle_id, source_id)?;

        let source = source_id.to_string();
        let vehicle = vehicle_id.to_string();
        transaction
            .execute(
                "DELETE FROM lifecycle_open_rows
                 WHERE source_id = ?1 AND vehicle_id = ?2",
                params![source, vehicle],
            )
            .map_err(StoreError::LifecycleWrite)?;
        transaction
            .execute(
                "DELETE FROM lifecycle_source_watermarks
                 WHERE source_id = ?1 AND vehicle_id = ?2",
                params![source, vehicle],
            )
            .map_err(StoreError::LifecycleWrite)?;
        let mut inserted = 0;
        if let Some(row) = &session.drive {
            inserted += insert_open_row(
                &transaction,
                &source,
                "drives",
                row.id,
                &vehicle,
                car_id,
                "drive",
                None,
                row,
            )?;
        }
        for row in &session.drive_positions {
            inserted += insert_open_row(
                &transaction,
                &source,
                "positions",
                row.id,
                &vehicle,
                car_id,
                "position",
                row.drive_id,
                row,
            )?;
        }
        if let Some(row) = &session.charge {
            inserted += insert_open_row(
                &transaction,
                &source,
                "charging_processes",
                row.id,
                &vehicle,
                car_id,
                "charge",
                None,
                row,
            )?;
        }
        for row in &session.charge_samples {
            inserted += insert_open_row(
                &transaction,
                &source,
                "charges",
                row.id,
                &vehicle,
                car_id,
                "charge_sample",
                Some(row.charging_process_id),
                row,
            )?;
        }
        if let Some(row) = &session.state {
            inserted += insert_open_row(
                &transaction,
                &source,
                "states",
                row.id,
                &vehicle,
                car_id,
                "state",
                None,
                row,
            )?;
        }
        for row in &session.standalone_positions {
            inserted += insert_open_row(
                &transaction,
                &source,
                "positions",
                row.id,
                &vehicle,
                car_id,
                "standalone_position",
                None,
                row,
            )?;
        }

        let mut standalone_positions_inserted = 0;
        for row in &session.standalone_positions {
            let position = crate::lifecycle::imported_position(row);
            let json =
                serde_json::to_string(&position).map_err(StoreError::SerializeLifecycleRow)?;
            standalone_positions_inserted += transaction
                .execute(
                    "INSERT INTO materialised_positions(
                        vehicle_id, position_id, drive_id, car_id, position_json,
                        speed, power, est_battery_range_km, fan_status,
                        driver_temp_setting, passenger_temp_setting,
                        is_climate_on, is_rear_defroster_on, is_front_defroster_on,
                        battery_heater, battery_heater_on, battery_heater_no_power,
                        tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr
                     ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                               ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
                     ON CONFLICT(vehicle_id, position_id) DO NOTHING",
                    params![
                        vehicle,
                        position.id,
                        car_id,
                        json,
                        position.speed,
                        position.power,
                        position.est_battery_range_km,
                        position.fan_status,
                        position.driver_temp_setting,
                        position.passenger_temp_setting,
                        position.is_climate_on.map(i64::from),
                        position.is_rear_defroster_on.map(i64::from),
                        position.is_front_defroster_on.map(i64::from),
                        position.battery_heater.map(i64::from),
                        position.battery_heater_on.map(i64::from),
                        position.battery_heater_no_power.map(i64::from),
                        position.tpms_pressure_fl,
                        position.tpms_pressure_fr,
                        position.tpms_pressure_rl,
                        position.tpms_pressure_rr,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }

        let watermarks = [
            ("drives", session.watermarks.drives),
            ("positions", session.watermarks.positions),
            ("charging_processes", session.watermarks.charging_processes),
            ("charges", session.watermarks.charges),
            ("states", session.watermarks.states),
            ("updates", session.watermarks.updates),
        ];
        for (domain, watermark) in watermarks {
            transaction
                .execute(
                    "INSERT INTO lifecycle_source_watermarks(
                        source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(source_id, vehicle_id, domain) DO UPDATE SET
                        max_source_row_id = MAX(max_source_row_id, excluded.max_source_row_id),
                        max_timestamp_ms = MAX(max_timestamp_ms, excluded.max_timestamp_ms)",
                    params![
                        source,
                        vehicle,
                        domain,
                        watermark.max_id,
                        watermark.max_timestamp_ms
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        let json = seeded
            .encode()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        transaction
            .execute(
                "INSERT INTO vehicle_lifecycle_state(
                    vehicle_id, car_id, last_observation_id, open_session_json,
                    quarantined, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, 0, ?5)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    car_id = excluded.car_id,
                    open_session_json = excluded.open_session_json,
                    updated_at_ms = MAX(updated_at_ms, excluded.updated_at_ms)",
                params![
                    vehicle,
                    car_id,
                    previous
                        .as_ref()
                        .map_or(0, |record| record.last_observation_id),
                    json,
                    updated_at_ms,
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        mark_export_dirty_in_transaction(&transaction, vehicle_id)?;

        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(OpenSessionSeedReport {
            provisional_rows_inserted: inserted,
            standalone_positions_inserted,
            watermarks_written: watermarks.len(),
            no_op: false,
        })
    }

    /// Reconstruct the full imported open-session view after a restart.
    pub fn load_imported_open_session(
        &self,
        source_id: Uuid,
        vehicle_id: Uuid,
    ) -> Result<Option<TeslaMateOpenSession>, StoreError> {
        if source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT domain, row_json FROM lifecycle_open_rows
                 WHERE source_id = ?1 AND vehicle_id = ?2
                 ORDER BY source_table, source_row_id",
            )
            .map_err(StoreError::Query)?;
        let mut session = TeslaMateOpenSession::default();
        let mut found = false;
        let rows = statement
            .query_map(
                params![source_id.to_string(), vehicle_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(StoreError::Query)?;
        for row in rows {
            let (domain, json) = row.map_err(StoreError::Query)?;
            found = true;
            match domain.as_str() {
                "drive" => {
                    session.drive = Some(
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                    )
                }
                "position" => session.drive_positions.push(
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                "charge" => {
                    session.charge = Some(
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                    )
                }
                "charge_sample" => session.charge_samples.push(
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                "state" => {
                    session.state = Some(
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                    )
                }
                "standalone_position" => session.standalone_positions.push(
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?,
                ),
                _ => return Err(StoreError::InvalidLifecycleSession),
            }
        }
        let mut watermark_statement = connection
            .prepare(
                "SELECT domain, max_source_row_id, max_timestamp_ms
                 FROM lifecycle_source_watermarks
                 WHERE source_id = ?1 AND vehicle_id = ?2",
            )
            .map_err(StoreError::Query)?;
        let watermarks = watermark_statement
            .query_map(
                params![source_id.to_string(), vehicle_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        crate::teslamate_projection::TeslaMateSourceWatermark {
                            max_id: row.get(1)?,
                            max_timestamp_ms: row.get(2)?,
                        },
                    ))
                },
            )
            .map_err(StoreError::Query)?;
        for watermark in watermarks {
            let (domain, value) = watermark.map_err(StoreError::Query)?;
            match domain.as_str() {
                "drives" => session.watermarks.drives = value,
                "positions" => session.watermarks.positions = value,
                "charging_processes" => session.watermarks.charging_processes = value,
                "charges" => session.watermarks.charges = value,
                "states" => session.watermarks.states = value,
                "updates" => session.watermarks.updates = value,
                _ => return Err(StoreError::InvalidLifecycleSession),
            }
        }
        if !found {
            return Ok(None);
        }
        session.car_id = session
            .drive
            .as_ref()
            .map(|row| row.car_id)
            .or_else(|| session.charge.as_ref().map(|row| row.car_id))
            .or_else(|| session.state.as_ref().map(|row| row.car_id))
            .or_else(|| session.drive_positions.first().map(|row| row.car_id))
            .unwrap_or_default();
        session
            .validate()
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        Ok(Some(session))
    }

    /// Preserve imported geofence labels and geometry as a durable, append-only
    /// catalog. Invalid geometry is skipped so unrelated history can proceed.
    pub fn upsert_geofences(
        &self,
        vehicle_id: Uuid,
        geofences: &[crate::teslamate_projection::TeslaMateGeofence],
    ) -> Result<usize, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let mut inserted = 0;
        for geofence in geofences {
            let Some((latitude, longitude, radius_m)) = geofence.valid_geometry() else {
                continue;
            };
            if geofence.name.trim().is_empty() || geofence.name.len() > 256 {
                continue;
            }
            inserted += transaction
                .execute(
                    "INSERT INTO geofences(
                        vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                        billing_type, cost_per_unit, session_fee
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(vehicle_id, source_geofence_id) DO NOTHING",
                    params![
                        vehicle_id.to_string(),
                        geofence.id,
                        geofence.name.trim(),
                        latitude,
                        longitude,
                        radius_m,
                        geofence
                            .billing_type
                            .map(crate::hub_pack::GeofenceBillingType::as_str),
                        geofence.cost_per_unit,
                        geofence.session_fee,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "UPDATE geofences SET
                        name = ?3, latitude = ?4, longitude = ?5, radius_m = ?6,
                        billing_type = COALESCE(?7, billing_type),
                        cost_per_unit = COALESCE(?8, cost_per_unit),
                        session_fee = COALESCE(?9, session_fee)
                     WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
                    params![
                        vehicle_id.to_string(),
                        geofence.id,
                        geofence.name.trim(),
                        latitude,
                        longitude,
                        radius_m,
                        geofence
                            .billing_type
                            .map(crate::hub_pack::GeofenceBillingType::as_str),
                        geofence.cost_per_unit,
                        geofence.session_fee,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(inserted)
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
        Self::commit_lifecycle_delta_in_transaction(&transaction, commit)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    fn maybe_stream_fault(&self, point: StreamFaultPoint) -> Result<(), StoreError> {
        #[cfg(test)]
        {
            let mut fault = self.stream_fault.lock().expect("stream fault lock");
            if fault.as_ref().is_some_and(|value| *value == point) {
                *fault = None;
                return Err(StoreError::InjectedStreamFault(point.label()));
            }
        }
        #[cfg(not(test))]
        let _ = point;
        Ok(())
    }

    #[cfg(test)]
    pub fn inject_stream_fault(&self, point: StreamFaultPoint) {
        *self.stream_fault.lock().expect("stream fault lock") = Some(point);
    }

    fn commit_lifecycle_delta_in_transaction(
        transaction: &Transaction<'_>,
        commit: &LifecycleCommit<'_>,
    ) -> Result<(), StoreError> {
        let mut delta = commit.delta.clone();
        let session = crate::lifecycle::OpenSessionState::decode(commit.open_session_json)
            .map_err(|_| StoreError::InvalidLifecycleSession)?;
        let lifecycle_source_id: String = transaction
            .query_row(
                "SELECT source_id FROM vehicles WHERE vehicle_id = ?1",
                params![commit.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::LifecycleWrite)?;
        let vehicle_key = commit.vehicle_id.to_string();
        for position in &delta.open_drive_positions {
            insert_open_row(
                &transaction,
                &lifecycle_source_id,
                "positions",
                position.id,
                &vehicle_key,
                commit.car_id,
                "position",
                position.drive_id,
                position,
            )?;
        }
        for sample in &delta.open_charge_samples {
            insert_open_row(
                &transaction,
                &lifecycle_source_id,
                "charges",
                sample.id,
                &vehicle_key,
                commit.car_id,
                "charge_sample",
                Some(sample.charge_process_id),
                sample,
            )?;
        }
        let fences = load_geofence_fences(&transaction, commit.vehicle_id)?;
        crate::lifecycle::apply_geofence_labels(&mut delta, &fences);
        let free_supercharging = transaction
            .query_row(
                "SELECT free_supercharging FROM car_settings
                 WHERE vehicle_id = ?1 AND car_id = ?2",
                params![commit.vehicle_id.to_string(), commit.car_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(StoreError::LifecycleWrite)?
            .unwrap_or(0)
            != 0;

        if let Some(patch) = session.car_metadata.as_ref() {
            let existing_json: Option<String> = transaction
                .query_row(
                    "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                    params![commit.vehicle_id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::LifecycleWrite)?;
            let existing = existing_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(StoreError::DeserializeLifecycleRow)?;
            let (fallback_name, fallback_vin): (Option<String>, Option<String>) = transaction
                .query_row(
                    "SELECT display_name, vin FROM vehicles WHERE vehicle_id = ?1",
                    params![commit.vehicle_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(StoreError::LifecycleWrite)?;
            let car = patch.into_car(
                commit.car_id,
                existing.as_ref(),
                fallback_name,
                fallback_vin,
            );
            let car_json =
                serde_json::to_string(&car).map_err(StoreError::SerializeLifecycleRow)?;
            let car_name = car.name.clone();
            let car_vin = car.vin.clone();
            transaction
                .execute(
                    "UPDATE vehicles SET display_name = COALESCE(?1, display_name), \
                         vin = COALESCE(?2, vin), last_seen_at_ms = MAX(last_seen_at_ms, ?3) \
                     WHERE vehicle_id = ?4",
                    params![
                        car_name,
                        car_vin,
                        commit.updated_at_ms,
                        commit.vehicle_id.to_string()
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "INSERT INTO materialised_cars(vehicle_id, car_id, car_json) \
                     VALUES (?1, ?2, ?3) \
                     ON CONFLICT(vehicle_id) DO UPDATE SET \
                         car_id = excluded.car_id, car_json = excluded.car_json",
                    params![commit.vehicle_id.to_string(), car.id, car_json],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "car",
                car.id,
                commit.car_id,
                "upsert",
                &car_json,
            )?;
        }

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
        mark_export_dirty_in_transaction(transaction, commit.vehicle_id)?;

        for drive in &delta.drives {
            let drive_json =
                serde_json::to_string(drive).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_drives(
                        vehicle_id, drive_id, car_id, drive_json,
                        inside_temp_avg, power_max, power_min,
                        start_ideal_range_km, end_ideal_range_km, ascent, descent
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                    ON CONFLICT(vehicle_id, drive_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        drive_json = excluded.drive_json,
                        inside_temp_avg = excluded.inside_temp_avg,
                        power_max = excluded.power_max,
                        power_min = excluded.power_min,
                        start_ideal_range_km = excluded.start_ideal_range_km,
                        end_ideal_range_km = excluded.end_ideal_range_km,
                        ascent = excluded.ascent,
                        descent = excluded.descent",
                    params![
                        commit.vehicle_id.to_string(),
                        drive.id,
                        commit.car_id,
                        drive_json,
                        drive.inside_temp_avg,
                        drive.power_max,
                        drive.power_min,
                        drive.start_ideal_range_km,
                        drive.end_ideal_range_km,
                        drive.ascent,
                        drive.descent
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "drive",
                drive.id,
                commit.car_id,
                "upsert",
                &drive_json,
            )?;
        }
        for position in &delta.positions {
            let position_json =
                serde_json::to_string(position).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_positions(
                        vehicle_id, position_id, drive_id, car_id, position_json,
                        speed, power, est_battery_range_km, fan_status,
                        driver_temp_setting, passenger_temp_setting,
                        is_climate_on, is_rear_defroster_on, is_front_defroster_on,
                        battery_heater, battery_heater_on, battery_heater_no_power,
                        tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                               ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                    params![
                        commit.vehicle_id.to_string(),
                        position.id,
                        position.drive_id,
                        commit.car_id,
                        position_json,
                        position.speed,
                        position.power,
                        position.est_battery_range_km,
                        position.fan_status,
                        position.driver_temp_setting,
                        position.passenger_temp_setting,
                        position.is_climate_on.map(i64::from),
                        position.is_rear_defroster_on.map(i64::from),
                        position.is_front_defroster_on.map(i64::from),
                        position.battery_heater.map(i64::from),
                        position.battery_heater_on.map(i64::from),
                        position.battery_heater_no_power.map(i64::from),
                        position.tpms_pressure_fl,
                        position.tpms_pressure_fr,
                        position.tpms_pressure_rl,
                        position.tpms_pressure_rr,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "position",
                position.id,
                commit.car_id,
                "upsert",
                &position_json,
            )?;
        }
        for charge in &delta.charges {
            let mut charge = charge.clone();
            let start_fence = delta
                .charge_start_coordinates
                .iter()
                .find(|(id, _, _)| *id == charge.id)
                .and_then(|(_, latitude, longitude)| {
                    crate::lifecycle::match_geofence(*latitude, *longitude, &fences)
                });
            if charge.geofence.is_none() {
                charge.geofence = start_fence.map(|fence| fence.name.clone());
            }
            if charge.billing_type.is_none() {
                charge.billing_type = start_fence.and_then(|fence| fence.billing_type);
            }
            if charge.cost_per_unit.is_none() {
                charge.cost_per_unit = start_fence.and_then(|fence| fence.cost_per_unit);
            }
            if charge.session_fee.is_none() {
                charge.session_fee = start_fence.and_then(|fence| fence.session_fee);
            }
            if charge.cost.is_none() {
                charge.cost = crate::lifecycle::calculate_charge_cost(
                    charge.fast_charger_type.as_deref(),
                    free_supercharging,
                    charge.charge_energy_added,
                    charge.charge_energy_used_kwh,
                    charge.duration_min,
                    start_fence.and_then(|fence| {
                        fence
                            .billing_type
                            .map(|billing_type| crate::lifecycle::ChargeTariff {
                                billing_type,
                                cost_per_unit: fence.cost_per_unit,
                                session_fee: fence.session_fee,
                            })
                    }),
                );
            }
            let charge_json =
                serde_json::to_string(&charge).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_charges(
                        vehicle_id, charge_id, car_id, charge_json
                    ) VALUES (?1, ?2, ?3, ?4)
                    ON CONFLICT(vehicle_id, charge_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        charge_json = excluded.charge_json",
                    params![
                        commit.vehicle_id.to_string(),
                        charge.id,
                        commit.car_id,
                        charge_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "charge",
                charge.id,
                commit.car_id,
                "upsert",
                &charge_json,
            )?;
        }
        for sample in &delta.charge_samples {
            let sample_json =
                serde_json::to_string(sample).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_charge_samples(
                        vehicle_id, sample_id, charge_id, sample_json
                    ) VALUES (?1, ?2, ?3, ?4)
                    ON CONFLICT(vehicle_id, sample_id) DO UPDATE SET
                        charge_id = excluded.charge_id,
                        sample_json = excluded.sample_json",
                    params![
                        commit.vehicle_id.to_string(),
                        sample.id,
                        sample.charge_process_id,
                        sample_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "charge_sample",
                sample.id,
                commit.car_id,
                "upsert",
                &sample_json,
            )?;
        }

        for drive in &delta.drives {
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'position'
                       AND parent_source_row_id = ?2",
                    params![vehicle_key, drive.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'drive' AND source_row_id = ?2",
                    params![vehicle_key, drive.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        for charge in &delta.charges {
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'charge_sample'
                       AND parent_source_row_id = ?2",
                    params![vehicle_key, charge.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "DELETE FROM lifecycle_open_rows
                     WHERE vehicle_id = ?1 AND domain = 'charge' AND source_row_id = ?2",
                    params![vehicle_key, charge.id],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        for state in &delta.states {
            if state.end_date_ms.is_some() {
                transaction
                    .execute(
                        "DELETE FROM lifecycle_open_rows
                         WHERE vehicle_id = ?1 AND domain = 'state' AND source_row_id = ?2",
                        params![vehicle_key, state.id],
                    )
                    .map_err(StoreError::LifecycleWrite)?;
            }
        }
        for state in &delta.states {
            let state_json =
                serde_json::to_string(state).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_states(
                        vehicle_id, state_id, car_id, state_json
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(vehicle_id, state_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        state_json = excluded.state_json",
                    params![
                        commit.vehicle_id.to_string(),
                        state.id,
                        commit.car_id,
                        state_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "state",
                state.id,
                commit.car_id,
                "upsert",
                &state_json,
            )?;
        }
        for update in &delta.updates {
            let update_json =
                serde_json::to_string(update).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "INSERT INTO materialised_updates(
                        vehicle_id, update_id, car_id, update_json
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(vehicle_id, update_id) DO UPDATE SET
                        car_id = excluded.car_id,
                        update_json = excluded.update_json",
                    params![
                        commit.vehicle_id.to_string(),
                        update.id,
                        commit.car_id,
                        update_json
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                commit.vehicle_id,
                "update",
                update.id,
                commit.car_id,
                "upsert",
                &update_json,
            )?;
        }

        enqueue_address_jobs(&transaction, commit.vehicle_id, &delta)?;

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
        let states = load_json_rows(
            &connection,
            "SELECT state_json FROM materialised_states WHERE vehicle_id = ?1 ORDER BY state_id ASC",
            &vehicle_key,
        )?;
        let updates = load_json_rows(
            &connection,
            "SELECT update_json FROM materialised_updates WHERE vehicle_id = ?1 ORDER BY update_id ASC",
            &vehicle_key,
        )?;
        let car = connection
            .query_row(
                "SELECT car_json FROM materialised_cars WHERE vehicle_id = ?1",
                params![vehicle_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(StoreError::DeserializeLifecycleRow)?;
        Ok(MaterialisedHistory {
            car,
            drives,
            positions,
            charges,
            charge_samples,
            states,
            updates,
        })
    }

    pub fn terrain_candidates(
        &self,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<TerrainCandidate>, StoreError> {
        let limit = i64::from(limit.min(1_000));
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT p.vehicle_id, p.position_json
                 FROM materialised_positions p
                 JOIN materialised_drives d
                   ON d.vehicle_id = p.vehicle_id AND d.drive_id = p.drive_id
                 LEFT JOIN terrain_elevation_provenance e
                   ON e.vehicle_id = p.vehicle_id AND e.position_id = p.position_id
                 LEFT JOIN terrain_enrichment_state c
                   ON c.vehicle_id = p.vehicle_id
                 WHERE json_extract(p.position_json, '$.elevation') IS NULL
                   AND (e.status IS NULL OR
                        (e.status = 'failed' AND COALESCE(e.retry_after_ms, 0) <= ?1))
                   AND (p.position_id > COALESCE(c.cursor_position_id, 0)
                        OR e.status = 'failed')
                   AND NOT EXISTS (
                       SELECT 1 FROM materialised_positions streamed
                       WHERE streamed.vehicle_id = p.vehicle_id
                         AND streamed.drive_id = p.drive_id
                         AND json_extract(streamed.position_json, '$.odometer') IS NOT NULL
                         AND json_extract(
                               streamed.position_json,
                               '$.ideal_battery_range_km'
                             ) IS NULL
                   )
                 ORDER BY p.vehicle_id ASC, p.position_id ASC
                 LIMIT ?2",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![now_ms, limit], |row| {
                let vehicle_id: String = row.get(0)?;
                let position_json: String = row.get(1)?;
                Ok((vehicle_id, position_json))
            })
            .map_err(StoreError::Query)?;
        rows.map(|row| {
            let (vehicle_id, position_json) = row.map_err(StoreError::Query)?;
            let vehicle_id = Uuid::parse_str(&vehicle_id).map_err(|_| StoreError::InvalidVehicleId)?;
            let position = serde_json::from_str(&position_json)
                .map_err(StoreError::DeserializeLifecycleRow)?;
            Ok(TerrainCandidate {
                vehicle_id,
                position,
            })
        })
        .collect()
    }

    pub fn record_terrain_failure(
        &self,
        candidate: &TerrainCandidate,
        error_code: &str,
        retry_after_ms: i64,
        attempted_at_ms: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        upsert_terrain_provenance(
            &transaction,
            candidate,
            None,
            None,
            None,
            None,
            "failed",
            Some(error_code),
            retry_after_ms,
            attempted_at_ms,
        )?;
        advance_terrain_cursor(&transaction, candidate, attempted_at_ms)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)
    }

    pub fn apply_terrain_result(
        &self,
        candidate: &TerrainCandidate,
        elevation_m: Option<i16>,
        tile_name: &str,
        tile_hash: &str,
        dataset_source: &str,
        dataset_version: &str,
        attempted_at_ms: i64,
    ) -> Result<bool, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let vehicle_key = candidate.vehicle_id.to_string();
        let current_json: String = transaction
            .query_row(
                "SELECT position_json FROM materialised_positions
                 WHERE vehicle_id = ?1 AND position_id = ?2",
                params![vehicle_key, candidate.position.id],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let mut position: ProjectionPosition = serde_json::from_str(&current_json)
            .map_err(StoreError::DeserializeLifecycleRow)?;
        let changed = position.elevation.is_none() && elevation_m.is_some();
        if changed {
            position.elevation = elevation_m.map(i64::from);
            let position_json = serde_json::to_string(&position)
                .map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_positions SET position_json = ?3
                     WHERE vehicle_id = ?1 AND position_id = ?2",
                    params![vehicle_key, position.id, position_json],
                )
                .map_err(StoreError::LifecycleWrite)?;
            if let Some(drive_id) = position.drive_id {
                recompute_terrain_drive(&transaction, &vehicle_key, drive_id)?;
                let drive_json: String = transaction
                    .query_row(
                        "SELECT drive_json FROM materialised_drives
                         WHERE vehicle_id = ?1 AND drive_id = ?2",
                        params![vehicle_key, drive_id],
                        |row| row.get(0),
                    )
                    .map_err(StoreError::LifecycleWrite)?;
                record_sync_mutation_in_transaction(
                    &transaction,
                    candidate.vehicle_id,
                    "drive",
                    drive_id,
                    position.car_id,
                    "upsert",
                    &drive_json,
                )?;
            }
            record_sync_mutation_in_transaction(
                &transaction,
                candidate.vehicle_id,
                "position",
                position.id,
                position.car_id,
                "upsert",
                &position_json,
            )?;
        }
        upsert_terrain_provenance(
            &transaction,
            candidate,
            Some(tile_name),
            Some(tile_hash),
            Some(dataset_source),
            Some(dataset_version),
            if elevation_m.is_some() { "success" } else { "void" },
            None,
            0,
            attempted_at_ms,
        )?;
        advance_terrain_cursor(&transaction, candidate, attempted_at_ms)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(changed)
    }

    pub fn publish_terrain_revision(
        &self,
        vehicle_id: Uuid,
        cursor_key: &CursorKey,
        minimum_free_bytes: u64,
    ) -> Result<bool, StoreError> {
        let history = self.materialised_history(vehicle_id)?;
        let Some(car) = history.car.clone() else {
            return Err(StoreError::TerrainCarMissing(vehicle_id));
        };
        let connection = self.open()?;
        let (source_id, generation): (String, i64) = connection
            .query_row(
                "SELECT v.source_id, s.generation
                 FROM vehicles v JOIN sources s ON s.source_id = v.source_id
                 WHERE v.vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(StoreError::Query)?;
        let account_id = Uuid::parse_str(&source_id).map_err(|_| StoreError::InvalidSourceId)?;
        let generation = u64::try_from(generation).map_err(|_| StoreError::InvalidStoredSequence)?;
        let snapshot = ProjectionSnapshot {
            cars: vec![car],
            drives: history.drives,
            positions: history.positions,
            charges: history.charges,
            charge_samples: history.charge_samples,
        };
        let fingerprint = Sha256Digest::from_bytes(
            Sha256::digest(
                serde_json::to_vec(&(&snapshot, &history.states, &history.updates))
                    .map_err(StoreError::SerializeLifecycleRow)?,
            )
            .into(),
        );
        if self.snapshot_fingerprint_is_current(vehicle_id, fingerprint.clone())? {
            return Ok(false);
        }
        // The collector invokes terrain publication under the same outer
        // publication gate as its outbox and lifecycle writes.
        let sequence = self.next_full_snapshot_sequence_while_gated(vehicle_id)?;
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: ProjectionBinding {
                installation_id: self.installation_id()?,
                account_id,
                vehicle_id,
                generation,
                selected_car_id: snapshot.cars[0].id,
            },
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &snapshot,
        };
        let writer = ProjectionPackWriter::new(self.packs_dir())
            .with_minimum_free_bytes(minimum_free_bytes);
        let built = writer
            .write_full_snapshot_with_states_and_updates(
                &request,
                &history.states,
                &history.updates,
            )
            .map_err(StoreError::TerrainPack)?;
        let manifest = request
            .signed_manifest_with_states_and_updates(
                &built,
                &history.states,
                &history.updates,
                cursor_key,
            )
            .map_err(StoreError::TerrainPack)?;
        self.publish_manifest(&manifest)?;
        self.record_snapshot_fingerprint(&manifest, fingerprint)?;
        Ok(true)
    }

    /// Check database integrity, report quarantined lifecycle state, and remove
    /// orphaned transport packs that are not referenced in the manifest catalog.
    ///
    /// A quarantine is evidence of a semantic projection failure. Clearing it
    /// without reconstructing from the immutable journal would make a damaged
    /// cursor appear healthy, so this safe repair deliberately preserves it.
    pub fn repair(&self) -> Result<RepairReport, StoreError> {
        let _publication_gate = self.try_acquire_publication_gate()?;
        self.verify_referenced_packs()?;
        let connection = self.open()?;
        let quarantined_sessions_preserved: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM vehicle_lifecycle_state WHERE quarantined != 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Query)?;
        let quarantined_sessions_preserved = usize::try_from(quarantined_sessions_preserved)
            .map_err(|_| StoreError::InvalidStoredCount)?;

        let mut catalog_shas = std::collections::HashSet::new();
        let mut statement = connection
            .prepare("SELECT sha256 FROM sync_packs")
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(StoreError::Query)?;
        for row in rows {
            let sha = row.map_err(StoreError::Query)?;
            catalog_shas.insert(sha);
        }

        let mut orphaned_packs_removed = 0;
        let mut freed_bytes = 0;
        for packs_dir in [
            self.packs_dir().to_path_buf(),
            self.packs_dir().join("sha256"),
        ] {
            if let Ok(entries) = std::fs::read_dir(packs_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let is_orphaned = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|name| name.strip_suffix(".sqlite.zst"))
                        .is_some_and(|sha| !catalog_shas.contains(sha));
                    if is_orphaned {
                        if let Ok(metadata) = entry.metadata() {
                            freed_bytes += metadata.len();
                        }
                        if std::fs::remove_file(&path).is_ok() {
                            orphaned_packs_removed += 1;
                        }
                    }
                }
            }
        }

        Ok(RepairReport {
            status: "ok".to_owned(),
            sqlite_integrity: "ok".to_owned(),
            quarantined_sessions_preserved,
            orphaned_packs_removed,
            freed_bytes,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct RepairReport {
    pub status: String,
    pub sqlite_integrity: String,
    pub quarantined_sessions_preserved: usize,
    pub orphaned_packs_removed: usize,
    pub freed_bytes: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOutboxClaim {
    pub vehicle_id: Uuid,
    pub dirty_revision: i64,
    pub attempts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncMutation {
    pub vehicle_id: Uuid,
    pub revision: i64,
    pub entity: String,
    pub entity_id: i64,
    pub car_id: i64,
    pub operation: String,
    pub payload_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncMutationClaim {
    pub vehicle_id: Uuid,
    pub from_revision: i64,
    pub to_revision: i64,
    pub mutations: Vec<SyncMutation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenSessionSeedReport {
    pub provisional_rows_inserted: usize,
    pub standalone_positions_inserted: usize,
    pub watermarks_written: usize,
    pub no_op: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MaterialisedHistory {
    pub car: Option<crate::hub_pack::ProjectionCar>,
    pub drives: Vec<crate::hub_pack::ProjectionDrive>,
    pub positions: Vec<crate::hub_pack::ProjectionPosition>,
    pub charges: Vec<crate::hub_pack::ProjectionCharge>,
    pub charge_samples: Vec<crate::hub_pack::ProjectionChargeSample>,
    pub states: Vec<crate::hub_pack::ProjectionState>,
    pub updates: Vec<crate::hub_pack::ProjectionUpdate>,
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
    pub tesla_eid: Option<i64>,
    pub tesla_vid: Option<i64>,
}

impl VehicleDescriptor {
    pub fn new(source_id: Uuid, source_vehicle_key: impl Into<String>) -> Self {
        Self {
            source_id,
            source_vehicle_key: source_vehicle_key.into(),
            vin: None,
            display_name: None,
            tesla_eid: None,
            tesla_vid: None,
        }
    }

    pub fn with_tesla_identity(mut self, eid: Option<i64>, vid: Option<i64>) -> Self {
        self.tesla_eid = eid;
        self.tesla_vid = vid;
        self
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
        if self.tesla_eid.is_some_and(|value| value <= 0)
            || self.tesla_vid.is_some_and(|value| value <= 0)
        {
            return Err(StoreError::InvalidVehicleIdentity);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservationTarget {
    vehicle_id: Uuid,
    source_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationWatermark {
    pub source_car_id: i64,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub observation_id: i64,
    pub observed_at_ms: Option<i64>,
    pub received_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationVerification {
    pub source_car_id: i64,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub after_observation_id: i64,
    pub latest_observation_id: Option<i64>,
    pub latest_observed_at_ms: Option<i64>,
    pub latest_received_at_ms: Option<i64>,
}

impl ObservationVerification {
    pub fn verified(&self) -> bool {
        self.latest_observation_id.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutboundRequestReceiptId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StreamSessionReceiptId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundRequestWatermark {
    pub receipt_id: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestTransport { OwnerApi, Stream, LegacyAuth }

impl OutboundRequestTransport {
    const fn as_str(self) -> &'static str { match self { Self::OwnerApi => "owner_api", Self::Stream => "stream", Self::LegacyAuth => "legacy_auth" } }
    fn parse(value: &str) -> Option<Self> { match value { "owner_api" => Some(Self::OwnerApi), "stream" => Some(Self::Stream), "legacy_auth" => Some(Self::LegacyAuth), _ => None } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestOperation { Products, VehicleProbe, VehicleData, TokenRefresh, StreamConnect, StreamSubscribe, StreamUnsubscribe }

impl OutboundRequestOperation {
    const fn as_str(self) -> &'static str { match self { Self::Products => "products", Self::VehicleProbe => "vehicle_probe", Self::VehicleData => "vehicle_data", Self::TokenRefresh => "token_refresh", Self::StreamConnect => "stream_connect", Self::StreamSubscribe => "stream_subscribe", Self::StreamUnsubscribe => "stream_unsubscribe" } }
    fn parse(value: &str) -> Option<Self> { match value { "products" => Some(Self::Products), "vehicle_probe" => Some(Self::VehicleProbe), "vehicle_data" => Some(Self::VehicleData), "token_refresh" => Some(Self::TokenRefresh), "stream_connect" => Some(Self::StreamConnect), "stream_subscribe" => Some(Self::StreamSubscribe), "stream_unsubscribe" => Some(Self::StreamUnsubscribe), _ => None } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestSafetyClass { NonWakeEndpoint, ConditionalRead, DirectWakeCommand }

impl OutboundRequestSafetyClass {
    const fn as_str(self) -> &'static str { match self { Self::NonWakeEndpoint => "non_wake_endpoint", Self::ConditionalRead => "conditional_read", Self::DirectWakeCommand => "direct_wake_command" } }
    fn parse(value: &str) -> Option<Self> { match value { "non_wake_endpoint" => Some(Self::NonWakeEndpoint), "conditional_read" => Some(Self::ConditionalRead), "direct_wake_command" => Some(Self::DirectWakeCommand), _ => None } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestPrecondition { NotRequired, StreamPowerConfirmed }

impl OutboundRequestPrecondition {
    const fn as_str(self) -> &'static str { match self { Self::NotRequired => "not_required", Self::StreamPowerConfirmed => "stream_power_confirmed" } }
    fn parse(value: &str) -> Option<Self> { match value { "not_required" => Some(Self::NotRequired), "stream_power_confirmed" => Some(Self::StreamPowerConfirmed), _ => None } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestOutcome { Success, HttpError, Timeout, TransportError, AuthenticationRejected, ProtocolError, ResponseTooLarge, Cancelled }

impl OutboundRequestOutcome {
    const fn as_str(self) -> &'static str { match self { Self::Success => "success", Self::HttpError => "http_error", Self::Timeout => "timeout", Self::TransportError => "transport_error", Self::AuthenticationRejected => "authentication_rejected", Self::ProtocolError => "protocol_error", Self::ResponseTooLarge => "response_too_large", Self::Cancelled => "cancelled" } }
    fn parse(value: &str) -> Option<Self> { match value { "success" => Some(Self::Success), "http_error" => Some(Self::HttpError), "timeout" => Some(Self::Timeout), "transport_error" => Some(Self::TransportError), "authentication_rejected" => Some(Self::AuthenticationRejected), "protocol_error" => Some(Self::ProtocolError), "response_too_large" => Some(Self::ResponseTooLarge), "cancelled" => Some(Self::Cancelled), _ => None } }
}

/// Typed metadata committed before network I/O. There is deliberately no URL,
/// header, token, request body, response body, or arbitrary error-text field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequestStart {
    pub correlation_id: Uuid,
    pub vehicle_tesla_id: Option<i64>,
    pub transport: OutboundRequestTransport,
    pub operation: OutboundRequestOperation,
    pub safety_class: OutboundRequestSafetyClass,
    pub precondition: OutboundRequestPrecondition,
}

impl OutboundRequestStart {
    fn validate(&self) -> Result<(), StoreError> {
        if self.correlation_id.is_nil() { return Err(StoreError::NilOutboundRequestCorrelationId); }
        if self.vehicle_tesla_id.is_some_and(|id| id <= 0) { return Err(StoreError::InvalidOutboundRequestVehicleId); }
        if self.operation == OutboundRequestOperation::VehicleData && (self.safety_class != OutboundRequestSafetyClass::ConditionalRead || self.precondition != OutboundRequestPrecondition::StreamPowerConfirmed) { return Err(StoreError::InvalidVehicleDataAuditPrecondition); }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequestCompletion { pub outcome: OutboundRequestOutcome, pub http_status: Option<u16> }

impl OutboundRequestCompletion {
    fn validate(&self) -> Result<(), StoreError> { if self.http_status.is_some_and(|status| !(100..=599).contains(&status)) { return Err(StoreError::InvalidOutboundRequestHttpStatus); } Ok(()) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequestReceipt {
    pub id: OutboundRequestReceiptId,
    pub correlation_id: Uuid,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub vehicle_tesla_id: Option<i64>,
    pub transport: OutboundRequestTransport,
    pub operation: OutboundRequestOperation,
    pub safety_class: OutboundRequestSafetyClass,
    pub precondition: OutboundRequestPrecondition,
    pub outcome: Option<OutboundRequestOutcome>,
    pub http_status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoWakeVerification {
    pub after_receipt_id: i64,
    pub correlation_id: Uuid,
    pub matching_receipts: i64,
    pub unresolved_receipts: i64,
    pub unresolved_stream_sessions: i64,
    pub direct_wake_receipts: i64,
    pub conditional_without_power_receipts: i64,
    pub observation: Option<ObservationVerification>,
}

impl NoWakeVerification {
    /// An empty audit window is not proof: absence of integration data fails closed.
    pub fn audit_verified(&self) -> bool { self.matching_receipts > 0 && self.unresolved_receipts == 0 && self.unresolved_stream_sessions == 0 && self.direct_wake_receipts == 0 && self.conditional_without_power_receipts == 0 }
    pub fn verified(&self) -> bool { self.audit_verified() && self.observation.as_ref().is_none_or(ObservationVerification::verified) }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppendObservation {
    pub observation: ObservationRecord,
    pub inserted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnerObservationResult {
    pub append: AppendObservation,
    pub drives_closed: usize,
    pub charges_closed: usize,
    pub positions_materialised: usize,
    pub charge_samples_materialised: usize,
    pub lifecycle_quarantined: bool,
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
            let id = parse_stored_uuid("vehicle_id", &row.get::<_, String>(1).map_err(StoreError::Query)?)?;
            if found_kind == kind && !strong.contains(&id) && !secondary.contains(&id) {
                if kind == "tesla_vid" { secondary.push(id); } else { strong.push(id); }
            }
        }
        Ok(())
    };
    if let Some(eid) = descriptor.tesla_eid { find("tesla_eid", eid.to_string())?; }
    if let Some(vin) = &descriptor.vin { find("vin", vin.clone())?; }
    if let Some(vid) = descriptor.tesla_vid { find("tesla_vid", vid.to_string())?; }
    if strong.len() > 1 || (!strong.is_empty() && secondary.iter().any(|id| !strong.contains(id))) {
        return Err(StoreError::VehicleIdentityConflict);
    }
    if strong.len() == 1 { return Ok(strong.into_iter().next()); }
    if descriptor.tesla_eid.is_some() || descriptor.vin.is_some() { return Ok(None); }
    if secondary.len() > 1 { return Err(StoreError::VehicleIdentityConflict); }
    Ok(secondary.into_iter().next())
}

fn register_vehicle_aliases(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    descriptor: &VehicleDescriptor,
) -> Result<(), StoreError> {
    let mut aliases = vec![("source_key", format!("{}:{}", descriptor.source_id, descriptor.source_vehicle_key))];
    if let Some(eid) = descriptor.tesla_eid { aliases.push(("tesla_eid", eid.to_string())); }
    if let Some(vin) = &descriptor.vin { aliases.push(("vin", vin.clone())); }
    if let Some(vid) = descriptor.tesla_vid { aliases.push(("tesla_vid", vid.to_string())); }
    for (kind, value) in aliases {
        let conflict: Option<String> = transaction
            .query_row(
                "SELECT vehicle_id FROM vehicle_identity_aliases WHERE alias_kind = ?1 AND alias_value = ?2",
                params![kind, value], |row| row.get(0),
            ).optional().map_err(StoreError::Query)?;
        if let Some(existing) = conflict && existing != vehicle_id.to_string() {
            if kind == "tesla_vid" && (descriptor.tesla_eid.is_some() || descriptor.vin.is_some()) {
                continue;
            }
            return Err(StoreError::VehicleIdentityConflict);
        }
        transaction.execute(
            "INSERT OR IGNORE INTO vehicle_identity_aliases
             (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![kind, value, vehicle_id.to_string(), descriptor.source_id.to_string(), descriptor.source_vehicle_key],
        ).map_err(StoreError::RegisterVehicle)?;
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
            Err(StoreError::VehicleSourceMismatch { vehicle_id, source_id })
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

#[derive(Debug, Clone, Copy)]
struct ObservationMetadata {
    observation_id: i64,
    observed_at_ms: i64,
    received_at_ms: i64,
}

fn latest_observation_metadata(
    connection: &Connection,
    vehicle_id: Uuid,
    after_observation_id: Option<i64>,
) -> Result<Option<ObservationMetadata>, StoreError> {
    connection
        .query_row(
            "SELECT observation_id, observed_at_ms, received_at_ms
             FROM raw_observations
             WHERE vehicle_id = ?1
               AND (?2 IS NULL OR observation_id > ?2)
             ORDER BY observation_id DESC LIMIT 1",
            params![vehicle_id.to_string(), after_observation_id],
            |row| {
                Ok(ObservationMetadata {
                    observation_id: row.get(0)?,
                    observed_at_ms: row.get(1)?,
                    received_at_ms: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::Query)
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

    if version == 5 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS snapshot_fingerprints (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    fingerprint_sha256 BLOB NOT NULL CHECK(length(fingerprint_sha256) = 32)
                ) STRICT;
                PRAGMA user_version = 6;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 6;
    }

    if version == 6 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS vehicle_snapshot_sequences (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    next_sequence INTEGER NOT NULL CHECK(next_sequence >= 2)
                ) STRICT;
                PRAGMA user_version = 7;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 7;
    }

    if version == 7 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_positions RENAME TO materialised_positions_v7;
                CREATE TABLE materialised_positions (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    position_id INTEGER NOT NULL CHECK(position_id > 0),
                    drive_id INTEGER CHECK(drive_id IS NULL OR drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    position_json TEXT NOT NULL CHECK(json_valid(position_json)),
                    PRIMARY KEY (vehicle_id, position_id)
                ) STRICT;
                INSERT INTO materialised_positions(
                    vehicle_id, position_id, drive_id, car_id, position_json
                )
                SELECT vehicle_id, position_id, drive_id, car_id, position_json
                FROM materialised_positions_v7;
                DROP TABLE materialised_positions_v7;
                CREATE INDEX materialised_positions_drive
                    ON materialised_positions(vehicle_id, drive_id);
                PRAGMA user_version = 8;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 8;
    }

    if version == 8 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE materialised_states (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    state_id INTEGER NOT NULL CHECK(state_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    state_json TEXT NOT NULL CHECK(json_valid(state_json)),
                    PRIMARY KEY (vehicle_id, state_id)
                ) STRICT;
                PRAGMA user_version = 9;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 9;
    }

    if version == 9 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE materialised_updates (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    update_id INTEGER NOT NULL CHECK(update_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    update_json TEXT NOT NULL CHECK(json_valid(update_json)),
                    PRIMARY KEY (vehicle_id, update_id)
                ) STRICT;
                PRAGMA user_version = 10;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 10;
    }

    if version == 10 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS materialised_cars (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    car_json TEXT NOT NULL CHECK(json_valid(car_json))
                ) STRICT;
                PRAGMA user_version = 11;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 11;
    }

    if version == 11 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS geofences (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    source_geofence_id INTEGER NOT NULL CHECK(source_geofence_id > 0),
                    name TEXT NOT NULL CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 256),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    radius_m REAL NOT NULL CHECK(radius_m > 0.0 AND radius_m <= 5000.0),
                    PRIMARY KEY(vehicle_id, source_geofence_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS geofences_vehicle_location
                    ON geofences(vehicle_id, latitude, longitude);
                PRAGMA user_version = 12;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 12;
    }

    if version == 12 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS address_cache (
                    osm_type TEXT NOT NULL CHECK(length(CAST(osm_type AS BLOB)) BETWEEN 1 AND 32),
                    osm_id INTEGER NOT NULL CHECK(osm_id > 0),
                    display_name TEXT NOT NULL
                        CHECK(length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256),
                    name TEXT CHECK(name IS NULL OR length(CAST(name AS BLOB)) <= 256),
                    PRIMARY KEY(osm_type, osm_id)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS address_lookup_cache (
                    lookup_key TEXT PRIMARY KEY NOT NULL
                        CHECK(length(CAST(lookup_key AS BLOB)) BETWEEN 1 AND 64),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    osm_type TEXT NOT NULL,
                    osm_id INTEGER NOT NULL,
                    looked_up_at_ms INTEGER NOT NULL CHECK(looked_up_at_ms >= 0),
                    FOREIGN KEY(osm_type, osm_id)
                        REFERENCES address_cache(osm_type, osm_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS address_lookup_cache_identity
                    ON address_lookup_cache(osm_type, osm_id);
                PRAGMA user_version = 13;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 13;
    }

    if version == 13 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS address_enrichment_jobs (
                    job_key TEXT PRIMARY KEY NOT NULL
                        CHECK(length(CAST(job_key AS BLOB)) BETWEEN 1 AND 256),
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    target_type TEXT NOT NULL CHECK(target_type IN ('drive', 'charge')),
                    target_id INTEGER NOT NULL CHECK(target_id > 0),
                    field TEXT NOT NULL CHECK(field IN ('start_address', 'end_address', 'address')),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    status TEXT NOT NULL CHECK(status IN ('pending', 'running', 'retry', 'complete')),
                    attempts INTEGER NOT NULL CHECK(attempts >= 0),
                    next_attempt_ms INTEGER NOT NULL CHECK(next_attempt_ms >= 0),
                    lease_until_ms INTEGER NOT NULL CHECK(lease_until_ms >= 0),
                    completed_at_ms INTEGER,
                    last_error TEXT
                ) STRICT;
                CREATE UNIQUE INDEX IF NOT EXISTS address_enrichment_target
                    ON address_enrichment_jobs(vehicle_id, target_type, target_id, field);
                PRAGMA user_version = 14;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 14;
    }

    if version == 14 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_positions ADD COLUMN battery_heater INTEGER
                    CHECK (battery_heater IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN battery_heater_on INTEGER
                    CHECK (battery_heater_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN battery_heater_no_power INTEGER
                    CHECK (battery_heater_no_power IN (0, 1));
                PRAGMA user_version = 15;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 15;
    }

    if version == 15 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_positions ADD COLUMN speed INTEGER;
                ALTER TABLE materialised_positions ADD COLUMN power REAL;
                ALTER TABLE materialised_positions ADD COLUMN est_battery_range_km REAL;
                ALTER TABLE materialised_positions ADD COLUMN fan_status INTEGER;
                ALTER TABLE materialised_positions ADD COLUMN driver_temp_setting REAL;
                ALTER TABLE materialised_positions ADD COLUMN passenger_temp_setting REAL;
                ALTER TABLE materialised_positions ADD COLUMN is_climate_on INTEGER
                    CHECK (is_climate_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN is_rear_defroster_on INTEGER
                    CHECK (is_rear_defroster_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN is_front_defroster_on INTEGER
                    CHECK (is_front_defroster_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_fl REAL;
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_fr REAL;
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_rl REAL;
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_rr REAL;
                PRAGMA user_version = 16;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 16;
    }

    if version == 16 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_drives ADD COLUMN inside_temp_avg REAL;
                ALTER TABLE materialised_drives ADD COLUMN power_max REAL;
                ALTER TABLE materialised_drives ADD COLUMN power_min REAL;
                ALTER TABLE materialised_drives ADD COLUMN start_ideal_range_km REAL;
                ALTER TABLE materialised_drives ADD COLUMN end_ideal_range_km REAL;
                ALTER TABLE materialised_drives ADD COLUMN ascent INTEGER;
                ALTER TABLE materialised_drives ADD COLUMN descent INTEGER;
                PRAGMA user_version = 17;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 17;
    }

    if version == 17 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE car_settings (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
                    use_streaming_api INTEGER NOT NULL CHECK(use_streaming_api IN (0, 1)),
                    suspend_after_idle_min INTEGER NOT NULL CHECK(suspend_after_idle_min > 0),
                    suspend_min INTEGER NOT NULL CHECK(suspend_min > 0),
                    req_not_unlocked INTEGER NOT NULL CHECK(req_not_unlocked IN (0, 1)),
                    free_supercharging INTEGER NOT NULL CHECK(free_supercharging IN (0, 1)),
                    lfp_battery INTEGER NOT NULL CHECK(lfp_battery IN (0, 1))
                ) STRICT;
                PRAGMA user_version = 18;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 18;
    }

    if version == 18 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE geofences ADD COLUMN billing_type TEXT
                    CHECK(billing_type IS NULL OR billing_type IN ('per_kwh', 'per_minute'));
                ALTER TABLE geofences ADD COLUMN cost_per_unit REAL;
                ALTER TABLE geofences ADD COLUMN session_fee REAL
                    CHECK(session_fee IS NULL OR session_fee >= 0.0);
                PRAGMA user_version = 19;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 19;
    }

    if version == 19 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS stream_watermarks (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    last_timestamp_ms INTEGER NOT NULL CHECK(last_timestamp_ms >= 0)
                ) STRICT;
                PRAGMA user_version = 20;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 20;
    }

    if version == 20 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS lifecycle_open_rows (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    source_table TEXT NOT NULL,
                    source_row_id INTEGER NOT NULL CHECK(source_row_id > 0),
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    domain TEXT NOT NULL CHECK(domain IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state',
                        'standalone_position'
                    )),
                    parent_source_row_id INTEGER,
                    row_json TEXT NOT NULL CHECK(json_valid(row_json)),
                    PRIMARY KEY(source_id, source_table, source_row_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS lifecycle_open_rows_vehicle_domain
                    ON lifecycle_open_rows(vehicle_id, domain, source_row_id);
                CREATE TABLE IF NOT EXISTS lifecycle_source_watermarks (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    domain TEXT NOT NULL,
                    max_source_row_id INTEGER,
                    max_timestamp_ms INTEGER,
                    PRIMARY KEY(source_id, vehicle_id, domain)
                ) STRICT;
                PRAGMA user_version = 21;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 21;
    }

    if version == 21 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE car_settings ADD COLUMN suspend_min_resolved INTEGER NOT NULL DEFAULT 1
                    CHECK(suspend_min_resolved IN (0, 1));
                PRAGMA user_version = 22;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 22;
    }

    if version == 22 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS terrain_enrichment_state (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    cursor_position_id INTEGER NOT NULL DEFAULT 0
                        CHECK(cursor_position_id >= 0),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS terrain_elevation_provenance (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    position_id INTEGER NOT NULL CHECK(position_id > 0),
                    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    elevation_m INTEGER,
                    tile_name TEXT,
                    tile_hash TEXT,
                    dataset_source TEXT,
                    dataset_version TEXT,
                    status TEXT NOT NULL CHECK(status IN ('success', 'void', 'failed')),
                    error_code TEXT,
                    attempts INTEGER NOT NULL CHECK(attempts >= 1),
                    attempted_at_ms INTEGER NOT NULL CHECK(attempted_at_ms >= 0),
                    retry_after_ms INTEGER NOT NULL CHECK(retry_after_ms >= 0),
                    PRIMARY KEY(vehicle_id, position_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS terrain_provenance_retry
                    ON terrain_elevation_provenance(status, retry_after_ms);
                PRAGMA user_version = 23;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 23;
    }

    if version == 23 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS vehicle_identity_aliases (
                    alias_kind TEXT NOT NULL CHECK(alias_kind IN ('source_key', 'tesla_eid', 'tesla_vid', 'vin')),
                    alias_value TEXT NOT NULL,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_vehicle_key TEXT NOT NULL,
                    PRIMARY KEY(alias_kind, alias_value),
                    CHECK(length(CAST(alias_value AS BLOB)) BETWEEN 1 AND 256),
                    CHECK(length(CAST(source_vehicle_key AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS vehicle_identity_aliases_vehicle
                    ON vehicle_identity_aliases(vehicle_id);
                DROP TRIGGER IF EXISTS raw_observations_match_vehicle_source;
                CREATE TRIGGER raw_observations_match_vehicle_source
                BEFORE INSERT ON raw_observations
                FOR EACH ROW
                WHEN NOT EXISTS (
                    SELECT 1 FROM vehicle_identity_aliases
                    WHERE vehicle_id = NEW.vehicle_id AND source_id = NEW.source_id
                )
                BEGIN
                    SELECT RAISE(ABORT, 'raw observation source and vehicle mismatch');
                END;
                INSERT OR IGNORE INTO vehicle_identity_aliases
                    (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
                SELECT 'vin', v.vin, v.vehicle_id, v.source_id, v.source_vehicle_key
                FROM vehicles v WHERE v.vin IS NOT NULL AND length(v.vin) > 0;
                INSERT OR IGNORE INTO vehicle_identity_aliases
                    (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
                SELECT 'source_key', v.source_id || ':' || v.source_vehicle_key,
                       v.vehicle_id, v.source_id, v.source_vehicle_key
                FROM vehicles v;
                INSERT OR IGNORE INTO vehicle_identity_aliases
                    (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
                SELECT 'tesla_eid', substr(v.source_vehicle_key, 5), v.vehicle_id,
                       v.source_id, v.source_vehicle_key
                FROM vehicles v
                WHERE v.source_vehicle_key GLOB 'eid:[0-9]*'
                  AND length(substr(v.source_vehicle_key, 5)) > 0;
                PRAGMA user_version = 24;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 24;
    }

    if version == 24 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sync_bases (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    snapshot_id TEXT NOT NULL UNIQUE,
                    base_sequence INTEGER NOT NULL CHECK(base_sequence >= 0),
                    base_digest TEXT NOT NULL CHECK(length(base_digest) = 64),
                    packs_json BLOB NOT NULL
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_deltas (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    from_sequence INTEGER NOT NULL CHECK(from_sequence >= 0),
                    to_sequence INTEGER NOT NULL CHECK(to_sequence > from_sequence),
                    parent_chain_digest TEXT NOT NULL CHECK(length(parent_chain_digest) = 64),
                    chain_digest TEXT NOT NULL CHECK(length(chain_digest) = 64),
                    pack_digest TEXT NOT NULL CHECK(length(pack_digest) = 64),
                    pack_json BLOB NOT NULL,
                    PRIMARY KEY(vehicle_id, from_sequence, to_sequence),
                    UNIQUE(vehicle_id, chain_digest)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_heads (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    base_snapshot_id TEXT NOT NULL REFERENCES sync_bases(snapshot_id) ON DELETE RESTRICT,
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0),
                    head_digest TEXT NOT NULL CHECK(length(head_digest) = 64),
                    terminal_cursor TEXT NOT NULL
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_deltas_vehicle_sequence
                    ON sync_deltas(vehicle_id, from_sequence, to_sequence);
                PRAGMA user_version = 25;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 25;
    }

    if version == 25 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS import_generations (
                    run_id TEXT PRIMARY KEY NOT NULL,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    status TEXT NOT NULL CHECK(status IN ('staging', 'promoting')),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS import_generations_vehicle
                    ON import_generations(vehicle_id, status);
                CREATE TABLE IF NOT EXISTS import_generation_sessions (
                    run_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES import_generations(run_id) ON DELETE CASCADE,
                    session_json TEXT NOT NULL CHECK(json_valid(session_json))
                ) STRICT;
                PRAGMA user_version = 26;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 26;
    }

    if version == 26 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE import_generations ADD COLUMN base_last_observation_id
                    INTEGER NOT NULL DEFAULT 0 CHECK(base_last_observation_id >= 0);
                ALTER TABLE import_generations ADD COLUMN base_updated_at_ms
                    INTEGER NOT NULL DEFAULT 0 CHECK(base_updated_at_ms >= 0);
                PRAGMA user_version = 27;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 27;
    }

    if version == 27 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS export_outbox (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    dirty_revision INTEGER NOT NULL CHECK(dirty_revision > 0),
                    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
                    next_attempt_ms INTEGER NOT NULL DEFAULT 0 CHECK(next_attempt_ms >= 0),
                    claimed_until_ms INTEGER NOT NULL DEFAULT 0 CHECK(claimed_until_ms >= 0),
                    last_error TEXT
                ) STRICT;
                PRAGMA user_version = 28;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 28;
    }

    if version == 28 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sync_mutation_sequences (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    next_revision INTEGER NOT NULL CHECK(next_revision > 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_mutations (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    revision INTEGER NOT NULL CHECK(revision > 0),
                    entity TEXT NOT NULL CHECK(entity IN
                        ('car', 'car_setting', 'geofence', 'address', 'drive',
                         'position', 'charge', 'charge_sample', 'state', 'update')),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    operation TEXT NOT NULL CHECK(operation IN ('upsert', 'tombstone')),
                    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
                    published INTEGER NOT NULL DEFAULT 0 CHECK(published IN (0, 1)),
                    claimed_until_ms INTEGER NOT NULL DEFAULT 0 CHECK(claimed_until_ms >= 0),
                    PRIMARY KEY(vehicle_id, revision)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_mutations_pending
                    ON sync_mutations(vehicle_id, published, revision, claimed_until_ms);
                PRAGMA user_version = 29;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 29;
    }

    if version == 29 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS mqtt_summary_revisions (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    revision INTEGER NOT NULL CHECK(revision > 0),
                    fields_json TEXT NOT NULL CHECK(json_valid(fields_json)),
                    healthy_clear_delivered INTEGER NOT NULL DEFAULT 0
                        CHECK(healthy_clear_delivered IN (0, 1)),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS mqtt_delivery_state (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    field TEXT NOT NULL,
                    topic TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    fingerprint TEXT NOT NULL CHECK(length(fingerprint) = 64),
                    qos INTEGER NOT NULL CHECK(qos = 1),
                    retain INTEGER NOT NULL CHECK(retain IN (0, 1)),
                    pending_revision INTEGER NOT NULL CHECK(pending_revision > 0),
                    pending INTEGER NOT NULL DEFAULT 1 CHECK(pending IN (0, 1)),
                    claimed_until_ms INTEGER NOT NULL DEFAULT 0 CHECK(claimed_until_ms >= 0),
                    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
                    last_error TEXT,
                    delivered_fingerprint TEXT,
                    phase INTEGER NOT NULL DEFAULT 0 CHECK(phase IN (0, 1)),
                    PRIMARY KEY(vehicle_id, field)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS mqtt_delivery_pending
                    ON mqtt_delivery_state(pending, claimed_until_ms, pending_revision);
                PRAGMA user_version = 30;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 30;
    }

    if version == 30 {
        // Existing rows have no trustworthy manifest identity. Preserve the
        // hash, but leave these nullable columns unset so it cannot skip a
        // later import by accidentally matching an arbitrary manifest.
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE snapshot_fingerprints ADD COLUMN snapshot_id TEXT;
                ALTER TABLE snapshot_fingerprints ADD COLUMN head_sequence INTEGER;
                CREATE INDEX IF NOT EXISTS snapshot_fingerprints_manifest
                    ON snapshot_fingerprints(snapshot_id, head_sequence);
                PRAGMA user_version = 31;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 31;
    }

    if version == 31 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS outbound_request_receipts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    vehicle_tesla_id INTEGER CHECK(vehicle_tesla_id > 0),
                    transport TEXT NOT NULL CHECK(transport IN ('owner_api', 'stream', 'legacy_auth')),
                    operation TEXT NOT NULL CHECK(operation IN ('products', 'vehicle_probe', 'vehicle_data', 'token_refresh', 'stream_connect', 'stream_subscribe', 'stream_unsubscribe')),
                    safety_class TEXT NOT NULL CHECK(safety_class IN ('non_wake_endpoint', 'conditional_read', 'direct_wake_command')),
                    precondition TEXT NOT NULL CHECK(precondition IN ('not_required', 'stream_power_confirmed')),
                    outcome TEXT NOT NULL CHECK(outcome IN ('started', 'success', 'http_error', 'timeout', 'transport_error', 'authentication_rejected', 'protocol_error', 'response_too_large', 'cancelled')),
                    http_status INTEGER CHECK(http_status BETWEEN 100 AND 599),
                    CHECK((outcome = 'started' AND completed_at_ms IS NULL AND duration_ms IS NULL AND http_status IS NULL) OR (outcome <> 'started' AND completed_at_ms IS NOT NULL AND duration_ms IS NOT NULL AND completed_at_ms >= started_at_ms AND duration_ms >= 0))
                ) STRICT;
                CREATE INDEX IF NOT EXISTS outbound_request_receipts_proof ON outbound_request_receipts(correlation_id, id, safety_class, outcome);
                CREATE INDEX IF NOT EXISTS outbound_request_receipts_retention ON outbound_request_receipts(outcome, completed_at_ms, id);
                PRAGMA user_version = 32;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 32;
    }

    if version == 32 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS stream_session_receipts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    vehicle_tesla_id INTEGER NOT NULL CHECK(vehicle_tesla_id > 0),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    outcome TEXT NOT NULL CHECK(outcome IN ('started', 'orderly_shutdown')),
                    unsubscribe_receipt_id INTEGER,
                    CHECK((outcome = 'started' AND completed_at_ms IS NULL AND duration_ms IS NULL AND unsubscribe_receipt_id IS NULL) OR (outcome = 'orderly_shutdown' AND completed_at_ms IS NOT NULL AND duration_ms IS NOT NULL AND completed_at_ms >= started_at_ms AND duration_ms >= 0 AND unsubscribe_receipt_id IS NOT NULL))
                ) STRICT;
                CREATE INDEX IF NOT EXISTS stream_session_receipts_proof
                    ON stream_session_receipts(correlation_id, outcome, id);
                CREATE INDEX IF NOT EXISTS stream_session_receipts_retention
                    ON stream_session_receipts(outcome, completed_at_ms, id);
                PRAGMA user_version = 33;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 33;
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

fn outbound_request_clock_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StoreError::OutboundRequestClock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StoreError::OutboundRequestClockOverflow)
}

fn prune_expired_outbound_request_receipts(
    transaction: &Transaction<'_>,
) -> Result<(), StoreError> {
    let cutoff_ms = outbound_request_clock_ms()?.saturating_sub(OUTBOUND_REQUEST_RETENTION_MS);
    transaction
        .execute(
            "DELETE FROM outbound_request_receipts WHERE outcome <> 'started' AND completed_at_ms < ?1",
            params![cutoff_ms],
        )
        .map_err(StoreError::OutboundRequestReceipt)?;
    Ok(())
}

fn ensure_outbound_request_capacity(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    prune_expired_outbound_request_receipts(transaction)?;
    let count: i64 = transaction
        .query_row("SELECT COUNT(*) FROM outbound_request_receipts", [], |row| row.get(0))
        .map_err(StoreError::OutboundRequestReceipt)?;
    if count >= MAX_OUTBOUND_REQUEST_RECEIPTS {
        return Err(StoreError::OutboundRequestAuditCapacityExhausted);
    }
    Ok(())
}

fn prune_expired_stream_session_receipts(
    transaction: &Transaction<'_>,
) -> Result<(), StoreError> {
    let cutoff_ms = outbound_request_clock_ms()?.saturating_sub(OUTBOUND_REQUEST_RETENTION_MS);
    transaction
        .execute(
            "DELETE FROM stream_session_receipts
             WHERE outcome = 'orderly_shutdown' AND completed_at_ms < ?1",
            params![cutoff_ms],
        )
        .map_err(StoreError::StreamSessionReceipt)?;
    Ok(())
}

fn ensure_stream_session_capacity(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    prune_expired_stream_session_receipts(transaction)?;
    let count: i64 = transaction
        .query_row("SELECT COUNT(*) FROM stream_session_receipts", [], |row| row.get(0))
        .map_err(StoreError::StreamSessionReceipt)?;
    if count >= MAX_OUTBOUND_REQUEST_RECEIPTS {
        return Err(StoreError::StreamSessionAuditCapacityExhausted);
    }
    Ok(())
}

fn invalid_outbound_request_receipt_value(index: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid outbound request receipt",
        )),
    )
}

fn receipt_from_row(row: &rusqlite::Row<'_>) -> Result<OutboundRequestReceipt, rusqlite::Error> {
    let correlation: String = row.get(1)?;
    let correlation_id = Uuid::parse_str(&correlation)
        .map_err(|_| invalid_outbound_request_receipt_value(1))?;
    let transport: String = row.get(6)?;
    let operation: String = row.get(7)?;
    let safety_class: String = row.get(8)?;
    let precondition: String = row.get(9)?;
    let outcome: String = row.get(10)?;
    let http_status = row.get::<_, Option<i64>>(11)?.map(|value| {
        u16::try_from(value).map_err(|_| invalid_outbound_request_receipt_value(11))
    }).transpose()?;
    Ok(OutboundRequestReceipt {
        id: OutboundRequestReceiptId(row.get(0)?),
        correlation_id,
        started_at_ms: row.get(2)?,
        completed_at_ms: row.get(3)?,
        duration_ms: row.get(4)?,
        vehicle_tesla_id: row.get(5)?,
        transport: OutboundRequestTransport::parse(&transport).ok_or_else(|| invalid_outbound_request_receipt_value(6))?,
        operation: OutboundRequestOperation::parse(&operation).ok_or_else(|| invalid_outbound_request_receipt_value(7))?,
        safety_class: OutboundRequestSafetyClass::parse(&safety_class).ok_or_else(|| invalid_outbound_request_receipt_value(8))?,
        precondition: OutboundRequestPrecondition::parse(&precondition).ok_or_else(|| invalid_outbound_request_receipt_value(9))?,
        outcome: if outcome == "started" { None } else { Some(OutboundRequestOutcome::parse(&outcome).ok_or_else(|| invalid_outbound_request_receipt_value(10))?) },
        http_status,
    })
}

fn cleanup_abandoned_import_generations(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute("DELETE FROM import_generations", [])
        .map_err(StoreError::ImportGeneration)?;
    Ok(())
}

fn require_positive_db(value: i64, field: &'static str) -> Result<(), StoreError> {
    if value <= 0 {
        Err(StoreError::InvalidLifecycleCarId)
    } else {
        let _ = field;
        Ok(())
    }
}

fn append_observation_in_transaction(
    transaction: &Transaction<'_>,
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
    let payload_json = String::from_utf8(payload_json).expect("serde_json is UTF-8");
    ensure_vehicle_belongs_to_source(transaction, input.vehicle_id, input.source_id)?;
    let inserted = transaction
        .execute(
            "INSERT INTO raw_observations
             (source_id, vehicle_id, observed_at_ms, received_at_ms, payload_sha256, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
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
        transaction,
        input.source_id,
        input.vehicle_id,
        input.observed_at_ms,
        payload_sha256,
    )?
    .ok_or(StoreError::ObservationMissingAfterInsert)?;
    Ok(AppendObservation {
        observation,
        inserted,
    })
}

fn stream_timestamp_is_newer(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    timestamp_ms: i64,
) -> Result<bool, StoreError> {
    let previous: Option<i64> = transaction
        .query_row(
            "SELECT last_timestamp_ms FROM stream_watermarks WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    Ok(previous.is_none_or(|value| timestamp_ms > value))
}

fn accept_stream_timestamp_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    timestamp_ms: i64,
) -> Result<bool, StoreError> {
    validate_timestamp("stream timestamp", timestamp_ms)?;
    Ok(transaction
        .execute(
            "INSERT INTO stream_watermarks(vehicle_id, last_timestamp_ms)
             VALUES (?1, ?2)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                 last_timestamp_ms = excluded.last_timestamp_ms
             WHERE excluded.last_timestamp_ms > stream_watermarks.last_timestamp_ms",
            params![vehicle_id.to_string(), timestamp_ms],
        )
        .map_err(StoreError::Query)?
        == 1)
}

fn load_lifecycle_state_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<Option<LifecycleStateRecord>, StoreError> {
    transaction
        .query_row(
            "SELECT vehicle_id, car_id, last_observation_id, open_session_json,
                    quarantined, updated_at_ms
             FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| {
                let vehicle_id = row
                    .get::<_, String>(0)?
                    .parse()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
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

fn observations_after_id_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    after_observation_id: i64,
    limit: u32,
) -> Result<Vec<ObservationRecord>, StoreError> {
    if after_observation_id < 0 {
        return Err(StoreError::InvalidLifecycleCursor);
    }
    if !(1..=MAX_OBSERVATION_QUERY_LIMIT).contains(&limit) {
        return Err(StoreError::InvalidObservationQueryLimit {
            actual: limit,
            maximum: MAX_OBSERVATION_QUERY_LIMIT,
        });
    }
    let mut statement = transaction
        .prepare(
            "SELECT observation_id, source_id, vehicle_id, observed_at_ms, received_at_ms,
                    payload_sha256, payload_json
             FROM raw_observations
             WHERE vehicle_id = ?1 AND observation_id > ?2
             ORDER BY observation_id ASC LIMIT ?3",
        )
        .map_err(StoreError::Query)?;
    statement
        .query_map(
            params![vehicle_id.to_string(), after_observation_id, i64::from(limit)],
            observation_from_row,
        )
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)
}

fn restore_lifecycle_open_children_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    state: &mut crate::lifecycle::OpenSessionState,
) -> Result<(), StoreError> {
    let vehicle = vehicle_id.to_string();
    let mut statement = transaction
        .prepare(
            "SELECT domain, parent_source_row_id, row_json
             FROM lifecycle_open_rows WHERE vehicle_id = ?1
             ORDER BY source_row_id",
        )
        .map_err(StoreError::Query)?;
    if let Some(open) = state.open_drive.as_mut() {
        open.positions.clear();
    }
    if let Some(open) = state.open_charge.as_mut() {
        open.samples.clear();
    }
    let rows = statement
        .query_map(params![vehicle], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(StoreError::Query)?;
    for row in rows {
        let (domain, parent_id, json) = row.map_err(StoreError::Query)?;
        match domain.as_str() {
            "position" => {
                let position: crate::hub_pack::ProjectionPosition =
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?;
                if state
                    .open_drive
                    .as_ref()
                    .is_some_and(|open| Some(open.id) == parent_id)
                {
                    state
                        .open_drive
                        .as_mut()
                        .expect("open drive")
                        .positions
                        .push(position);
                }
            }
            "charge_sample" => {
                let sample: crate::hub_pack::ProjectionChargeSample =
                    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?;
                if state
                    .open_charge
                    .as_ref()
                    .is_some_and(|open| Some(open.id) == parent_id)
                {
                    state
                        .open_charge
                        .as_mut()
                        .expect("open charge")
                        .samples
                        .push(sample);
                }
            }
            _ => {}
        }
    }
    if let Some(open) = state.open_drive.as_mut() {
        if let Some(first) = open.positions.first() {
            open.start_latitude = Some(first.latitude);
            open.start_longitude = Some(first.longitude);
            open.start_soc = first.battery_level;
            open.start_rated_range_km = first.rated_battery_range_km;
        }
        open.outside_temp_sum = 0.0;
        open.outside_temp_count = 0;
        open.speed_max = None;
        for position in &open.positions {
            if let Some(value) = position.outside_temp {
                open.outside_temp_sum += value;
                open.outside_temp_count = open.outside_temp_count.saturating_add(1);
            }
            open.speed_max = match (open.speed_max, position.speed) {
                (Some(current), Some(next)) => Some(current.max(next)),
                (None, value) => value,
                (current, None) => current,
            };
        }
    }
    Ok(())
}

fn observation_vehicle_state(payload: &Value) -> String {
    payload
        .get("source_vehicle_state")
        .and_then(Value::as_str)
        .filter(|state| {
            !state.is_empty() && state.len() <= 64 && !state.chars().any(char::is_control)
        })
        .unwrap_or("unknown")
        .to_owned()
}

fn ensure_vehicle_source(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    source_id: Uuid,
) -> Result<(), StoreError> {
    let actual: Option<i64> = transaction
        .query_row(
            "SELECT 1 FROM vehicle_identity_aliases
             WHERE vehicle_id = ?1 AND source_id = ?2",
            params![vehicle_id.to_string(), source_id.to_string()],
            |_| Ok(1),
        )
        .optional()
        .map_err(StoreError::LifecycleWrite)?;
    let Some(actual) = actual else {
        return Err(StoreError::UnknownVehicle(vehicle_id));
    };
    let _ = actual;
    Ok(())
}

fn insert_open_row<T: Serialize>(
    transaction: &Transaction<'_>,
    source_id: &str,
    source_table: &str,
    source_row_id: i64,
    vehicle_id: &str,
    car_id: i64,
    domain: &str,
    parent_source_row_id: Option<i64>,
    row: &T,
) -> Result<usize, StoreError> {
    let row_json = serde_json::to_string(row).map_err(StoreError::SerializeLifecycleRow)?;
    transaction
        .execute(
            "INSERT INTO lifecycle_open_rows(
                source_id, source_table, source_row_id, vehicle_id, car_id,
                domain, parent_source_row_id, row_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(source_id, source_table, source_row_id) DO NOTHING",
            params![
                source_id,
                source_table,
                source_row_id,
                vehicle_id,
                car_id,
                domain,
                parent_source_row_id,
                row_json,
            ],
        )
        .map_err(StoreError::LifecycleWrite)
}

fn mark_export_dirty_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO export_outbox(
                vehicle_id, dirty_revision, attempts, next_attempt_ms,
                claimed_until_ms, last_error
             ) VALUES (?1, 1, 0, 0, 0, NULL)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                dirty_revision = export_outbox.dirty_revision + 1,
                attempts = 0, next_attempt_ms = 0,
                claimed_until_ms = 0, last_error = NULL",
            params![vehicle_id.to_string()],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn record_sync_mutation_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    entity: &str,
    entity_id: i64,
    car_id: i64,
    operation: &str,
    payload_json: &str,
) -> Result<(), StoreError> {
    let next_revision: i64 = transaction
        .query_row(
            "INSERT INTO sync_mutation_sequences(vehicle_id, next_revision)
             VALUES (?1, 2)
             ON CONFLICT(vehicle_id) DO UPDATE SET next_revision = next_revision + 1
             RETURNING next_revision - 1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    transaction
        .execute(
            "INSERT INTO sync_mutations(
                vehicle_id, revision, entity, entity_id, car_id,
                operation, payload_json, published, claimed_until_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0)",
            params![
                vehicle_id.to_string(),
                next_revision,
                entity,
                entity_id,
                car_id,
                operation,
                payload_json,
            ],
        )
        .map_err(StoreError::LineageCatalog)?;
    Ok(())
}

fn parse_sync_entity(value: &str) -> Option<ProjectionDeltaEntity> {
    match value {
        "car" => Some(ProjectionDeltaEntity::Car),
        "car_setting" => Some(ProjectionDeltaEntity::CarSetting),
        "geofence" => Some(ProjectionDeltaEntity::Geofence),
        "address" => Some(ProjectionDeltaEntity::Address),
        "drive" => Some(ProjectionDeltaEntity::Drive),
        "position" => Some(ProjectionDeltaEntity::Position),
        "charge" => Some(ProjectionDeltaEntity::Charge),
        "charge_sample" => Some(ProjectionDeltaEntity::ChargeSample),
        "state" => Some(ProjectionDeltaEntity::State),
        "update" => Some(ProjectionDeltaEntity::Update),
        _ => None,
    }
}

fn load_projection_json<T: DeserializeOwned>(
    connection: &Connection,
    table: &str,
    column: &str,
    id_column: &str,
    mutation: &SyncMutation,
) -> Result<T, StoreError> {
    let sql = format!(
        "SELECT {column} FROM {table} WHERE vehicle_id = ?1 AND {id_column} = ?2"
    );
    let json: Option<String> = connection
        .query_row(
            &sql,
            params![mutation.vehicle_id.to_string(), mutation.entity_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    let json = json.ok_or_else(|| {
        StoreError::SyncMutation(format!(
            "missing materialised {} {}",
            mutation.entity, mutation.entity_id
        ))
    })?;
    serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)
}

fn address_lookup_key(point: crate::location::Wgs84Point) -> String {
    format!("{:.6}:{:.6}", point.latitude, point.longitude)
}

fn advance_terrain_cursor(
    transaction: &Transaction<'_>,
    candidate: &TerrainCandidate,
    attempted_at_ms: i64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO terrain_enrichment_state(
                vehicle_id, cursor_position_id, updated_at_ms
             ) VALUES (?1, ?2, ?3)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                cursor_position_id = MAX(cursor_position_id, excluded.cursor_position_id),
                updated_at_ms = excluded.updated_at_ms",
            params![
                candidate.vehicle_id.to_string(),
                candidate.position.id,
                attempted_at_ms,
            ],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn upsert_terrain_provenance(
    transaction: &Transaction<'_>,
    candidate: &TerrainCandidate,
    tile_name: Option<&str>,
    tile_hash: Option<&str>,
    dataset_source: Option<&str>,
    dataset_version: Option<&str>,
    status: &str,
    error_code: Option<&str>,
    retry_after_ms: i64,
    attempted_at_ms: i64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO terrain_elevation_provenance(
                vehicle_id, position_id, drive_id, latitude, longitude,
                elevation_m, tile_name, tile_hash, dataset_source, dataset_version,
                status, error_code, attempts, attempted_at_ms, retry_after_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                       COALESCE((SELECT attempts FROM terrain_elevation_provenance
                                 WHERE vehicle_id = ?1 AND position_id = ?2), 0) + 1,
                       ?13, ?14)
             ON CONFLICT(vehicle_id, position_id) DO UPDATE SET
                drive_id = excluded.drive_id,
                latitude = excluded.latitude,
                longitude = excluded.longitude,
                elevation_m = excluded.elevation_m,
                tile_name = excluded.tile_name,
                tile_hash = excluded.tile_hash,
                dataset_source = excluded.dataset_source,
                dataset_version = excluded.dataset_version,
                status = excluded.status,
                error_code = excluded.error_code,
                attempts = terrain_elevation_provenance.attempts + 1,
                attempted_at_ms = excluded.attempted_at_ms,
                retry_after_ms = excluded.retry_after_ms",
            params![
                candidate.vehicle_id.to_string(),
                candidate.position.id,
                candidate.position.drive_id,
                candidate.position.latitude,
                candidate.position.longitude,
                candidate.position.elevation,
                tile_name,
                tile_hash,
                dataset_source,
                dataset_version,
                status,
                error_code,
                attempted_at_ms,
                retry_after_ms,
            ],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn recompute_terrain_drive(
    transaction: &Transaction<'_>,
    vehicle_id: &str,
    drive_id: i64,
) -> Result<(), StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT position_json FROM materialised_positions
             WHERE vehicle_id = ?1 AND drive_id = ?2
             ORDER BY position_id ASC",
        )
        .map_err(StoreError::LifecycleWrite)?;
    let rows = statement
        .query_map(params![vehicle_id, drive_id], |row| row.get::<_, String>(0))
        .map_err(StoreError::LifecycleWrite)?;
    let positions: Vec<ProjectionPosition> = rows
        .map(|row| {
            let json = row.map_err(StoreError::LifecycleWrite)?;
            serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)
        })
        .collect::<Result<_, _>>()?;
    let (ascent, descent) = terrain_elevation_totals(&positions);
    let drive_json: String = transaction
        .query_row(
            "SELECT drive_json FROM materialised_drives
             WHERE vehicle_id = ?1 AND drive_id = ?2",
            params![vehicle_id, drive_id],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    let mut drive: ProjectionDrive =
        serde_json::from_str(&drive_json).map_err(StoreError::DeserializeLifecycleRow)?;
    drive.ascent = Some(ascent);
    drive.descent = Some(descent);
    let drive_json = serde_json::to_string(&drive).map_err(StoreError::SerializeLifecycleRow)?;
    transaction
        .execute(
            "UPDATE materialised_drives SET drive_json = ?3, ascent = ?4, descent = ?5
             WHERE vehicle_id = ?1 AND drive_id = ?2",
            params![vehicle_id, drive_id, drive_json, ascent, descent],
        )
        .map_err(StoreError::LifecycleWrite)?;
    Ok(())
}

fn terrain_elevation_totals(positions: &[ProjectionPosition]) -> (i64, i64) {
    let mut previous = None;
    let mut ascent = 0_i64;
    let mut descent = 0_i64;
    for elevation in positions.iter().filter_map(|position| position.elevation) {
        if let Some(previous_elevation) = previous {
            let delta = elevation - previous_elevation;
            if delta > 0 {
                ascent = ascent.saturating_add(delta);
            } else if delta < 0 {
                descent = descent.saturating_add(delta.unsigned_abs() as i64);
            }
        }
        previous = Some(elevation);
    }
    (
        if ascent >= 32_768 { 0 } else { ascent },
        if descent >= 32_768 { 0 } else { descent },
    )
}

fn validate_address_cache_record(record: &AddressCacheRecord) -> Result<(), StoreError> {
    if record.osm_type.is_empty()
        || record.osm_type.len() > 32
        || record.osm_type.chars().any(char::is_control)
        || record.osm_id <= 0
        || record.display_name.trim().is_empty()
        || record.display_name.len() > MAX_DISPLAY_NAME_BYTES
        || record.name.as_deref().is_some_and(|name| {
            name.len() > MAX_DISPLAY_NAME_BYTES || name.chars().any(char::is_control)
        })
        || !record.lookup_latitude.is_finite()
        || !(-90.0..=90.0).contains(&record.lookup_latitude)
        || !record.lookup_longitude.is_finite()
        || !(-180.0..=180.0).contains(&record.lookup_longitude)
        || record.looked_up_at_ms < 0
    {
        return Err(StoreError::InvalidAddressCache);
    }
    Ok(())
}

fn load_geofence_fences(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<Vec<crate::lifecycle::GeofenceFence>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT name, latitude, longitude, radius_m, billing_type,
                    cost_per_unit, session_fee
             FROM geofences WHERE vehicle_id = ?1 ORDER BY source_geofence_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id.to_string()], |row| {
            Ok(crate::lifecycle::GeofenceFence {
                name: row.get(0)?,
                latitude: row.get(1)?,
                longitude: row.get(2)?,
                radius_m: row.get(3)?,
                billing_type: row
                    .get::<_, Option<String>>(4)?
                    .map(|value| match value.as_str() {
                        "per_kwh" => crate::hub_pack::GeofenceBillingType::PerKwh,
                        "per_minute" => crate::hub_pack::GeofenceBillingType::PerMinute,
                        _ => crate::hub_pack::GeofenceBillingType::PerKwh,
                    }),
                cost_per_unit: row.get(5)?,
                session_fee: row.get(6)?,
            })
        })
        .map_err(StoreError::Query)?;
    rows.map(|row| row.map_err(StoreError::Query)).collect()
}

fn enqueue_address_jobs(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    delta: &crate::lifecycle::LifecycleDelta,
) -> Result<(), StoreError> {
    for drive in &delta.drives {
        let endpoints = [
            (
                "start_address",
                drive.start_latitude,
                drive.start_longitude,
                drive.start_address.is_some(),
            ),
            (
                "end_address",
                drive.end_latitude,
                drive.end_longitude,
                drive.end_address.is_some(),
            ),
        ];
        for (field, latitude, longitude, already_labeled) in endpoints {
            if already_labeled {
                continue;
            }
            let (Some(latitude), Some(longitude)) = (latitude, longitude) else {
                continue;
            };
            if latitude.is_finite()
                && longitude.is_finite()
                && (-90.0..=90.0).contains(&latitude)
                && (-180.0..=180.0).contains(&longitude)
            {
                insert_address_job(
                    transaction,
                    vehicle_id,
                    "drive",
                    drive.id,
                    field,
                    latitude,
                    longitude,
                )?;
            }
        }
    }
    for charge in &delta.charges {
        if charge.address.is_some() {
            continue;
        }
        let Some((_, latitude, longitude)) = delta
            .charge_start_coordinates
            .iter()
            .find(|(id, _, _)| *id == charge.id)
        else {
            continue;
        };
        if latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(latitude)
            && (-180.0..=180.0).contains(longitude)
        {
            insert_address_job(
                transaction,
                vehicle_id,
                "charge",
                charge.id,
                "address",
                *latitude,
                *longitude,
            )?;
        }
    }
    Ok(())
}

fn insert_address_job(
    transaction: &rusqlite::Transaction<'_>,
    vehicle_id: Uuid,
    target_type: &str,
    target_id: i64,
    field: &str,
    latitude: f64,
    longitude: f64,
) -> Result<(), StoreError> {
    let job_key = format!("{vehicle_id}:{target_type}:{target_id}:{field}");
    transaction
        .execute(
            "INSERT INTO address_enrichment_jobs(
                job_key, vehicle_id, target_type, target_id, field,
                latitude, longitude, status, attempts, next_attempt_ms,
                lease_until_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', 0, 0, 0)
             ON CONFLICT(vehicle_id, target_type, target_id, field) DO NOTHING",
            params![
                job_key,
                vehicle_id.to_string(),
                target_type,
                target_id,
                field,
                latitude,
                longitude
            ],
        )
        .map_err(StoreError::AddressEnrichmentWrite)?;
    Ok(())
}

fn sync_mqtt_publications_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    revision: i64,
    publications: &[MqttPublication],
    healthy_clear_delivered: bool,
) -> Result<(), StoreError> {
    if !healthy_clear_delivered {
        let Some(first_topic) = publications.first().map(|publication| publication.topic.as_str()) else {
            return Err(StoreError::MqttDelivery(rusqlite::Error::InvalidQuery));
        };
        let Some((base, _)) = first_topic.rsplit_once('/') else {
            return Err(StoreError::MqttDelivery(rusqlite::Error::InvalidQuery));
        };
        let clear = MqttPublication {
            topic: format!("{base}/healthy"),
            payload: String::new(),
            qos: MqttQos::AtLeastOnce,
            retain: true,
        };
        let fingerprint = mqtt_publication_fingerprint(&clear);
        transaction
            .execute(
                "INSERT INTO mqtt_delivery_state(
                    vehicle_id, field, topic, payload, fingerprint, qos, retain,
                    pending_revision, pending, claimed_until_ms, attempts, last_error,
                    delivered_fingerprint, phase
                 ) VALUES (?1, 'healthy', ?2, '', ?3, 1, 1, ?4, 1, 0, 0, NULL, NULL, 1)
                 ON CONFLICT(vehicle_id, field) DO NOTHING",
                params![vehicle_id.to_string(), clear.topic, fingerprint, revision],
            )
            .map_err(StoreError::MqttDelivery)?;
    }

    for publication in publications {
        let Some(field) = publication.topic.rsplit('/').next() else {
            return Err(StoreError::MqttDelivery(rusqlite::Error::InvalidQuery));
        };
        if field == "healthy" && !healthy_clear_delivered {
            continue;
        }
        let fingerprint = mqtt_publication_fingerprint(publication);
        transaction
            .execute(
                "INSERT INTO mqtt_delivery_state(
                    vehicle_id, field, topic, payload, fingerprint, qos, retain,
                    pending_revision, pending, claimed_until_ms, attempts, last_error,
                    delivered_fingerprint, phase
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, 1, 0, 0, NULL, NULL, 0)
                 ON CONFLICT(vehicle_id, field) DO UPDATE SET
                    topic = excluded.topic,
                    payload = excluded.payload,
                    qos = excluded.qos,
                    retain = excluded.retain,
                    pending_revision = excluded.pending_revision,
                    pending = CASE WHEN mqtt_delivery_state.delivered_fingerprint = excluded.fingerprint
                                   THEN 0 ELSE 1 END,
                    claimed_until_ms = CASE WHEN mqtt_delivery_state.fingerprint = excluded.fingerprint
                                            THEN mqtt_delivery_state.claimed_until_ms ELSE 0 END,
                    attempts = CASE WHEN mqtt_delivery_state.fingerprint = excluded.fingerprint
                                    THEN mqtt_delivery_state.attempts ELSE 0 END,
                    last_error = NULL,
                    phase = 0",
                params![
                    vehicle_id.to_string(),
                    field,
                    publication.topic,
                    publication.payload,
                    fingerprint,
                    i64::from(publication.retain),
                    revision,
                ],
            )
            .map_err(StoreError::MqttDelivery)?;
    }
    Ok(())
}

fn mqtt_publication_fingerprint(publication: &MqttPublication) -> String {
    let mut digest = Sha256::new();
    digest.update(publication.topic.as_bytes());
    digest.update([0]);
    digest.update(publication.payload.as_bytes());
    digest.update([0, u8::from(publication.retain)]);
    digest.update([crate::mqtt::MQTT_QOS_AT_LEAST_ONCE]);
    hex::encode(digest.finalize())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_file_hex(path: &Path) -> Result<String, StoreError> {
    let mut file = fs::File::open(path).map_err(StoreError::OpenBackupPack)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(StoreError::ReadBackupPack)?;
        if read == 0 {
            return Ok(hex::encode(digest.finalize()));
        }
        digest.update(&buffer[..read]);
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("cannot create data directory: {0}")]
    CreateDataDir(std::io::Error),
    #[error("cannot create packs directory: {0}")]
    CreatePacksDir(std::io::Error),
    #[error("cannot protect data directory: {0}")]
    ProtectDataDir(std::io::Error),
    #[error("cannot protect packs directory: {0}")]
    ProtectPacksDir(std::io::Error),
    #[error("cannot open hub database: {0}")]
    Open(rusqlite::Error),
    #[error("cannot configure hub database: {0}")]
    Configure(rusqlite::Error),
    #[error("invalid address cache record")]
    InvalidAddressCache,
    #[error("cannot write address cache: {0}")]
    AddressCacheWrite(rusqlite::Error),
    #[error("invalid address enrichment result")]
    InvalidAddressEnrichment,
    #[error("cannot write address enrichment job: {0}")]
    AddressEnrichmentWrite(rusqlite::Error),
    #[error("cannot create Hub SQLite backup: {0}")]
    Backup(rusqlite::Error),
    #[error("Hub SQLite backup destination already exists: {0}")]
    BackupDestinationExists(PathBuf),
    #[error("Hub SQLite backup destination must not be the live catalogue")]
    BackupDestinationIsLiveDatabase,
    #[error("cannot create Hub backup directory: {0}")]
    CreateBackupDirectory(std::io::Error),
    #[error("cannot copy Hub backup pack from {source_path} to {destination}: {source_error}")]
    CopyBackupPack {
        source_path: PathBuf,
        destination: PathBuf,
        source_error: std::io::Error,
    },
    #[error("Hub backup pack {path} is {actual} bytes; expected {expected}")]
    BackupPackSizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("cannot open Hub backup pack: {0}")]
    OpenBackupPack(std::io::Error),
    #[error("cannot read Hub backup pack: {0}")]
    ReadBackupPack(std::io::Error),
    #[error("Hub backup pack digest mismatches its catalogue: {path}")]
    BackupPackDigestMismatch { path: PathBuf },
    #[error("cannot migrate hub database: {0}")]
    Migrate(rusqlite::Error),
    #[error("database query failed: {0}")]
    Query(rusqlite::Error),
    #[error("cannot begin local transaction: {0}")]
    Begin(rusqlite::Error),
    #[error("cannot open Hub publication gate: {0}")]
    OpenPublicationGate(std::io::Error),
    #[error("cannot protect Hub publication gate: {0}")]
    ProtectPublicationGate(std::io::Error),
    #[error("cannot acquire Hub publication gate: {0}")]
    LockPublicationGate(std::io::Error),
    #[error("Hub publication gate is busy")]
    PublicationGateBusy,
    #[error("cannot publish sync manifest: {0}")]
    PublishManifest(rusqlite::Error),
    #[error("cannot associate a snapshot fingerprint with uncatalogued manifest {0}")]
    FingerprintManifestMissing(Uuid),
    #[error("invalid stored vehicle identity")]
    InvalidVehicleId,
    #[error("invalid vehicle identity value")]
    InvalidVehicleIdentity,
    #[error("vehicle identity conflicts across sources")]
    VehicleIdentityConflict,
    #[error("invalid stored source identity")]
    InvalidSourceId,
    #[error("terrain materialised car is missing for {0}")]
    TerrainCarMissing(Uuid),
    #[error("cannot publish terrain projection pack: {0}")]
    TerrainPack(ProjectionPackError),
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
    #[error("cannot access import generation: {0}")]
    ImportGeneration(rusqlite::Error),
    #[error("import generation is invalid")]
    InvalidImportGeneration,
    #[error("import generation was not found or is not staging")]
    ImportGenerationNotFound,
    #[error("import generation promotion became unsettled by newer live state")]
    ImportGenerationConflict,
    #[error("cannot access lineage catalogue: {0}")]
    LineageCatalog(rusqlite::Error),
    #[error("lineage catalogue conflicts with an existing sequence")]
    LineageCatalogConflict,
    #[error("invalid sync mutation: {0}")]
    SyncMutation(String),
    #[error("lineage pack is not verified and ready")]
    LineagePackNotReady,
    #[error("lineage pack digest does not match its content")]
    LineagePackDigestMismatch,
    #[error("cannot serialize raw observation: {0}")]
    SerializeObservation(serde_json::Error),
    #[error("invalid sync manifest: {0}")]
    Manifest(crate::protocol::ProtocolError),
    #[error("sync sequence does not fit SQLite signed integer")]
    SequenceTooLarge,
    #[error("sync sequence is exhausted")]
    SequenceExhausted,
    #[error("stored sync sequence is invalid")]
    InvalidStoredSequence,
    #[error(
        "manifest sequence {attempted} is stale; current sequence is {current} for {vehicle_id}"
    )]
    StaleManifest {
        vehicle_id: Uuid,
        attempted: u64,
        current: u64,
    },
    #[error("pack size does not fit SQLite signed integer")]
    PackSizeTooLarge,
    #[error("stored pack path is not canonical")]
    UnsafeStoredPackPath,
    #[error("cannot inspect catalogue pack {path}: {source}")]
    InspectCatalogPack {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("catalogue pack is not a regular file: {path}")]
    CatalogPackNotRegular { path: PathBuf },
    #[error("catalogue pack {path} is {actual} bytes; expected {expected}")]
    CatalogPackSizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("catalogue pack digest mismatches its record: {path}")]
    CatalogPackDigestMismatch { path: PathBuf },
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
    #[error("stored count is invalid")]
    InvalidStoredCount,
    #[error("{0} lifecycle session(s) require reconstruction")]
    QuarantinedLifecycle(usize),
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
    #[error("cannot project lifecycle: {0}")]
    LifecycleProjection(crate::lifecycle::LifecycleError),
    #[error("injected stream fault at {0}")]
    InjectedStreamFault(&'static str),
    #[error("cannot serialize lifecycle history row: {0}")]
    SerializeLifecycleRow(serde_json::Error),
    #[error("cannot deserialize lifecycle history row: {0}")]
    DeserializeLifecycleRow(serde_json::Error),
    #[error("cannot project MQTT summary: {0}")]
    MqttProjection(MqttProjectError),
    #[error("cannot serialize MQTT summary: {0}")]
    SerializeMqtt(serde_json::Error),
    #[error("cannot deserialize MQTT summary: {0}")]
    DeserializeMqtt(serde_json::Error),
    #[error("cannot access MQTT delivery state: {0}")]
    MqttDelivery(rusqlite::Error),
    #[error("MQTT summary revision is exhausted")]
    MqttRevisionExhausted,
    #[error("cannot access outbound request receipt: {0}")]
    OutboundRequestReceipt(rusqlite::Error),
    #[error("outbound request receipt id must be positive")]
    InvalidOutboundRequestReceiptId,
    #[error("outbound request receipt is missing or already terminal")]
    OutboundRequestReceiptNotStarted,
    #[error("outbound request correlation id must not be nil")]
    NilOutboundRequestCorrelationId,
    #[error("outbound request vehicle id must be positive")]
    InvalidOutboundRequestVehicleId,
    #[error("vehicle_data audit records require conditional_read and stream_power_confirmed")]
    InvalidVehicleDataAuditPrecondition,
    #[error("cannot read the store clock for outbound request auditing: {0}")]
    OutboundRequestClock(std::time::SystemTimeError),
    #[error("outbound request audit clock does not fit epoch milliseconds")]
    OutboundRequestClockOverflow,
    #[error("outbound request HTTP status must be between 100 and 599")]
    InvalidOutboundRequestHttpStatus,
    #[error("outbound request watermark must be non-negative")]
    InvalidOutboundRequestWatermark,
    #[error("outbound request query limit {actual} must be between 1 and {maximum}")]
    InvalidOutboundRequestQueryLimit { actual: u32, maximum: u32 },
    #[error("outbound request audit has no room without deleting an unresolved receipt")]
    OutboundRequestAuditCapacityExhausted,
    #[error("cannot access stream session receipt: {0}")]
    StreamSessionReceipt(rusqlite::Error),
    #[error("stream session receipt id must be positive")]
    InvalidStreamSessionReceiptId,
    #[error("stream session receipt is missing or already terminal")]
    StreamSessionReceiptNotStarted,
    #[error("stream session requires a successful matching unsubscribe receipt")]
    StreamSessionUnsubscribeNotCompleted,
    #[error("stream session audit has no room without deleting an unresolved session")]
    StreamSessionAuditCapacityExhausted,
}

#[derive(Debug, Error)]
pub enum ObservationVerificationError {
    #[error("source car id must be positive")]
    InvalidSourceCarId,
    #[error("watermark must be non-negative")]
    InvalidWatermark,
    #[error("no vehicle mapping for source car")]
    NoVehicleMapping,
    #[error("ambiguous vehicle mapping for source car")]
    AmbiguousVehicleMapping,
    #[error("observation query failed: {0}")]
    Store(#[from] StoreError),
}

impl ObservationVerificationError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSourceCarId => "invalid_source_car_id",
            Self::InvalidWatermark => "invalid_watermark",
            Self::NoVehicleMapping => "no_vehicle_mapping",
            Self::AmbiguousVehicleMapping => "ambiguous_vehicle_mapping",
            Self::Store(_) => "database_error",
        }
    }
}

#[derive(Debug, Error)]
pub enum NoWakeVerificationError {
    #[error("audit watermark must be non-negative")]
    InvalidAuditWatermark,
    #[error("no-wake audit query failed: {0}")]
    Store(#[from] StoreError),
    #[error("observation verification failed: {0}")]
    Observation(#[from] ObservationVerificationError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        CursorClaims, CursorKey, LineageBase, LineageCapability, LineageDelta,
        LineageManifestV2, MirrorTable, OpaqueCursor, PackCompression, PackFormat,
        ProtocolVersion, SchemaVersion, SequenceRange, TransferMode, LINEAGE_PROTOCOL_V2,
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
    fn online_catalogue_backup_restores_through_normal_store_checks() {
        let source_directory = tempfile::tempdir().expect("source directory");
        let store = HubStore::initialize(source_directory.path()).expect("source store");
        let installation_id = store.installation_id().expect("source installation");
        let restore_directory = tempfile::tempdir().expect("restore directory");
        let backup_path = restore_directory.path().join("hub.sqlite");

        store
            .backup_catalogue_to(&backup_path)
            .expect("online backup");
        assert!(backup_path.is_file());
        let restored = HubStore::initialize(restore_directory.path()).expect("restored store");
        restored.quick_check().expect("restored integrity");
        assert_eq!(restored.installation_id().unwrap(), installation_id);
        assert!(matches!(
            store.backup_catalogue_to(&backup_path),
            Err(StoreError::BackupDestinationExists(_))
        ));
    }

    #[test]
    fn complete_backup_copies_catalogue_referenced_pack_set() {
        let source_directory = tempfile::tempdir().expect("source directory");
        let store = HubStore::initialize(source_directory.path()).expect("source store");
        let manifest = test_manifest();
        let pack = &manifest.chunks[0];
        let source_pack = store
            .packs_dir()
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        fs::create_dir_all(source_pack.parent().expect("pack parent")).expect("pack parent");
        fs::write(&source_pack, vec![7_u8; 100]).expect("source pack");
        store.publish_manifest(&manifest).expect("catalogue pack");

        let backup_parent = tempfile::tempdir().expect("backup parent");
        let backup_root = backup_parent.path().join("backup");
        store.backup_to(&backup_root).expect("complete backup");
        let restored = HubStore::initialize(&backup_root).expect("restored store");
        restored.quick_check().expect("restored integrity");
        let restored_pack = restored
            .pack_for_digest(pack.sha256)
            .expect("restored catalogue")
            .expect("restored pack");
        assert_eq!(fs::read(restored_pack.path).unwrap(), vec![7_u8; 100]);
    }

    #[test]
    fn corrupt_referenced_pack_refuses_and_cleans_backup_root() {
        let source_directory = tempfile::tempdir().expect("source directory");
        let store = HubStore::initialize(source_directory.path()).expect("source store");
        let manifest = test_manifest();
        let source_pack = store
            .packs_dir()
            .join("sha256")
            .join(format!("{}.sqlite.zst", manifest.chunks[0].sha256));
        fs::create_dir_all(source_pack.parent().expect("pack parent")).expect("pack parent");
        fs::write(&source_pack, vec![7_u8; 100]).expect("source pack");
        store.publish_manifest(&manifest).expect("catalogue pack");
        fs::write(&source_pack, vec![8_u8; 100]).expect("corrupt pack");

        let backup_parent = tempfile::tempdir().expect("backup parent");
        let backup_root = backup_parent.path().join("corrupt-backup");
        assert!(matches!(
            store.backup_to(&backup_root),
            Err(StoreError::BackupPackDigestMismatch { .. })
        ));
        assert!(!backup_root.exists());
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
                    tesla_eid: None,
                    tesla_vid: None,
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
                    tesla_eid: None,
                    tesla_vid: None,
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
            tesla_eid: None,
            tesla_vid: None,
        };
        let vehicle = store
            .register_vehicle_with_id(&descriptor, 2_000, expected_vehicle_id)
            .expect("vehicle registers");
        assert_eq!(vehicle.vehicle_id, expected_vehicle_id);
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        assert_eq!(
            store
                .reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)
                .expect("first marker"),
            1
        );
        assert_eq!(
            store
                .reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)
                .expect("second marker"),
            2
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
    fn lineage_catalog_requires_verified_packs_and_is_restart_safe() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let base_snapshot_id = Uuid::new_v4();
        let delta_snapshot_id = Uuid::new_v4();
        let make_pack = |snapshot_id: Uuid, sequence: SequenceRange, bytes: &[u8]| {
            let digest = Sha256Digest::of_bytes(bytes);
            TransportPack {
                pack_id: Uuid::new_v4(),
                snapshot_id,
                ordinal: 0,
                schema: SchemaVersion { major: 1, minor: 0 },
                format: PackFormat::SqliteTransport,
                compression: PackCompression::Zstd,
                relative_path: TransportPack::canonical_relative_path(digest),
                sha256: digest,
                compressed_bytes: bytes.len() as u64,
                uncompressed_bytes: 100,
                row_count: 1,
                sequence,
                tables: vec![MirrorTable::Vehicle],
            }
        };
        let base_pack = make_pack(
            base_snapshot_id,
            SequenceRange {
                from_exclusive: 10,
                to_inclusive: 10,
            },
            b"base-pack",
        );
        let delta_pack = make_pack(
            delta_snapshot_id,
            SequenceRange {
                from_exclusive: 10,
                to_inclusive: 11,
            },
            b"delta-pack",
        );
        let base_digest = Sha256Digest::of_bytes(b"base-chain");
        let chain_digest = Sha256Digest::of_bytes(b"delta-chain");
        let cursor = OpaqueCursor::issue(
            &CursorKey::from_bytes([7; 32]),
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: SchemaVersion { major: 1, minor: 0 },
                installation_id: store.installation_id().expect("installation"),
                account_id: Uuid::new_v4(),
                vehicle_id: vehicle.vehicle_id,
                generation: 1,
                sequence: 11,
            },
        )
        .expect("cursor");
        let lineage = LineageManifestV2 {
            protocol: LINEAGE_PROTOCOL_V2,
            capability: LineageCapability::ImmutableBaseOrderedDeltas,
            schema: SchemaVersion { major: 1, minor: 0 },
            installation_id: store.installation_id().expect("installation"),
            account_id: Uuid::new_v4(),
            vehicle_id: vehicle.vehicle_id,
            generation: 1,
            base: LineageBase {
                snapshot_id: base_snapshot_id,
                sequence: 10,
                digest: base_digest,
                packs: vec![base_pack.clone()],
            },
            deltas: vec![LineageDelta {
                from_sequence: 10,
                to_sequence: 11,
                parent_chain_digest: base_digest,
                chain_digest,
                pack_digest: delta_pack.sha256,
                pack: delta_pack.clone(),
            }],
            head_sequence: 11,
            head_digest: chain_digest,
            terminal_cursor: cursor,
        };
        assert!(matches!(
            store.commit_lineage_catalog(&lineage),
            Err(StoreError::LineagePackNotReady)
        ));
        let pack_dir = store.packs_dir().join("sha256");
        fs::create_dir_all(&pack_dir).expect("pack directory");
        for pack in [&base_pack, &delta_pack] {
            fs::write(
                pack_dir.join(format!("{}.sqlite.zst", pack.sha256)),
                if pack.pack_id == base_pack.pack_id {
                    b"base-pack".as_slice()
                } else {
                    b"delta-pack".as_slice()
                },
            )
            .expect("pack");
        }
        store
            .commit_lineage_catalog(&lineage)
            .expect("catalog commit");
        store
            .commit_lineage_catalog(&lineage)
            .expect("same commit is idempotent");
        let reopened = HubStore::initialize(temp.path()).expect("reopen");
        let count: i64 = reopened
            .open()
            .expect("open")
            .query_row("SELECT COUNT(*) FROM sync_deltas", [], |row| row.get(0))
            .expect("delta count");
        assert_eq!(count, 1);

        let mut conflict = lineage.clone();
        conflict.deltas[0].chain_digest = Sha256Digest::of_bytes(b"conflict-chain");
        conflict.head_digest = conflict.deltas[0].chain_digest;
        assert!(matches!(
            reopened.commit_lineage_catalog(&conflict),
            Err(StoreError::LineageCatalogConflict)
        ));
    }

    #[test]
    fn import_generation_staging_survives_active_state_and_promotes_once() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (source, vehicle) = test_registered_vehicle(&store);
        let active = crate::teslamate_projection::TeslaMateOpenSession {
            car_id: 10,
            state: Some(crate::teslamate_projection::TeslaMateState {
                id: 1,
                car_id: 10,
                state: "online".into(),
                start_date_ms: 1_000,
                end_date_ms: None,
            }),
            ..Default::default()
        };
        store
            .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 10, &active, 1_000)
            .expect("active seed");

        let run = store
            .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 2_000)
            .expect("generation");
        store
            .stage_import_generation_session(run, &active)
            .expect("stage");
        let staged_count: i64 = store
            .open()
            .expect("open")
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| row.get(0))
            .expect("staged count");
        assert_eq!(staged_count, 1);
        assert_eq!(
            store
                .load_imported_open_session(source.source_id, vehicle.vehicle_id)
                .expect("active load"),
            Some(active.clone())
        );

        let reopened = HubStore::initialize(temp.path()).expect("restart cleanup");
        let cleaned_count: i64 = reopened
            .open()
            .expect("open after restart")
            .query_row("SELECT COUNT(*) FROM import_generations", [], |row| row.get(0))
            .expect("cleaned count");
        assert_eq!(cleaned_count, 0);
        assert_eq!(
            reopened
                .load_imported_open_session(source.source_id, vehicle.vehicle_id)
                .expect("active survives restart"),
            Some(active.clone())
        );

        let successful = reopened
            .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 3_000)
            .expect("second generation");
        let mut promoted = active.clone();
        promoted.watermarks.positions.max_id = Some(12);
        reopened
            .stage_import_generation_session(successful, &promoted)
            .expect("stage second generation");
        reopened
            .promote_import_generation(
                successful,
                source.source_id,
                vehicle.vehicle_id,
                10,
                3_000,
            )
            .expect("promote generation");
        assert_eq!(
            reopened
                .load_imported_open_session(source.source_id, vehicle.vehicle_id)
                .expect("promoted load"),
            Some(promoted)
        );
    }

    #[test]
    fn import_generation_promotion_rejects_newer_live_cursor_without_reopening_state() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        let source = store
            .register_source(
                &SourceDescriptor::new("test", "race"),
                1_000,
            )
            .expect("source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "race-car"),
                1_000,
            )
            .expect("vehicle");
        let active = crate::teslamate_projection::TeslaMateOpenSession {
            car_id: 10,
            state: Some(crate::teslamate_projection::TeslaMateState {
                id: 1,
                car_id: 10,
                state: "online".into(),
                start_date_ms: 1_000,
                end_date_ms: None,
            }),
            ..Default::default()
        };
        store
            .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 10, &active, 1_000)
            .expect("active seed");
        let run = store
            .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 2_000)
            .expect("generation");
        store
            .stage_import_generation_session(run, &active)
            .expect("stage");
        store
            .open()
            .expect("open")
            .execute(
                "UPDATE vehicle_lifecycle_state
                 SET last_observation_id = 9, updated_at_ms = 9_000
                 WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
            )
            .expect("simulate live close");
        let error = store
            .promote_import_generation(
                run,
                source.source_id,
                vehicle.vehicle_id,
                10,
                2_000,
            )
            .expect_err("newer live cursor must settle import");
        assert!(matches!(error, StoreError::ImportGenerationConflict));
        let state = store
            .load_lifecycle_state(vehicle.vehicle_id)
            .expect("state")
            .expect("live state remains");
        assert_eq!(state.last_observation_id, 9);
        assert_eq!(state.updated_at_ms, 9_000);
    }

    #[test]
    fn export_outbox_coalesces_retries_survives_restart_and_respects_v2_base() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("test", "outbox"), 1_000)
            .expect("source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "outbox-car"),
                1_000,
            )
            .expect("vehicle");
        let session = crate::teslamate_projection::TeslaMateOpenSession {
            car_id: 10,
            ..Default::default()
        };
        store
            .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 10, &session, 1_000)
            .expect("dirty seed");
        let claim = store
            .claim_export_outbox(1_000)
            .expect("claim")
            .expect("outbox row");
        store
            .fail_export_outbox(&claim, "https://secret.invalid/token", 1_000)
            .expect("retry");
        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("restart");
        let error: String = reopened
            .open()
            .expect("database")
            .query_row(
                "SELECT last_error FROM export_outbox WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("error");
        assert_eq!(error, "publication_failed");
        let second = reopened
            .claim_export_outbox(4_000)
            .expect("retry claim")
            .expect("retry row");
        assert!(second.attempts >= 2);
        reopened
            .complete_export_outbox(&second)
            .expect("complete");

        let base_id = Uuid::new_v4();
        reopened
            .open()
            .expect("database")
            .execute(
                "INSERT INTO sync_bases(vehicle_id, snapshot_id, base_sequence, base_digest, packs_json)
                 VALUES (?1, ?2, 1, ?3, ?4)",
                params![
                    vehicle.vehicle_id.to_string(),
                    base_id.to_string(),
                    "0".repeat(64),
                    b"[]".as_slice()
                ],
            )
            .expect("base");
        assert!(reopened.vehicle_has_v2_base(vehicle.vehicle_id).expect("base check"));
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
        let digest = Sha256Digest::of_bytes(&[7_u8; 100]);
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

    #[test]
    fn imported_home_work_geofences_match_live_endpoints_after_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let imported = vec![
            crate::teslamate_projection::TeslaMateGeofence {
                id: 10,
                name: "Home".into(),
                latitude: Some(51.0000),
                longitude: Some(-0.1000),
                radius_m: Some(150.0),
                billing_type: Some(crate::hub_pack::GeofenceBillingType::PerKwh),
                cost_per_unit: Some(0.30),
                session_fee: Some(2.0),
            },
            crate::teslamate_projection::TeslaMateGeofence {
                id: 11,
                name: "Work".into(),
                latitude: Some(51.0010),
                longitude: Some(-0.1010),
                radius_m: Some(150.0),
                billing_type: Some(crate::hub_pack::GeofenceBillingType::PerMinute),
                cost_per_unit: Some(0.10),
                session_fee: Some(1.0),
            },
        ];
        assert_eq!(
            store
                .upsert_geofences(vehicle.vehicle_id, &imported)
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .upsert_geofences(vehicle.vehicle_id, &imported)
                .unwrap(),
            0
        );

        let session = crate::lifecycle::OpenSessionState::new();
        let encoded = session.encode().expect("encode session");
        let drive = crate::hub_pack::ProjectionDrive {
            id: 1,
            car_id: 1,
            optimized_at_ms: None,
            start_date_ms: 1_000,
            end_date_ms: 2_000,
            distance_km: Some(1.0),
            duration_min: Some(1),
            efficiency: None,
            outside_temp_avg: None,
            inside_temp_avg: None,
            speed_max: Some(20),
            power_max: None,
            power_min: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_address: None,
            end_address: None,
            start_geofence: None,
            end_geofence: None,
            start_latitude: Some(51.0001),
            start_longitude: Some(-0.1001),
            end_latitude: Some(51.0011),
            end_longitude: Some(-0.1011),
            start_soc: Some(80),
            end_soc: Some(79),
            start_rated_range_km: None,
            end_rated_range_km: None,
            ascent: None,
            descent: None,
        };
        let charge = crate::hub_pack::ProjectionCharge {
            id: 2,
            car_id: 1,
            start_date_ms: 3_000,
            end_date_ms: Some(4_000),
            charge_energy_added: Some(1.0),
            charge_energy_used_kwh: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            cost: None,
            fast_charger_type: None,
            billing_type: None,
            cost_per_unit: None,
            session_fee: None,
            start_latitude: None,
            start_longitude: None,
            start_battery_level: Some(50),
            end_battery_level: Some(51),
            duration_min: Some(1),
            address: None,
            location_name: None,
            geofence: None,
            is_dc: Some(false),
            charge_rate_km_per_hour: None,
            max_charger_power_kw: Some(7.0),
            outside_temp_avg: None,
            start_rated_range_km: None,
            end_rated_range_km: None,
        };
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 1,
                quarantined: false,
                updated_at_ms: 4_000,
                delta: &crate::lifecycle::LifecycleDelta {
                    drives: vec![drive],
                    charges: vec![charge],
                    charge_start_coordinates: vec![(2, 51.0001, -0.1001)],
                    ..Default::default()
                },
            })
            .expect("live endpoint materialisation");

        let reopened = HubStore::initialize(temp.path()).expect("restart store");
        let connection = reopened.open().expect("open queue");
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM address_enrichment_jobs WHERE vehicle_id = ?1",
                    params![vehicle.vehicle_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            3
        );
        drop(connection);
        let first_job = reopened
            .claim_address_enrichment_job(5_000)
            .unwrap()
            .expect("pending start job");
        assert_eq!(first_job.target_type, "charge");
        assert_eq!(first_job.field, "address");
        reopened
            .complete_address_enrichment(&first_job, Some("Delayed response address"), 6_000)
            .unwrap();
        let retry_job = reopened
            .claim_address_enrichment_job(5_000)
            .unwrap()
            .expect("pending end job");
        reopened
            .retry_address_enrichment(&retry_job, "temporary transport", 5_000)
            .unwrap();
        let remaining_job = reopened
            .claim_address_enrichment_job(5_000)
            .unwrap()
            .expect("remaining endpoint job");
        reopened
            .complete_address_enrichment(&remaining_job, None, 6_000)
            .unwrap();
        drop(reopened);
        let resumed = HubStore::initialize(temp.path()).expect("resume store");
        assert!(
            resumed
                .claim_address_enrichment_job(14_999)
                .unwrap()
                .is_none()
        );
        assert!(
            resumed
                .claim_address_enrichment_job(15_000)
                .unwrap()
                .is_some()
        );
        let history = resumed
            .materialised_history(vehicle.vehicle_id)
            .expect("history");
        assert_eq!(
            history.charges[0].address.as_deref(),
            Some("Delayed response address")
        );
        let stored_charge = resumed
            .open()
            .unwrap()
            .query_row(
                "SELECT charge_json FROM materialised_charges WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert!(!stored_charge.contains("osm_type"));
        assert_eq!(history.drives[0].start_geofence.as_deref(), Some("Home"));
        assert_eq!(history.drives[0].end_geofence.as_deref(), Some("Work"));
        assert_eq!(history.charges[0].geofence.as_deref(), Some("Home"));
        assert_eq!(
            history.charges[0].billing_type,
            Some(crate::hub_pack::GeofenceBillingType::PerKwh)
        );
        assert_eq!(history.charges[0].cost_per_unit, Some(0.30));
        assert_eq!(history.charges[0].session_fee, Some(2.0));
        assert_eq!(history.charges[0].cost, Some(2.3));
    }

    #[test]
    fn lifecycle_state_intervals_upsert_and_survive_store_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let state = crate::lifecycle::OpenSessionState::new();
        let encoded = state.encode().expect("encode session");
        let first = crate::hub_pack::ProjectionState {
            id: 1,
            car_id: 1,
            state: "online".into(),
            start_date_ms: 1_000,
            end_date_ms: None,
        };
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 1,
                quarantined: false,
                updated_at_ms: 1_000,
                delta: &crate::lifecycle::LifecycleDelta {
                    states: vec![first.clone()],
                    ..Default::default()
                },
            })
            .expect("write open state");

        let closed = crate::hub_pack::ProjectionState {
            end_date_ms: Some(2_000),
            ..first
        };
        let next = crate::hub_pack::ProjectionState {
            id: 2,
            car_id: 1,
            state: "asleep".into(),
            start_date_ms: 2_000,
            end_date_ms: None,
        };
        let update = crate::hub_pack::ProjectionUpdate {
            id: 1,
            car_id: 1,
            start_date_ms: 1_500,
            end_date_ms: 2_500,
            version: "2026.2".into(),
        };
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 2,
                quarantined: false,
                updated_at_ms: 2_000,
                delta: &crate::lifecycle::LifecycleDelta {
                    states: vec![closed, next],
                    updates: vec![update],
                    ..Default::default()
                },
            })
            .expect("close and open state");

        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("restart store");
        let history = reopened
            .materialised_history(vehicle.vehicle_id)
            .expect("state history");
        assert_eq!(history.states.len(), 2);
        assert_eq!(history.states[0].state, "online");
        assert_eq!(history.states[0].end_date_ms, Some(2_000));
        assert_eq!(history.states[1].state, "asleep");
        assert_eq!(history.states[1].end_date_ms, None);
        assert_eq!(history.updates.len(), 1);
        assert_eq!(history.updates[0].version, "2026.2");
    }

    #[test]
    fn lifecycle_car_metadata_is_durable_and_preserves_imported_efficiency() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let imported = crate::hub_pack::ProjectionCar {
            id: 1,
            name: "Imported car".into(),
            model: "3".into(),
            vin: Some("5YJIMPORTED123456".into()),
            source_eid: Some(88),
            source_vid: Some(99),
            trim_badging: Some("74D".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            firmware_version: Some("2026.0".into()),
            efficiency_wh_per_km: Some(145.0),
            settings: Default::default(),
        };
        store
            .open()
            .expect("open")
            .execute(
                "INSERT INTO materialised_cars(vehicle_id, car_id, car_json) VALUES (?1, ?2, ?3)",
                params![
                    vehicle.vehicle_id.to_string(),
                    imported.id,
                    serde_json::to_string(&imported).expect("serialize imported car")
                ],
            )
            .expect("seed imported car");

        let mut state = crate::lifecycle::OpenSessionState::new();
        state.last_observation_id = 1;
        state.car_metadata = Some(crate::hub_pack::ProjectionCarPatch {
            name: Some("Road car".into()),
            model: Some("3".into()),
            vin: Some("5YJNEWVIN1234567".into()),
            trim_badging: Some("74D".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            firmware_version: Some("2026.1".into()),
        });
        let encoded = state.encode().expect("encode metadata state");
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 1,
                quarantined: false,
                updated_at_ms: 2_000,
                delta: &crate::lifecycle::LifecycleDelta::default(),
            })
            .expect("commit metadata");

        let history = store
            .materialised_history(vehicle.vehicle_id)
            .expect("load metadata");
        let car = history.car.expect("materialised car");
        assert_eq!(car.name, "Road car");
        assert_eq!(car.model, "3");
        assert_eq!(car.vin.as_deref(), Some("5YJNEWVIN1234567"));
        assert_eq!(car.trim_badging.as_deref(), Some("74D"));
        assert_eq!(car.marketing_name.as_deref(), Some("LR AWD"));
        assert_eq!(car.exterior_color.as_deref(), Some("Pearl White"));
        assert_eq!(car.wheel_type.as_deref(), Some("Apollo"));
        assert_eq!(car.spoiler_type.as_deref(), Some("None"));
        assert_eq!(car.firmware_version.as_deref(), Some("2026.1"));
        assert_eq!(car.efficiency_wh_per_km, Some(145.0));

        state.last_observation_id = 2;
        state.car_metadata = Some(crate::hub_pack::ProjectionCarPatch {
            firmware_version: Some("2026.2".into()),
            ..Default::default()
        });
        let encoded = state.encode().expect("encode partial metadata state");
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 2,
                quarantined: false,
                updated_at_ms: 3_000,
                delta: &crate::lifecycle::LifecycleDelta::default(),
            })
            .expect("commit partial metadata");
        let car = store
            .materialised_history(vehicle.vehicle_id)
            .expect("reload metadata")
            .car
            .expect("materialised car after partial update");
        assert_eq!(car.name, "Road car");
        assert_eq!(car.vin.as_deref(), Some("5YJNEWVIN1234567"));
        assert_eq!(car.firmware_version.as_deref(), Some("2026.2"));
        assert_eq!(car.efficiency_wh_per_km, Some(145.0));
    }

    #[test]
    fn repair_preserves_quarantined_sessions_and_removes_orphaned_packs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);

        let connection = store.open().expect("open");
        connection
            .execute(
                "INSERT INTO vehicle_lifecycle_state(
                    vehicle_id, car_id, last_observation_id, open_session_json, quarantined, updated_at_ms
                 ) VALUES (?1, 1, 1, x'7b7d', 1, 1000)",
                params![vehicle.vehicle_id.to_string()],
            )
            .expect("insert quarantined");
        drop(connection);

        let orphaned_pack = store
            .packs_dir()
            .join("0000000000000000000000000000000000000000000000000000000000000000.sqlite.zst");
        std::fs::write(&orphaned_pack, b"orphaned bytes").expect("write pack");

        let report = store.repair().expect("repair");
        assert_eq!(report.status, "ok");
        assert_eq!(report.sqlite_integrity, "ok");
        assert!(matches!(
            store.readiness_check(),
            Err(StoreError::QuarantinedLifecycle(1))
        ));
        assert_eq!(report.quarantined_sessions_preserved, 1);
        assert_eq!(report.orphaned_packs_removed, 1);
        assert_eq!(report.freed_bytes, 14);
        assert!(!orphaned_pack.exists());

        let connection = store.open().expect("open");
        let quarantined: i64 = connection
            .query_row(
                "SELECT quarantined FROM vehicle_lifecycle_state WHERE vehicle_id = ?1",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("query quarantined");
        assert_eq!(quarantined, 1);
    }

    #[test]
    fn car_settings_are_idempotent_and_survive_reopen() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let settings = ProjectionCarSettings {
            enabled: false,
            use_streaming_api: false,
            suspend_after_idle_min: 4,
            suspend_min: 9,
            suspend_min_resolved: true,
            req_not_unlocked: true,
            free_supercharging: true,
            lfp_battery: true,
        };
        store
            .upsert_car_settings(vehicle.vehicle_id, 1, &settings)
            .expect("first settings write");
        store
            .upsert_car_settings(vehicle.vehicle_id, 1, &settings)
            .expect("idempotent settings write");
        assert_eq!(
            store.load_car_settings(vehicle.vehicle_id).unwrap(),
            settings
        );
        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("reopen");
        assert_eq!(
            reopened.load_car_settings(vehicle.vehicle_id).unwrap(),
            settings
        );
    }

    #[test]
    fn unresolved_live_default_resolves_once_and_explicit_value_wins() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let live = ProjectionCarSettings::new_live();
        store
            .upsert_car_settings(vehicle.vehicle_id, 1, &live)
            .expect("live settings");
        assert!(store
            .resolve_car_suspend_min(vehicle.vehicle_id, Some("3"), Some("74D"), None)
            .expect("resolve model 3"));
        let resolved = store.load_car_settings(vehicle.vehicle_id).unwrap();
        assert_eq!(resolved.suspend_min, 12);
        assert!(resolved.suspend_min_resolved);
        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("restart");
        assert!(!reopened
            .resolve_car_suspend_min(vehicle.vehicle_id, Some("Y"), None, None)
            .expect("metadata must not rewrite"));
        assert_eq!(reopened.load_car_settings(vehicle.vehicle_id).unwrap().suspend_min, 12);

        let explicit_source = reopened
            .register_source(&SourceDescriptor::new("tesla_owner_api", "explicit-test"), 2_000)
            .expect("explicit source");
        let explicit_vehicle = reopened
            .register_vehicle(
                &VehicleDescriptor::new(explicit_source.source_id, "explicit-vehicle"),
                2_001,
            )
            .expect("explicit vehicle");
        let explicit = ProjectionCarSettings {
            suspend_min: 7,
            suspend_min_resolved: true,
            ..ProjectionCarSettings::default()
        };
        reopened
            .upsert_car_settings(explicit_vehicle.vehicle_id, 1, &explicit)
            .expect("explicit settings");
        assert_eq!(reopened.load_car_settings(explicit_vehicle.vehicle_id).unwrap().suspend_min, 7);
        assert!(!reopened
            .resolve_car_suspend_min(explicit_vehicle.vehicle_id, Some("3"), None, None)
            .expect("explicit value must stay authoritative"));
    }

    #[test]
    fn stream_watermark_is_strictly_increasing_and_survives_reopen() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);

        assert!(
            store
                .accept_stream_timestamp(vehicle.vehicle_id, 1_000)
                .expect("first watermark")
        );
        assert!(
            !store
                .accept_stream_timestamp(vehicle.vehicle_id, 1_000)
                .expect("duplicate watermark")
        );
        assert!(
            !store
                .accept_stream_timestamp(vehicle.vehicle_id, 999)
                .expect("older watermark")
        );
        assert!(
            store
                .accept_stream_timestamp(vehicle.vehicle_id, 1_001)
                .expect("newer watermark")
        );

        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("reopen");
        assert!(
            !reopened
                .accept_stream_timestamp(vehicle.vehicle_id, 1_000)
                .expect("old frame after restart")
        );
        assert!(
            reopened
                .accept_stream_timestamp(vehicle.vehicle_id, 1_002)
                .expect("new frame after restart")
        );
    }

    #[test]
    fn sync_mutations_are_durable_monotonic_and_coalescible() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let (_, vehicle) = test_registered_vehicle(&store);
        let car = crate::hub_pack::ProjectionCar {
            id: 1,
            name: "Test car".into(),
            model: "3".into(),
            vin: None,
            source_eid: None,
            source_vid: None,
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            firmware_version: None,
            efficiency_wh_per_km: None,
            settings: ProjectionCarSettings::default(),
        };
        store
            .persist_materialised_car_if_absent(vehicle.vehicle_id, &car)
            .expect("car");
        store
            .upsert_car_settings(vehicle.vehicle_id, 1, &ProjectionCarSettings::default())
            .expect("settings one");
        store
            .upsert_car_settings(
                vehicle.vehicle_id,
                1,
                &ProjectionCarSettings {
                    enabled: false,
                    ..ProjectionCarSettings::default()
                },
            )
            .expect("settings two");

        let connection = store.open().expect("open");
        let revisions: Vec<i64> = connection
            .prepare(
                "SELECT revision FROM sync_mutations
                 WHERE vehicle_id = ?1 ORDER BY revision",
            )
            .expect("journal query")
            .query_map(params![vehicle.vehicle_id.to_string()], |row| row.get(0))
            .expect("journal rows")
            .map(|row| row.expect("revision"))
            .collect();
        assert_eq!(revisions, vec![1, 2, 3]);
        drop(connection);

        let claim = store
            .claim_sync_mutations(vehicle.vehicle_id, 2_000, 100)
            .expect("claim")
            .expect("pending mutations");
        assert_eq!((claim.from_revision, claim.to_revision), (1, 3));
        let delta = store
            .projection_delta_for_mutations(
                &claim,
                store.v2_projection_binding(vehicle.vehicle_id).expect("binding"),
                SequenceRange {
                    from_exclusive: 0,
                    to_inclusive: 3,
                },
                Sha256Digest::of_bytes(b"parent"),
            )
            .expect("typed delta");
        assert_eq!(delta.cars.len(), 1);
        assert_eq!(delta.car_settings.len(), 0);
        assert_eq!(delta.cars.len() + delta.car_settings.len(), 1);
        store.release_sync_mutations(&claim).expect("release");
    }
}
#[cfg(test)]
mod terrain_background_tests {
    use super::*;
    use crate::{
        hub_pack::{ProjectionDrive, ProjectionPosition},
        lifecycle::{LifecycleDelta, OpenSessionState},
        protocol::CursorKey,
    };

    fn position(id: i64, elevation: Option<i64>) -> ProjectionPosition {
        ProjectionPosition {
            id,
            drive_id: Some(7),
            car_id: 7,
            date_ms: id * 1_000,
            latitude: 51.0,
            longitude: -0.1,
            speed: Some(20),
            power: None,
            battery_level: Some(80),
            usable_battery_level: None,
            elevation,
            odometer: None,
            ideal_battery_range_km: None,
            est_battery_range_km: None,
            rated_battery_range_km: None,
            fan_status: None,
            driver_temp_setting: None,
            passenger_temp_setting: None,
            is_climate_on: None,
            is_rear_defroster_on: None,
            is_front_defroster_on: None,
            inside_temp: None,
            outside_temp: None,
            battery_heater: None,
            battery_heater_on: None,
            battery_heater_no_power: None,
            tpms_pressure_fl: None,
            tpms_pressure_fr: None,
            tpms_pressure_rl: None,
            tpms_pressure_rr: None,
        }
    }

    #[test]
    fn terrain_enrichment_is_restart_safe_authoritative_and_republishes_revision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("terrain_test", "one"), 1_000)
            .expect("source");
        let mut descriptor = VehicleDescriptor::new(source.source_id, "7");
        descriptor.display_name = Some("Terrain car".into());
        let vehicle = store.register_vehicle(&descriptor, 1_000).expect("vehicle");
        let drive = ProjectionDrive {
            id: 7,
            car_id: 7,
            optimized_at_ms: None,
            start_date_ms: 1_000,
            end_date_ms: 3_000,
            distance_km: Some(1.0),
            duration_min: Some(1),
            efficiency: None,
            outside_temp_avg: None,
            inside_temp_avg: None,
            speed_max: Some(20),
            power_max: None,
            power_min: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_address: None,
            end_address: None,
            start_geofence: None,
            end_geofence: None,
            start_latitude: Some(51.0),
            start_longitude: Some(-0.1),
            end_latitude: Some(51.0),
            end_longitude: Some(-0.1),
            start_soc: Some(80),
            end_soc: Some(79),
            start_rated_range_km: None,
            end_rated_range_km: None,
            ascent: None,
            descent: None,
        };
        let mut open = OpenSessionState::new();
        open.car_metadata = Some(crate::hub_pack::ProjectionCarPatch {
            name: Some("Terrain car".into()),
            model: Some("3".into()),
            ..Default::default()
        });
        let encoded = open.encode().expect("open state");
        store
            .commit_lifecycle_delta(&LifecycleCommit {
                vehicle_id: vehicle.vehicle_id,
                car_id: 7,
                open_session_json: &encoded,
                last_observation_id: 3,
                quarantined: false,
                updated_at_ms: 3_000,
                delta: &LifecycleDelta {
                    drives: vec![drive],
                    positions: vec![position(1, None), position(2, None), position(3, None)],
                    ..Default::default()
                },
            })
            .expect("lifecycle commit");

        let candidates = store.terrain_candidates(4_000, 1_000).expect("candidates");
        assert_eq!(candidates.len(), 3);
        for (candidate, elevation) in candidates.into_iter().zip([100_i16, 110, 90]) {
            assert!(store
                .apply_terrain_result(
                    &candidate,
                    Some(elevation),
                    "N51W001",
                    "aabb",
                    "cache",
                    "srtm-0.8.0-hgt",
                    4_000,
                )
                .expect("terrain result"));
        }
        let history = store.materialised_history(vehicle.vehicle_id).expect("history");
        assert_eq!(history.drives[0].ascent, Some(10));
        assert_eq!(history.drives[0].descent, Some(20));
        assert_eq!(history.positions.iter().map(|p| p.elevation).collect::<Vec<_>>(),
            vec![Some(100), Some(110), Some(90)]);
        assert!(store.terrain_candidates(4_000, 1_000).expect("drained").is_empty());

        let authoritative = TerrainCandidate {
            vehicle_id: vehicle.vehicle_id,
            position: position(1, Some(999)),
        };
        assert!(!store
            .apply_terrain_result(
                &authoritative,
                Some(1),
                "N51W001",
                "different",
                "cache",
                "srtm-0.8.0-hgt",
                5_000,
            )
            .expect("authoritative result"));
        assert_eq!(
            store
                .materialised_history(vehicle.vehicle_id)
                .expect("authoritative history")
                .positions[0]
                .elevation,
            Some(100)
        );

        assert!(store
            .publish_terrain_revision(vehicle.vehicle_id, &CursorKey::from_bytes([9; 32]), 1)
            .expect("publish terrain revision"));
        assert!(!store
            .publish_terrain_revision(vehicle.vehicle_id, &CursorKey::from_bytes([9; 32]), 1)
            .expect("idempotent publish"));
        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("restart");
        let connection = reopened.open().expect("open after restart");
        let provenance: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE vehicle_id = ?1 AND status = 'success'",
                params![vehicle.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("provenance");
        assert_eq!(provenance, 3);
    }

    #[test]
    fn tesla_eid_unifies_sources_and_survives_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let imported = store.register_source(&SourceDescriptor::new("teslamate", "copy"), 1).unwrap();
        let live = store.register_source(&SourceDescriptor::new("owner_api_compat", "local_installation_v1"), 2).unwrap();
        let first = store.register_vehicle(
            &VehicleDescriptor::new(imported.source_id, "eid:700").with_tesla_identity(Some(700), Some(900)), 1).unwrap();
        let second = store.register_vehicle(
            &VehicleDescriptor::new(live.source_id, "700").with_tesla_identity(Some(700), None), 2).unwrap();
        assert_eq!(first.vehicle_id, second.vehicle_id);
        drop(store);
        let reopened = HubStore::initialize(temp.path()).expect("reopen");
        let third = reopened.register_vehicle(
            &VehicleDescriptor::new(live.source_id, "700").with_tesla_identity(Some(700), None), 3).unwrap();
        assert_eq!(first.vehicle_id, third.vehicle_id);
    }

    #[test]
    fn distinct_eid_cars_do_not_merge_on_reused_vid_and_conflicts_fail() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = HubStore::initialize(temp.path()).expect("store");
        let source = store.register_source(&SourceDescriptor::new("teslamate", "copy"), 1).unwrap();
        let one = store.register_vehicle(
            &VehicleDescriptor {
                source_id: source.source_id,
                source_vehicle_key: "eid:701".into(),
                vin: Some("VIN-701".into()),
                display_name: None,
                tesla_eid: Some(701),
                tesla_vid: Some(901),
            }, 1).unwrap();
        let two = store.register_vehicle(
            &VehicleDescriptor::new(source.source_id, "eid:702")
                .with_tesla_identity(Some(702), Some(901)), 2).unwrap();
        assert_ne!(one.vehicle_id, two.vehicle_id);
        let vin_conflict = store.register_vehicle(
            &VehicleDescriptor {
                source_id: source.source_id,
                source_vehicle_key: "eid:703".into(),
                vin: Some("VIN-OTHER".into()),
                display_name: None,
                tesla_eid: Some(701),
                tesla_vid: None,
            }, 4);
        assert!(matches!(vin_conflict, Err(StoreError::VehicleIdentityConflict)));
    }
}

#[cfg(test)]
mod observation_verification_tests {
    use rusqlite::params;

    use super::*;

    fn map_car(store: &HubStore, vehicle_id: Uuid, car_id: i64) {
        store
            .open()
            .expect("open mapping database")
            .execute(
                "INSERT INTO materialised_cars(vehicle_id, car_id, car_json)
                 VALUES (?1, ?2, ?3)",
                params![vehicle_id.to_string(), car_id, "{}"],
            )
            .expect("map source car");
    }

    #[test]
    fn watermark_and_verification_use_only_durable_observation_metadata() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (source, vehicle) = {
            let source = store
                .register_source(&SourceDescriptor::new("teslamate", "cutover-test"), 1_000)
                .expect("source");
            let vehicle = store
                .register_vehicle(&VehicleDescriptor::new(source.source_id, "vin:test"), 1_001)
                .expect("vehicle");
            (source, vehicle)
        };
        map_car(&store, vehicle.vehicle_id, 17);

        store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: 2_000,
                    payload: serde_json::json!({"secret_like": "payload must not be read"}),
                },
                2_001,
            )
            .expect("first observation");

        let read_only = HubStore::open_read_only(temporary.path()).expect("read-only store");
        let watermark = read_only
            .observation_watermark(17)
            .expect("watermark");
        assert_eq!(watermark.observation_id, 1);
        assert_eq!(watermark.observed_at_ms, Some(2_000));
        assert_eq!(watermark.received_at_ms, Some(2_001));
        assert!(!read_only
            .verify_observation_after(17, watermark.observation_id)
            .expect("pre-cutover verification")
            .verified());

        store
            .append_observation(
                &ObservationInput {
                    source_id: source.source_id,
                    vehicle_id: vehicle.vehicle_id,
                    observed_at_ms: 3_000,
                    payload: serde_json::json!({"next": true}),
                },
                3_001,
            )
            .expect("new observation");
        let verification = read_only
            .verify_observation_after(17, watermark.observation_id)
            .expect("verification");
        assert!(verification.verified());
        assert_eq!(verification.latest_observation_id, Some(2));
        assert_eq!(verification.latest_observed_at_ms, Some(3_000));
        assert_eq!(verification.latest_received_at_ms, Some(3_001));
    }

    #[test]
    fn source_car_mapping_fails_closed_when_missing_or_ambiguous() {
        let temporary = tempfile::tempdir().expect("temporary database");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("tesla_owner_api", "mapping-test"), 1_000)
            .expect("source");
        let vehicle = store
            .register_vehicle(&VehicleDescriptor::new(source.source_id, "vehicle-test"), 1_001)
            .expect("vehicle");
        assert!(matches!(
            store.observation_watermark(17),
            Err(ObservationVerificationError::NoVehicleMapping)
        ));

        map_car(&store, vehicle.vehicle_id, 17);
        let other_source = store
            .register_source(&SourceDescriptor::new("teslamate", "other"), 2_000)
            .expect("other source");
        let other_vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(other_source.source_id, "vin:other"),
                2_001,
            )
            .expect("other vehicle");
        map_car(&store, other_vehicle.vehicle_id, 17);

        assert!(matches!(
            store.observation_watermark(17),
            Err(ObservationVerificationError::AmbiguousVehicleMapping)
        ));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AddressCacheRecord {
    pub osm_type: String,
    pub osm_id: i64,
    pub display_name: String,
    pub name: Option<String>,
    pub lookup_latitude: f64,
    pub lookup_longitude: f64,
    pub looked_up_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AddressEnrichmentJob {
    pub job_key: String,
    pub vehicle_id: Uuid,
    pub target_type: String,
    pub target_id: i64,
    pub field: String,
    pub latitude: f64,
    pub longitude: f64,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressEnrichmentCompletion {
    pub vehicle_id: Uuid,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerrainCandidate {
    pub vehicle_id: Uuid,
    pub position: ProjectionPosition,
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
fn publish_manifest_in_transaction(
    transaction: &Transaction<'_>,
    manifest: &SyncManifest,
) -> Result<(), StoreError> {
    manifest.validate().map_err(StoreError::Manifest)?;
    let payload = serde_json::to_vec(manifest).map_err(StoreError::SerializeManifest)?;
    let snapshot_id = manifest.snapshot_id.to_string();
    let vehicle_id = manifest.vehicle_id.to_string();
    let head_sequence = i64::try_from(manifest.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let current = transaction.query_row(
        "SELECT snapshot_id, head_sequence FROM sync_manifests WHERE vehicle_id = ?1 ORDER BY head_sequence DESC LIMIT 1",
        params![vehicle_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    ).optional().map_err(StoreError::Query)?;
    if let Some((current_snapshot_id, current_sequence)) = current {
        let current_sequence = u64::try_from(current_sequence).map_err(|_| StoreError::InvalidStoredSequence)?;
        if current_sequence > manifest.head_sequence || (current_sequence == manifest.head_sequence && current_snapshot_id != snapshot_id) {
            return Err(StoreError::StaleManifest { vehicle_id: manifest.vehicle_id, attempted: manifest.head_sequence, current: current_sequence });
        }
    }
    transaction.execute(
        "INSERT INTO sync_manifests(snapshot_id, vehicle_id, head_sequence, manifest_json)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(snapshot_id) DO UPDATE SET vehicle_id = excluded.vehicle_id,
            head_sequence = excluded.head_sequence, manifest_json = excluded.manifest_json",
        params![snapshot_id, vehicle_id, head_sequence, payload],
    ).map_err(StoreError::PublishManifest)?;
    transaction.execute("DELETE FROM sync_packs WHERE snapshot_id = ?1", params![manifest.snapshot_id.to_string()])
        .map_err(StoreError::PublishManifest)?;
    for pack in &manifest.chunks {
        transaction.execute(
            "INSERT INTO sync_packs(sha256, snapshot_id, ordinal, relative_path, compressed_bytes, uncompressed_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![pack.sha256.to_string(), manifest.snapshot_id.to_string(), i64::from(pack.ordinal),
                pack.relative_path, i64::try_from(pack.compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
                i64::try_from(pack.uncompressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?],
        ).map_err(StoreError::PublishManifest)?;
    }
    if manifest.schema == crate::hub_pack::HUB_PROJECTION_SCHEMA_V2 && !manifest.chunks.is_empty() {
        let base_digest = manifest.chunks[0].sha256.to_string();
        let packs_json = serde_json::to_vec(&manifest.chunks).map_err(StoreError::SerializeManifest)?;
        transaction.execute(
            "INSERT INTO sync_bases(vehicle_id, snapshot_id, base_sequence, base_digest, packs_json)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(vehicle_id) DO NOTHING",
            params![manifest.vehicle_id.to_string(), manifest.snapshot_id.to_string(), head_sequence, base_digest, packs_json],
        ).map_err(StoreError::LineageCatalog)?;
        transaction.execute(
            "INSERT INTO sync_heads(vehicle_id, base_snapshot_id, head_sequence, head_digest, terminal_cursor)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(vehicle_id) DO NOTHING",
            params![manifest.vehicle_id.to_string(), manifest.snapshot_id.to_string(), head_sequence,
                manifest.chunks[0].sha256.to_string(), serde_json::to_string(&manifest.terminal_cursor).map_err(StoreError::SerializeManifest)?],
        ).map_err(StoreError::LineageCatalog)?;
        transaction.execute(
            "UPDATE sync_mutations SET published = 1, claimed_until_ms = 0 WHERE vehicle_id = ?1 AND published = 0",
            params![manifest.vehicle_id.to_string()],
        ).map_err(StoreError::LineageCatalog)?;
    }
    Ok(())
}

fn record_snapshot_fingerprint_in_transaction(
    transaction: &Transaction<'_>,
    manifest: &SyncManifest,
    fingerprint: Sha256Digest,
) -> Result<(), StoreError> {
    manifest.validate().map_err(StoreError::Manifest)?;
    let head_sequence =
        i64::try_from(manifest.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?;
    let associated: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_manifests
                 WHERE snapshot_id = ?1 AND vehicle_id = ?2 AND head_sequence = ?3
            )",
            params![manifest.snapshot_id.to_string(), manifest.vehicle_id.to_string(), head_sequence],
            |row| row.get(0),
        )
        .map_err(StoreError::PublishManifest)?;
    if !associated {
        return Err(StoreError::FingerprintManifestMissing(manifest.snapshot_id));
    }
    transaction
        .execute(
            "INSERT INTO snapshot_fingerprints(
                vehicle_id, fingerprint_sha256, snapshot_id, head_sequence
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                fingerprint_sha256 = excluded.fingerprint_sha256,
                snapshot_id = excluded.snapshot_id,
                head_sequence = excluded.head_sequence",
            params![
                manifest.vehicle_id.to_string(),
                fingerprint.as_bytes().as_slice(),
                manifest.snapshot_id.to_string(),
                head_sequence,
            ],
        )
        .map_err(StoreError::PublishManifest)?;
    Ok(())
}

fn upsert_geofences_in_transaction(
    transaction: &Transaction<'_>, vehicle_id: Uuid,
    geofences: &[crate::teslamate_projection::TeslaMateGeofence],
) -> Result<usize, StoreError> {
    let mut inserted = 0;
    for geofence in geofences {
        let Some((latitude, longitude, radius_m)) = geofence.valid_geometry() else { continue; };
        if geofence.name.trim().is_empty() || geofence.name.len() > 256 { continue; }
        inserted += transaction.execute(
            "INSERT INTO geofences(vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                billing_type, cost_per_unit, session_fee) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(vehicle_id, source_geofence_id) DO NOTHING",
            params![vehicle_id.to_string(), geofence.id, geofence.name.trim(), latitude, longitude, radius_m,
                geofence.billing_type.map(crate::hub_pack::GeofenceBillingType::as_str), geofence.cost_per_unit, geofence.session_fee],
        ).map_err(StoreError::LifecycleWrite)?;
        transaction.execute(
            "UPDATE geofences SET name=?3, latitude=?4, longitude=?5, radius_m=?6,
                billing_type=COALESCE(?7,billing_type), cost_per_unit=COALESCE(?8,cost_per_unit),
                session_fee=COALESCE(?9,session_fee) WHERE vehicle_id=?1 AND source_geofence_id=?2",
            params![vehicle_id.to_string(), geofence.id, geofence.name.trim(), latitude, longitude, radius_m,
                geofence.billing_type.map(crate::hub_pack::GeofenceBillingType::as_str), geofence.cost_per_unit, geofence.session_fee],
        ).map_err(StoreError::LifecycleWrite)?;
    }
    Ok(inserted)
}

fn promote_imported_open_session_in_transaction(
    transaction: &Transaction<'_>, source_id: Uuid, vehicle_id: Uuid, car_id: i64,
    session: &TeslaMateOpenSession, updated_at_ms: i64, expected: Option<(i64, i64)>,
) -> Result<OpenSessionSeedReport, StoreError> {
    if source_id.is_nil() || vehicle_id.is_nil() || car_id <= 0 { return Err(StoreError::InvalidLifecycleCarId); }
    validate_timestamp("open session updated_at_ms", updated_at_ms)?;
    session.validate().map_err(|_| StoreError::InvalidLifecycleSession)?;
    let previous = load_lifecycle_state_in_transaction(transaction, vehicle_id)?;
    if let Some((last_observation_id, prior_updated_at_ms)) = expected {
        let actual = previous.as_ref().map(|state| (state.last_observation_id, state.updated_at_ms));
        if actual != Some((last_observation_id, prior_updated_at_ms)) { return Err(StoreError::ImportGenerationConflict); }
    }
    let previous_state = previous.as_ref().map(|state| crate::lifecycle::OpenSessionState::decode(&state.open_session_json)
        .map_err(|_| StoreError::InvalidLifecycleSession)).transpose()?;
    let seeded = crate::lifecycle::seed_imported_open_session_state(source_id, session, previous_state.as_ref())
        .map_err(|_| StoreError::InvalidLifecycleSession)?;
    ensure_source_exists(transaction, source_id)?;
    ensure_vehicle_source(transaction, vehicle_id, source_id)?;
    let source = source_id.to_string();
    let vehicle = vehicle_id.to_string();
    transaction.execute("DELETE FROM lifecycle_open_rows WHERE source_id=?1 AND vehicle_id=?2", params![source, vehicle])
        .map_err(StoreError::LifecycleWrite)?;
    transaction.execute("DELETE FROM lifecycle_source_watermarks WHERE source_id=?1 AND vehicle_id=?2", params![source, vehicle])
        .map_err(StoreError::LifecycleWrite)?;
    let mut inserted = 0;
    if let Some(row) = &session.drive { inserted += insert_open_row(transaction, &source, "drives", row.id, &vehicle, car_id, "drive", None, row)?; }
    for row in &session.drive_positions { inserted += insert_open_row(transaction, &source, "positions", row.id, &vehicle, car_id, "position", row.drive_id, row)?; }
    if let Some(row) = &session.charge { inserted += insert_open_row(transaction, &source, "charging_processes", row.id, &vehicle, car_id, "charge", None, row)?; }
    for row in &session.charge_samples { inserted += insert_open_row(transaction, &source, "charges", row.id, &vehicle, car_id, "charge_sample", Some(row.charging_process_id), row)?; }
    if let Some(row) = &session.state { inserted += insert_open_row(transaction, &source, "states", row.id, &vehicle, car_id, "state", None, row)?; }
    for row in &session.standalone_positions { inserted += insert_open_row(transaction, &source, "positions", row.id, &vehicle, car_id, "standalone_position", None, row)?; }
    let mut standalone_positions_inserted = 0;
    for row in &session.standalone_positions {
        let position = crate::lifecycle::imported_position(row);
        let json = serde_json::to_string(&position).map_err(StoreError::SerializeLifecycleRow)?;
        standalone_positions_inserted += transaction.execute(
            "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json,
                speed, power, est_battery_range_km, fan_status, driver_temp_setting, passenger_temp_setting,
                is_climate_on, is_rear_defroster_on, is_front_defroster_on, battery_heater, battery_heater_on,
                battery_heater_no_power, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
             ON CONFLICT(vehicle_id, position_id) DO NOTHING",
            params![vehicle, position.id, car_id, json, position.speed, position.power, position.est_battery_range_km,
                position.fan_status, position.driver_temp_setting, position.passenger_temp_setting,
                position.is_climate_on.map(i64::from), position.is_rear_defroster_on.map(i64::from),
                position.is_front_defroster_on.map(i64::from), position.battery_heater.map(i64::from),
                position.battery_heater_on.map(i64::from), position.battery_heater_no_power.map(i64::from),
                position.tpms_pressure_fl, position.tpms_pressure_fr, position.tpms_pressure_rl, position.tpms_pressure_rr],
        ).map_err(StoreError::LifecycleWrite)?;
    }
    let watermarks = [("drives", session.watermarks.drives), ("positions", session.watermarks.positions),
        ("charging_processes", session.watermarks.charging_processes), ("charges", session.watermarks.charges),
        ("states", session.watermarks.states), ("updates", session.watermarks.updates)];
    for (domain, watermark) in watermarks {
        transaction.execute(
            "INSERT INTO lifecycle_source_watermarks(source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(source_id, vehicle_id, domain) DO UPDATE SET
             max_source_row_id=MAX(max_source_row_id, excluded.max_source_row_id),
             max_timestamp_ms=MAX(max_timestamp_ms, excluded.max_timestamp_ms)",
            params![source, vehicle, domain, watermark.max_id, watermark.max_timestamp_ms],
        ).map_err(StoreError::LifecycleWrite)?;
    }
    let json = seeded.encode().map_err(|_| StoreError::InvalidLifecycleSession)?;
    transaction.execute(
        "INSERT INTO vehicle_lifecycle_state(vehicle_id, car_id, last_observation_id, open_session_json, quarantined, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, 0, ?5) ON CONFLICT(vehicle_id) DO UPDATE SET car_id=excluded.car_id,
         open_session_json=excluded.open_session_json, updated_at_ms=MAX(updated_at_ms, excluded.updated_at_ms)",
        params![vehicle, car_id, previous.as_ref().map_or(0, |state| state.last_observation_id), json, updated_at_ms],
    ).map_err(StoreError::LifecycleWrite)?;
    mark_export_dirty_in_transaction(transaction, vehicle_id)?;
    Ok(OpenSessionSeedReport { provisional_rows_inserted: inserted, standalone_positions_inserted,
        watermarks_written: watermarks.len(), no_op: false })
}
