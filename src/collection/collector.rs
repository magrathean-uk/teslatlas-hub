// SPDX-License-Identifier: AGPL-3.0-only

//! Persistence boundary for TeslaMate legacy Owner API reads.
//!
//! Networking lives in `owner_api`; this module turns completed reads into
//! bounded, append-only Hub observations, materialises durable drive/charge
//! history through the pure lifecycle projector, and optionally runs a
//! supervised no-wake schedule. Credentials are never held in configuration or
//! argv.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    future::Future,
    io::Read,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rustix::{
    fs::{FileType, Mode, OFlags, fstat, open},
    process::getuid,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::{Instant, MissedTickBehavior, sleep, timeout};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot, watch},
    task::{JoinError, JoinHandle},
};
use uuid::Uuid;

use crate::{
    config::{CollectorCadence, CollectorProvider, ConfigError, HubConfig, TerrainConfig},
    credentials::{
        AuthWait, CollectorStartView, CredentialError, LegacyAuthManager, LegacyAuthManagerError,
        OwnerTokens, RefusalRefresh,
    },
    db::{
        HubStore, ObservationInput, OutboundRequestCompletion, OutboundRequestOperation,
        OutboundRequestOutcome, OutboundRequestPrecondition, OutboundRequestSafetyClass,
        OutboundRequestStart, OutboundRequestTransport, SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
        SourceDescriptor, StoreError, StreamObservationResult, StreamObservationWriter,
        SupervisedCollectorLease, SupervisedCollectorState, VehicleDescriptor,
    },
    fleet_api::{
        FleetApi, FleetApiConfigError, FleetApiError, FleetAuthApi, FleetCommand,
        FleetCommandProxy, FleetCommandProxyBase, FleetCommandResult, FleetTelemetryConfigBuilder,
        FleetTelemetryVins, VehicleVin, WakeResult,
    },
    fleet_credentials::{FleetAuthManager, FleetCredentialError, FleetSetupCredentials},
    fleet_telemetry::{FleetTelemetryAccumulator, ReceiverEnvelope, ReceiverKind, parse_envelope},
    geocoder::{AdmittedUserEgressGuard, Geocoder, GeocoderError},
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionDeltaPackRequest, ProjectionPackError,
        ProjectionPackRequest, ProjectionPackWriter, ProjectionSnapshot,
    },
    legacy_auth::{
        AuthEndpointConfig, LegacyAuth, LegacyAuthError, LegacyAuthFuse, RefusalReason,
        state::{AuthState, AuthStatus, AuthStop},
    },
    lifecycle::{
        LifecycleError, LifecycleSample, OpenSessionState, apply_sample, stream_observation_payload,
    },
    location::Wgs84Point,
    owner_api::{
        LegacyVehicleAction, LegacyVehicleActionResult, ManualCollection, OwnerApi,
        OwnerApiAuthError, OwnerApiConfigError, OwnerApiError, ProviderSecrets, StreamVehicleId,
        Vehicle, VehicleCollectionFailure, VehicleData, VehicleId,
    },
    protocol::{
        CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V2, LineageDelta, OpaqueCursor, PROTOCOL_V1,
        ProtocolError, ProtocolLimits, SequenceRange, Sha256Digest, canonical_delta_chain_digest,
    },
    terrain_cache::{TerrainCache, TerrainCacheError},
    tesla_stream::{
        StreamEvent, StreamPowerGate, StreamRegion, TeslaStreamSupervisor, streaming_endpoint,
    },
};

#[cfg(test)]
use crate::db::StreamFaultPoint;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
const OWNER_API_SOURCE_KIND: &str = "owner_api_compat";
const OWNER_API_SOURCE_KEY: &str = "local_installation_v1";
const FLEET_API_SOURCE_KIND: &str = "fleet_api_compat";
const FLEET_API_SOURCE_KEY: &str = "local_installation_v1";
// Plausibility bounds of an observation time. The lifecycle projection owns the
// values and their reasons (C1 R5), so intake and projection use one rule.
use crate::lifecycle::{K_EARLIEST_MS, K_SKEW_MS};
const STREAM_SOURCE_KIND: &str = OWNER_API_SOURCE_KIND;
const STREAM_SOURCE_KEY: &str = OWNER_API_SOURCE_KEY;
const FLEET_TELEMETRY_CONFIG_LIFETIME_SECONDS: u64 = 30 * 24 * 60 * 60;
const FLEET_TELEMETRY_CONFIG_RENEWAL_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const FLEET_TELEMETRY_CONFIG_RETRY_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// The most mutations one publication claim takes.
const SYNC_CLAIM_MUTATION_LIMIT: usize = 10_000;
const FLEET_REFRESH_REQUEST_NOT_SENT_RETRY: Duration = Duration::from_secs(5 * 60);

include!("collector/authentication.rs");
include!("collector/supervised_test_support.rs");
include!("collector/fleet_setup.rs");
include!("collector/terrain.rs");
include!("collector/fleet_supervision.rs");
include!("collector/owner_supervision.rs");
include!("collector/streaming.rs");
include!("collector/scheduler.rs");
include!("collector/scheduler_sleep.rs");
include!("collector/scheduler_state.rs");
include!("collector/scheduler_errors.rs");
include!("collector/scheduler_holds.rs");
include!("collector/scheduler_counters.rs");
include!("collector/tick.rs");
include!("collector/projection.rs");
include!("collector/receiver.rs");
include!("collector/errors.rs");

/// What one bounded publication turn did (G2-6 6A S9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PublicationTurn {
    /// Mutations published by this turn.
    pub published: usize,
    /// The claim was full, so more unpublished mutations may remain: take
    /// another turn.
    pub more: bool,
}

/// Publish one bounded claim (at most `claim_limit` mutations, never more than
/// 10,000) of a vehicle's ordinary durable sync mutations. The caller holds the
/// publication gate for the whole turn and calls again while `more` is set, so a
/// long backlog is never published in one unbounded loop. The turn neither claims,
/// completes nor fails an export-outbox row: ordinary replay completes an empty
/// row later.
pub(crate) fn publish_unpublished_sync_mutations(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    now_ms: i64,
    _publication_gate: &crate::db::PublicationGate,
    claim_limit: usize,
) -> Result<PublicationTurn, CollectorError> {
    if !store.vehicle_has_v2_base(vehicle_id)? {
        return Err(StoreError::LineageCatalogConflict.into());
    }
    let Some(claim) = store.claim_sync_mutations(vehicle_id, now_ms, claim_limit)? else {
        return Ok(PublicationTurn {
            published: 0,
            more: false,
        });
    };
    let outcome = {
        #[cfg(test)]
        {
            if store.instrumentation().take_publication_fault() {
                Err(StoreError::InjectedStreamFault("publication-turn").into())
            } else {
                publish_v2_delta(store, cursor_key, &claim)
            }
        }
        #[cfg(not(test))]
        {
            publish_v2_delta(store, cursor_key, &claim)
        }
    };
    match outcome {
        Ok(()) => Ok(PublicationTurn {
            published: claim.mutations.len(),
            more: claim.mutations.len() >= claim_limit.min(SYNC_CLAIM_MUTATION_LIMIT),
        }),
        Err(error) => {
            store.release_sync_mutations(&claim)?;
            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "collector/tests.rs"]
mod tests;
