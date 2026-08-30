// SPDX-License-Identifier: AGPL-3.0-only

//! Deterministic vehicle lifecycle projection from owner-API observations.
//!
//! This module is pure: it never performs I/O, never wakes a vehicle, and never
//! fabricates history from a single present-state sample. Open sessions are
//! serialized so a collector can resume after a process or host restart and
//! produce identical completed drives, positions, charges, and charge samples.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::hub_pack::{
    GeofenceBillingType, ProjectionCarPatch, ProjectionCharge, ProjectionChargeSample,
    ProjectionDrive, ProjectionPosition, ProjectionState, ProjectionUpdate,
    normalize_tesla_model_code,
};
use crate::teslamate_projection::{
    TeslaMateCharge, TeslaMateChargingProcess, TeslaMateDrive, TeslaMateOpenSession,
    TeslaMatePosition, TeslaMateState, project_charge_sample,
};

/// Maximum UTF-8 bytes retained for one vehicle's open-session blob.
///
/// An active drive retains positions at the driving cadence and an active
/// charge retains charge samples at the charging cadence. 64 KiB can be
/// exceeded by an ordinary long session, preventing the collector from
/// checkpointing and therefore from recovering safely. Keep a finite corrupt
/// input guard, but size it for multi-day real-world continuations.
pub const MAX_OPEN_SESSION_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const DEFAULT_OFFLINE_DRIVE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

include!("lifecycle/models.rs");
include!("lifecycle/projection.rs");
include!("lifecycle/drive_and_charge.rs");
include!("lifecycle/state_and_updates.rs");

#[cfg(test)]
#[path = "lifecycle/tests.rs"]
mod tests;
