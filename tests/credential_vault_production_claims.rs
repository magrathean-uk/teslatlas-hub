// SPDX-License-Identifier: AGPL-3.0-only

use std::{os::unix::fs::PermissionsExt, sync::Arc};
use teslatlas_hub::{db::{HubStore, StoreError}, hub_user_process::AdmittedUserHub, open_credential_vault_for_entry};

/// Integration tests link the library without cfg(test), including both leases.
#[test]
fn production_claims_remain_taken_after_release() {
    let root = tempfile::tempdir().expect("production fixture");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).expect("private fixture");
    let store = HubStore::initialize(root.path()).expect("store");
    drop(store.take_credential_connection().expect("first take"));
    assert!(matches!(store.take_credential_connection(), Err(StoreError::CredentialConnectionTaken)), "production store is once only");

    let admission = AdmittedUserHub::admit(root.path()).expect("admission");
    let store = HubStore::initialize(root.path()).expect("fresh store");
    let entry = open_credential_vault_for_entry(store, Arc::clone(&admission), root.path())
        .unwrap_or_else(|error| panic!("production open: {}", error.code()));
    drop(entry);
    let store = HubStore::initialize(root.path()).expect("independent store");
    assert!(open_credential_vault_for_entry(store, admission, root.path()).is_err(), "production admission is once only");
}
