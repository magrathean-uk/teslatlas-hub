// SPDX-License-Identifier: AGPL-3.0-only

//! Source database compatibility, capture, and projection.

pub(crate) mod canonical_json;
pub mod direct;
pub mod fragments;
pub mod importer;
pub mod parity;
pub(crate) mod physical_delta_compare;
pub(crate) mod physical_delta_pack;
pub mod progress;
pub mod projection;
pub mod projection_state;
pub mod reader;
pub mod schema;
pub mod source;
pub mod stage;
