// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

use super::*;
use crate::{
    credentials::{LegacyAuthManager, OwnerTokens},
    db::{SourceDescriptor, StoreReadFault, SyncMutation, SyncMutationClaim, VehicleDescriptor},
    lifecycle::OpenSessionState,
    owner_api::{Vehicle, VehicleData},
};

// Paused worker clocks must not expire while another runtime owns the pass lock.
static TERRAIN_PASS_TEST_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// Cache ownership counts are process-wide, so real adapter fixtures share this lock.
static TERRAIN_CACHE_TEST_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Independent test observation of ciphertext immutability after admission loss.
/// It never supplies credentials or a generation to a running manager.
fn collector_pair_snapshot(store: &HubStore, fleet: bool) -> Option<(Vec<u8>,Vec<u8>,Option<Uuid>,i64,i64)> {
    use rusqlite::OptionalExtension;
    store.open().expect("snapshot observer").query_row("SELECT access_envelope,refresh_envelope,generation,expires_at,next_refresh_at FROM credential_pairs WHERE account_id=1 AND slot=?1",[if fleet { "fleet_api" } else { "owner_api" }],|r| {
        let generation: String = r.get(2)?;
        Ok((r.get(0)?,r.get(1)?,Uuid::parse_str(&generation).ok(),r.get(3)?,r.get(4)?))
    }).optional().expect("snapshot")
}
fn collector_unresolved(store: &HubStore, fleet: bool) -> Result<bool,crate::db::StoreError> {
    store.open()?.query_row("SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts a JOIN outbound_request_receipts r ON r.id=a.receipt_id WHERE a.slot=?1 AND r.outcome='started')",[if fleet { "fleet_api" } else { "owner_api" }],|r| r.get(0)).map_err(crate::db::StoreError::Query)
}

// Opt-in step measurements keep ordinary test output quiet.
macro_rules! terrain_test_timing {
    ($($arg:tt)*) => {
        if std::env::var_os("TESLATLAS_TEST_TIMING").is_some() {
            eprintln!($($arg)*);
        }
    };
}

include!("tests/runtime_and_setup.rs");
include!("tests/legacy_auth.rs");
include!("tests/projection_and_outbox.rs");
include!("tests/close_count.rs");
include!("tests/supervised_restart.rs");
include!("tests/cadence_wait.rs");
include!("tests/scheduler.rs");
include!("tests/scheduler_core.rs");
include!("tests/durable_state.rs");
include!("tests/error_policy.rs");
include!("tests/holds_and_actions.rs");
include!("tests/refusals.rs");
include!("tests/request_counter.rs");
include!("tests/request_counter_b2.rs");
include!("tests/sleep_policy.rs");
include!("tests/stream_evidence.rs");
include!("tests/stream_evidence_b2.rs");
include!("tests/no_wake_journey.rs");
include!("tests/terrain.rs");
#[path = "tests/b3_c7_c.rs"]
mod b3_c7_c;
// C9 Part W worker scheduling and diagnostics.
#[path = "tests/b3_c9_a.rs"]
mod b3_c9_a;
#[path = "tests/b3_c9_p.rs"]
mod b3_c9_p;
#[path = "tests/b3_c9_s.rs"]
mod b3_c9_s;
#[path = "tests/b3_c9_w.rs"]
pub(super) mod b3_c9_w;
include!("tests/provider_secret_scrub.rs");
include!("tests/loop_waiting.rs");
include!("tests/loop_drain.rs");
include!("tests/credential_integration.rs");
include!("tests/credential_fix_round.rs");
include!("tests/fix_round_c3.rs");
include!("tests/supervised_fix2.rs");
include!("tests/late_denial.rs");
include!("tests/write_guard.rs");
include!("tests/write_guard_f3.rs");
include!("tests/r3_fix_round.rs");
