// SPDX-License-Identifier: AGPL-3.0-only

//! Vehicle discovery, polling, streaming, and telemetry collection.

pub mod collector;
pub mod current_state;
#[cfg(test)]
pub mod fake_tesla;
pub mod fleet_telemetry;
pub mod tesla_stream;
