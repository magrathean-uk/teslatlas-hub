// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::protocol::{
    CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V3, LINEAGE_PROTOCOL_V2, LineageBase,
    LineageCapability, LineageDelta, LineageManifestV2, MirrorTable, OpaqueCursor, PackCompression,
    PackFormat, ProtocolError, ProtocolVersion, SchemaVersion, SequenceRange, TransferMode,
};

use crate::test_assertions::LabelledResult;

/// The shared fallible store constructor for synthetic fixtures.
fn try_test_store() -> Result<(tempfile::TempDir, HubStore), Box<dyn std::error::Error>> {
    let temporary = crate::private_tempdir()?;
    let store = HubStore::initialize(temporary.path())?;
    Ok((temporary, store))
}

include!("tests/database_and_migrations.rs");
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
include!("tests/receiver_and_retirement.rs");
include!("tests/auth_state.rs");
include!("tests/legacy_attempt_close.rs");
include!("tests/r3_fix_round.rs");

#[path = "tests/b3_c1_a.rs"]
mod b3_c1_a;
#[path = "tests/b3_inherited_drives.rs"]
mod b3_c1_b;
#[path = "tests/b3_c1_c.rs"]
mod b3_c1_c;
#[path = "tests/b3_c2_a.rs"]
mod b3_c2_a;
#[path = "tests/b3_c2_b.rs"]
mod b3_c2_b;
#[path = "tests/b3_c2_c.rs"]
mod b3_c2_c;
#[path = "tests/b3_c6_i1.rs"]
mod b3_c6_i1;
#[path = "tests/vehicle_identity_tidy.rs"]
mod b3_c6_i2;
#[path = "tests/b3_c8_l.rs"]
mod b3_c8_l;
#[path = "tests/b3_c8_sa.rs"]
mod b3_c8_sa;
#[path = "tests/b3_c8_sb.rs"]
mod b3_c8_sb;
#[path = "tests/b3_c9_ca.rs"]
mod b3_c9_ca;
#[path = "tests/b3_c9_cb.rs"]
mod b3_c9_cb;
#[path = "tests/b3_c9_cc.rs"]
mod b3_c9_cc;
#[path = "tests/b3_c9_q.rs"]
mod b3_c9_q;

#[path = "tests/g2_4_m1.rs"]
mod g2_4_m1;

#[path = "tests/source_key_guards.rs"]
mod source_key_guards;

#[path = "tests/import_storage_cost.rs"]
mod import_storage_cost;
