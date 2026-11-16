// SPDX-License-Identifier: AGPL-3.0-only

//! The rename's migrations and fallbacks (DEC-160): what lets a Hub installed
//! under the earlier product name keep its data, service, Keychain items,
//! settings and paired clients after the rename. Every literal of the earlier
//! spelling here sits on a line marked `rename:keep`; the current names are
//! the product's own constants, so before the rename every step finds the
//! earlier and current names equal and does nothing.

pub mod spelling;
pub mod env;
pub mod keychain;
pub mod wire;
#[cfg(unix)]
pub mod folders;
#[cfg(unix)]
pub mod config_paths;
#[cfg(unix)]
pub mod hand_over;
#[cfg(unix)]
pub mod cli;
#[cfg(all(test, target_os = "macos"))]
mod tests;

#[cfg(unix)]
pub use config_paths::prepare_config_path;
#[cfg(unix)]
pub use cli::run_if_requested;
