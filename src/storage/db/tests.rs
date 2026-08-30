// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::protocol::{
    CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V3, LINEAGE_PROTOCOL_V2, LineageBase,
    LineageCapability, LineageDelta, LineageManifestV2, MirrorTable, OpaqueCursor, PackCompression,
    PackFormat, ProtocolError, ProtocolVersion, SchemaVersion, SequenceRange, TransferMode,
};

include!("tests/database_and_migrations.rs");
include!("tests/catalogue_and_pairing.rs");
include!("tests/observations_and_import_setup.rs");
include!("tests/projection_state.rs");
include!("tests/live_sync.rs");
include!("tests/import_generation.rs");
include!("tests/schema22_and_lifecycle.rs");
