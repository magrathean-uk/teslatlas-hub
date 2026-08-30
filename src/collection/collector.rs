// SPDX-License-Identifier: AGPL-3.0-only

//! Persistence boundary for TeslaMate legacy Owner API reads.
//!
//! Networking lives in `owner_api`; this module turns completed reads into
//! bounded, append-only Hub observations, materialises durable drive/charge
//! history through the pure lifecycle projector, and optionally runs a
//! supervised no-wake schedule. Credentials are never held in configuration or
//! argv.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
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
    credentials::{CredentialError, LegacyAuthManager, LegacyAuthManagerError, OwnerTokens},
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
    fleet_telemetry::FleetTelemetrySnapshot,
    geocoder::{AdmittedUserEgressGuard, Geocoder, GeocoderError},
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionDeltaPackRequest, ProjectionPackError,
        ProjectionPackRequest, ProjectionPackWriter, ProjectionSnapshot,
    },
    legacy_auth::{LegacyAuth, LegacyAuthError, LegacyAuthFuse},
    lifecycle::{
        LifecycleError, LifecycleSample, OpenSessionState, apply_sample, force_close_for_service,
        stream_observation_payload,
    },
    location::Wgs84Point,
    owner_api::{
        LegacyVehicleAction, LegacyVehicleActionResult, ManualCollection, OwnerApi,
        OwnerApiAuthError, OwnerApiConfigError, OwnerApiError, StreamVehicleId, Vehicle,
        VehicleCollectionFailure, VehicleData, VehicleId,
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
const OWNER_API_SOURCE_KIND: &str = "owner_api_compat";
const OWNER_API_SOURCE_KEY: &str = "local_installation_v1";
const FLEET_API_SOURCE_KIND: &str = "fleet_api_compat";
const FLEET_API_SOURCE_KEY: &str = "local_installation_v1";
const EARLIEST_PLAUSIBLE_TIMESTAMP_MS: i64 = 946_684_800_000; // 2000-01-01 UTC
const FUTURE_TIMESTAMP_SKEW_MS: i64 = 5 * 60 * 1000;
const STREAM_SOURCE_KIND: &str = OWNER_API_SOURCE_KIND;
const STREAM_SOURCE_KEY: &str = OWNER_API_SOURCE_KEY;
const FLEET_TELEMETRY_CONFIG_LIFETIME_SECONDS: u64 = 30 * 24 * 60 * 60;
const FLEET_TELEMETRY_CONFIG_RENEWAL_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const FLEET_TELEMETRY_CONFIG_RETRY_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const FLEET_REFRESH_REQUEST_NOT_SENT_RETRY: Duration = Duration::from_secs(5 * 60);

include!("collector/authentication.rs");
include!("collector/fleet_setup.rs");
include!("collector/terrain.rs");
include!("collector/fleet_supervision.rs");
include!("collector/owner_supervision.rs");
include!("collector/streaming.rs");
include!("collector/scheduler.rs");
include!("collector/projection.rs");
include!("collector/errors.rs");

#[cfg(test)]
#[path = "collector/tests.rs"]
mod tests;
