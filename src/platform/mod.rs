// SPDX-License-Identifier: AGPL-3.0-only

//! Operating-system service and process integration.

#[cfg(unix)]
pub mod hub_user_process;
#[cfg(target_os = "linux")]
pub mod linux_systemd;
#[cfg(target_os = "macos")]
pub mod macos_launch_agent;
#[cfg(unix)]
pub(crate) mod user_lifetime_lock;
