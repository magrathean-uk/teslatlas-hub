// SPDX-License-Identifier: AGPL-3.0-only

//! Operator commands behind the service's window authority (M02-T10).
//!
//! The command tree and its admission rules live here, and the binary calls
//! them: [`cli`] parses, [`admission`] routes every command before a secret
//! path, standard input or the network is touched, [`channel`] forwards the
//! window, setup and recovery verbs to the running service, and
//! [`authority`] is that service's side: one window controller on the
//! process-wide egress gate, the supervisor that runs the collector only
//! inside an Open window, and the channel handler. [`issuer`] holds the
//! issuer verbs of the issuer Mac.

use std::path::{Path, PathBuf};

pub mod admission;
pub mod authority;
pub mod channel;
pub mod cli;
pub mod host;
pub mod issuer;
pub mod trust;

#[cfg(test)]
mod tests;

/// `<data dir>/window`, the window controller's private directory.
pub fn window_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("window")
}
