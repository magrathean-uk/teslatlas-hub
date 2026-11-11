// SPDX-License-Identifier: AGPL-3.0-only

//! Operating-system service and process integration.

pub(crate) mod file_attempts;
#[cfg(unix)]
pub mod hub_user_process;
#[cfg(target_os = "linux")]
pub mod linux_systemd;
#[cfg(target_os = "macos")]
pub mod macos_launch_agent;
#[cfg(unix)]
pub(crate) mod private_directory;
pub mod r78_lifecycle;
#[cfg(unix)]
pub mod r78_service;
#[cfg(unix)]
pub mod r78_recording;
#[cfg(all(feature = "credential-relocation-fixture", not(debug_assertions)))]
compile_error!("credential-relocation-fixture is available only in debug builds");
#[cfg(unix)]
pub(crate) mod user_lifetime_lock;
