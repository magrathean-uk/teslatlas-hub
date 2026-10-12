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
/// Default silence tolerance `T_gap` passed to
/// `apply_sample_with_offline_drive_timeout`: 10 minutes (C1 section 5, R43,
/// R45, R65).
///
/// It is the longest silence, or stationary run of a gear-less source, that
/// the Hub still treats as continuity. It must exceed the longest normal
/// interval between observations of a driving or charging car (Fleet polling
/// defaults of 30 s driving and 120 s charging; the stream is near real time)
/// and an ordinary connectivity drop on a continuous drive (a 15 km tunnel at
/// 90 km/h takes 10 minutes). A larger value would not change end times, which
/// are the last evidence, but would delay the close of a drive from a gear-less
/// source and widen the window in which two trips separated by an unobserved
/// stop merge. The configured `offline_drive_timeout_seconds` default must
/// equal this constant.
pub(crate) const DEFAULT_OFFLINE_DRIVE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

include!("lifecycle/models.rs");
include!("lifecycle/projection.rs");
include!("lifecycle/drive_and_charge.rs");
include!("lifecycle/state_and_updates.rs");

#[cfg(test)]
#[path = "lifecycle/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lifecycle/phase_tests.rs"]
mod phase_tests;

#[cfg(test)]
#[path = "lifecycle/drive_tests.rs"]
pub(crate) mod drive_tests;

#[cfg(test)]
#[path = "lifecycle/charge_tests.rs"]
pub(crate) mod charge_tests;

#[cfg(test)]
#[path = "lifecycle/power_tests.rs"]
mod power_tests;

#[cfg(test)]
#[path = "lifecycle/energy_tests.rs"]
pub(crate) mod energy_tests;

#[cfg(test)]
#[path = "lifecycle/cost_tests.rs"]
mod cost_tests;

#[cfg(test)]
#[path = "lifecycle/efficiency_tests.rs"]
mod efficiency_tests;
