// SPDX-License-Identifier: AGPL-3.0-only

use std::env;

use crate::protocol::{
    CursorClaims, LINEAGE_PROTOCOL_V2, LineageBase, LineageCapability, LineageDelta,
    LineageManifestV2, OpaqueCursor, PROTOCOL_V1,
};
use crate::teslamate_projection::TeslaMateGeofencePhysicalV2_2;

use super::*;

include!("tests/writer_basics.rs");
include!("tests/schema22_core.rs");
include!("tests/schema22_contracts.rs");
include!("tests/deltas.rs");
