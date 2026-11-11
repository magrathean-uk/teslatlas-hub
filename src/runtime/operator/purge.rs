// SPDX-License-Identifier: AGPL-3.0-only

//! The refused-credential purge (M02-T11, DEC-083).
//!
//! An upgraded store keeps the credentials it refuses (pre-upgrade source
//! rows, stored Fleet rows and pairs the Hub's own login did not make), their
//! old key files and old tables until the owner removes them; nothing is
//! deleted on upgrade. `purge-refused-credentials` is forwarded to the running
//! service like the other operator commands (ruling R5) and makes no network
//! connection. Without `--confirm` it lists what the purge would remove and
//! deletes nothing; with it, the service removes the refused rows, their
//! ciphertext and the old tables in one transaction and the key files after
//! the commit. The admitted own-login pair is never touched. The purge is
//! refused while a collection window is open or a refresh is unresolved.
//! Status, the doctor and the backup refusal name the command until then.

use std::path::Path;

use serde_json::{Value, json};

use super::admission::OperatorRefusal;
use crate::{
    CredentialVaultEntry,
    auth::credential_vault::{self, CredentialStoreError, RefusedCredentialReport},
    db::{HubStore, REFUSED_PURGE_COLLECTOR_RUNNING, REFUSED_PURGE_REFRESH_ACTIVE, StoreError},
};

/// The command, as status, the doctor and the backup refusal name it.
pub const PURGE_COMMAND: &str = credential_vault::REFUSED_CREDENTIAL_PURGE_COMMAND;

fn encode(report: &RefusedCredentialReport) -> Result<Value, OperatorRefusal> {
    serde_json::to_value(report)
        .map_err(|_| OperatorRefusal::new("purge_failed", "the report is not encodable"))
}

fn store_refusal(error: CredentialStoreError) -> OperatorRefusal {
    match error {
        CredentialStoreError::Store(StoreError::WriteRefused(code))
            if code == REFUSED_PURGE_COLLECTOR_RUNNING =>
        {
            OperatorRefusal::new(
                code,
                "a collector is running inside a collection window; close the window and try again",
            )
        }
        CredentialStoreError::Store(StoreError::WriteRefused(code))
            if code == REFUSED_PURGE_REFRESH_ACTIVE =>
        {
            OperatorRefusal::new(
                code,
                "a refresh of the Hub's own sign-in is unresolved; try again once it has finished",
            )
        }
        other => OperatorRefusal::new(
            other.code(),
            "the credential store refused the purge; nothing more was removed",
        ),
    }
}

/// The listing: read-only, so nothing is deleted or written.
pub fn list(store: &HubStore, data_dir: &Path) -> Result<Value, OperatorRefusal> {
    encode(&credential_vault::refused_credential_listing(store, data_dir).map_err(store_refusal)?)
}

/// The confirmed purge, on the service's one credential vault.
pub fn purge(entry: &CredentialVaultEntry) -> Result<Value, OperatorRefusal> {
    encode(&entry.purge_refused_credentials().map_err(store_refusal)?)
}

/// What status and the doctor report while refused credentials are kept:
/// the refused slots and the command that lists and removes them. `None`
/// once nothing is refused.
pub fn hint(store: &HubStore) -> Result<Option<Value>, StoreError> {
    let inventory = store.refused_credential_inventory()?;
    if inventory.rows.is_empty() {
        return Ok(None);
    }
    let slots: Vec<&str> = inventory.rows.iter().map(|row| row.slot.as_str()).collect();
    Ok(Some(json!({
        "refusedSlots": slots,
        "command": PURGE_COMMAND,
        "message": format!(
            "The Hub keeps the credentials it refuses until you remove them. List them with `{PURGE_COMMAND}` and remove them with `{PURGE_COMMAND} --confirm`."
        ),
    })))
}
