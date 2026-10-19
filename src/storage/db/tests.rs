// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::protocol::{
    CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V3, LINEAGE_PROTOCOL_V2, LineageBase,
    LineageCapability, LineageDelta, LineageManifestV2, MirrorTable, OpaqueCursor, PackCompression,
    PackFormat, ProtocolError, ProtocolVersion, SchemaVersion, SequenceRange, TransferMode,
};

include!("tests/database_and_migrations.rs");
include!("tests/edge_delivery.rs");
include!("tests/catalogue_and_pairing.rs");
include!("tests/device_vehicle_grants.rs");
include!("tests/device_credentials.rs");
include!("tests/observations_and_import_setup.rs");
include!("tests/projection_state.rs");
include!("tests/live_sync.rs");
include!("tests/import_generation.rs");
include!("tests/schema22_and_lifecycle.rs");
include!("tests/lifecycle_integration.rs");
include!("tests/drive_lifecycle.rs");
include!("tests/close_paging.rs");
include!("tests/charge_lifecycle.rs");
include!("tests/cost_close.rs");
include!("tests/seed_noop.rs");
include!("tests/cost_fill.rs");
include!("tests/efficiency.rs");
include!("tests/public_history_queries.rs");
include!("tests/address_lookup_cache.rs");
include!("tests/terrain_tile_digests.rs");
include!("tests/lineage_digest_cache.rs");
include!("tests/collector_state.rs");
include!("tests/auth_state.rs");
include!("tests/legacy_attempt_close.rs");
include!("tests/r3_fix_round.rs");
