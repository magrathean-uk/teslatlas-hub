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
use rcgen::{CertifiedKey, generate_simple_self_signed};
use reqwest::{Certificate, Client};
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
include!("tests/supervised_restart.rs");
include!("tests/scheduler.rs");
include!("tests/terrain.rs");
