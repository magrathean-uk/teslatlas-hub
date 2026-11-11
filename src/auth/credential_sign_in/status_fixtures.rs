// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::Arc;
use crate::{auth::{credential_migration, credential_vault::{CredentialVault, VaultOptions, fixtures::{FixtureExclusion, fixture_admission, open_fixture_vault}}}, db::HubStore};

pub(crate) fn phase_b(kind: &str) -> (tempfile::TempDir, HubStore) {
    let root = match kind {
        "mapping" => credential_migration::g24_m2_m::seed_admitted_fence_mapping(),
        "blocked" => credential_migration::g24_m2_m::seed_admitted_blocked_owner(),
        _ => credential_migration::g24_m2_m::seed_admitted_open_fixture(false, None, false, 0),
    };
    let store = HubStore::initialize(root.path()).expect("status fixture store");
    if kind == "pending" { return (root, store); }
    credential_migration::seam::reset();
    credential_migration::seam::with(|state| {
        state.fake_pre_upgrade = true;
        state.fake_generation = true;
        if kind == "purge_wait" { state.free_bytes = Some((0, 0)); }
    });
    drop(open_fixture_vault(store.clone(), root.path()));
    credential_migration::seam::reset();
    (root, store)
}

pub(crate) fn durability_failure() -> (tempfile::TempDir, HubStore) {
    let root = crate::private_tempdir().expect("durability fixture");
    let store = HubStore::initialize(root.path()).expect("store");
    let admission = fixture_admission(&store, root.path());
    drop(CredentialVault::open(store.clone(), admission, root.path(), VaultOptions {
        exclusion: Some(Arc::new(FixtureExclusion)), probe_failure: true, ..VaultOptions::default()
    }).expect("read-only vault"));
    (root, store)
}

pub(crate) fn recover(store: HubStore, data_dir: &std::path::Path) -> HubStore {
    let store = crate::auth::credential_vault::fixtures::restart_fixture_context(store, data_dir).expect("new fixture process");
    credential_migration::seam::with(|state| { state.fake_pre_upgrade = true; state.fake_generation = true; });
    drop(open_fixture_vault(store.clone(), data_dir));
    credential_migration::seam::reset();
    store
}
