// SPDX-License-Identifier: AGPL-3.0-only

// Used by build.rs and by the library tests; unused in the library itself.
#[allow(dead_code)]
pub const UNBOUND_SOURCE_COMMIT: &str = "UNBOUND";

pub fn is_exact_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
