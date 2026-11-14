// SPDX-License-Identifier: AGPL-3.0-only

use std::{os::unix::fs::PermissionsExt, sync::Arc};
use teslatlas_hub::{db::{HubStore, StoreError}, hub_user_process::AdmittedUserHub, open_credential_vault_for_entry};

/// Integration tests link the library without cfg(test), including both leases.
/// M02-T10: each claim is refused while held and released when its holder drops.
#[test]
fn production_claims_are_once_at_a_time_and_released_on_close() {
    let root = tempfile::tempdir().expect("production fixture");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).expect("private fixture");
    let store = HubStore::initialize(root.path()).expect("store");
    let held = store.take_credential_connection().expect("first take");
    assert!(matches!(store.take_credential_connection(), Err(StoreError::CredentialConnectionTaken)), "production store is once at a time");
    drop(held);
    drop(store.take_credential_connection().expect("take again after release"));

    let admission = AdmittedUserHub::admit(root.path()).expect("admission");
    let store = HubStore::initialize(root.path()).expect("fresh store");
    let entry = open_credential_vault_for_entry(store, Arc::clone(&admission), root.path())
        .unwrap_or_else(|error| panic!("production open: {}", error.code()));
    let store = HubStore::initialize(root.path()).expect("independent store");
    assert!(open_credential_vault_for_entry(store, Arc::clone(&admission), root.path()).is_err(), "production admission is once at a time");
    drop(entry);
    let store = HubStore::initialize(root.path()).expect("store after close");
    drop(open_credential_vault_for_entry(store, admission, root.path())
        .unwrap_or_else(|error| panic!("production reopen after close: {}", error.code())));
}
