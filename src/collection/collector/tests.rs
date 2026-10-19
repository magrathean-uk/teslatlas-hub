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
    db::{SourceDescriptor, SyncMutation, SyncMutationClaim, VehicleDescriptor},
    lifecycle::OpenSessionState,
    owner_api::{Vehicle, VehicleData},
};

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
