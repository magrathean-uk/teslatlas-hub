// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

include!("credential_sign_in_core_tests.rs");
include!("credential_sign_in_entry_tests.rs");
include!("credential_sign_in_fault_tests.rs");
#[cfg(target_os = "macos")]
include!("credential_sign_in_service_tests.rs");
