// SPDX-License-Identifier: AGPL-3.0-only

//! This host's id: a digest of the platform identity, so a grant names one
//! host without carrying the raw identifier. macOS reads the platform UUID
//! (readable without privilege; a Tart guest has its own); elsewhere the
//! machine id. A cloned VM may share it, which M08-T03 records.

use std::process::Command;

use super::admission::OperatorRefusal;
use crate::collection::window::ledger::sha256_hex;

pub const HOST_ID_UNAVAILABLE: &str = "host_id_unavailable";

const DOMAIN: &str = "teslatlas-hub-host-id/1";

/// The host id for a platform identity string.
pub fn host_id_for(identity: &str) -> String {
    let digest = sha256_hex(format!("{DOMAIN}\n{}", identity.trim()).as_bytes());
    format!("host-{}", &digest[..32])
}

/// Reads `"IOPlatformUUID" = "…"` from `ioreg` output.
pub fn parse_platform_uuid(text: &str) -> Option<String> {
    text.lines()
        .find(|line| line.contains("\"IOPlatformUUID\""))
        .and_then(|line| line.split('=').nth(1))
        .map(|value| value.trim().trim_matches('"').to_owned())
        .filter(|value| value.len() >= 8)
}

fn unavailable() -> OperatorRefusal {
    OperatorRefusal::new(HOST_ID_UNAVAILABLE, "this host's identity cannot be read")
}

/// This host's id.
pub fn this_host_id() -> Result<String, OperatorRefusal> {
    if cfg!(target_vendor = "apple") {
        let output = Command::new("/usr/sbin/ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .map_err(|_| unavailable())?;
        let uuid = parse_platform_uuid(&String::from_utf8_lossy(&output.stdout))
            .ok_or_else(unavailable)?;
        Ok(host_id_for(&uuid))
    } else {
        let text = std::fs::read_to_string("/etc/machine-id").map_err(|_| unavailable())?;
        if text.trim().len() < 8 {
            return Err(unavailable());
        }
        Ok(host_id_for(&text))
    }
}
