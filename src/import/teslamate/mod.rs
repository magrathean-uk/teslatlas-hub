// SPDX-License-Identifier: AGPL-3.0-only

//! TeslaMate compatibility, capture, projection, and write-back.

pub mod direct;
pub mod fragments;
pub mod importer;
pub mod parity;
pub(crate) mod physical_delta_compare;
pub(crate) mod physical_delta_pack;
pub mod physical_fragments;
pub mod physical_publication;
pub(crate) mod prepared_map;
pub mod progress;
pub mod projection;
pub mod projection_state;
pub mod reader;
pub mod schema;
pub mod source;
pub mod stage;
pub mod writeback;
