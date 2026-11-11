// SPDX-License-Identifier: AGPL-3.0-only

//! Vehicle discovery, polling, streaming, and telemetry collection.

pub mod collector;
pub mod current_state;
pub mod fleet_telemetry;
#[cfg(test)]
pub(crate) mod loopback_fixture;
pub mod source_db_current;
pub mod tesla_stream;
