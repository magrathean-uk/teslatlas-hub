// SPDX-License-Identifier: AGPL-3.0-only

// M02-T04: the refresh fence tests of `credential_pairs.rs`, included into its
// `refresh_fence_tests` module (kept apart so that file stays within the
// layout gate's size limit).

use super::*;
use crate::hub_user_process::AdmittedUserHub;
fn fenced_store() -> (tempfile::TempDir, HubStore) {
    let directory = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(directory.path()).expect("store");
    store.test_enforce_refresh_fence();
    (directory, store)
}
/// The process lock: this process's data-dir admission, bound as the vault binds it.
fn admit(directory: &Path, connection: &mut CredentialConnection) -> Arc<AdmittedUserHub> {
    let admission = AdmittedUserHub::admit(directory).expect("process lock");
    connection.bind_vault_admission(admission.take_credential_vault().expect("vault admission"));
    admission
}
fn seed(connection: &mut CredentialConnection, slot: CredentialSlot, generation: Uuid, refresh: &[u8]) -> Result<(), StoreError> {
    let now = connection.now_ms()? / 1_000;
    let pair = s_test_synthetic_pair(connection, slot, generation, b"fence-access", refresh, now + 28_800, now + 21_600)?;
    if slot == CredentialSlot::FleetApi {
        // M09-T08: only the Hub's own grant writes the Fleet slot.
        return connection.test_replace_own_fleet_grant(&pair, ReplacePolicy::ReplaceAny);
    }
    connection.replace_credential_pair(&pair, &EntryProvenance::Supplied, &LegacyCandidates(Vec::new()), ReplacePolicy::ReplaceAny)
}
fn attempts(connection: &CredentialConnection) -> i64 {
    connection.connection.query_row("SELECT count(*) FROM credential_refresh_attempts", [], |row| row.get(0)).expect("attempt count")
}
fn now_ms() -> i64 {
    outbound_request_clock_ms().expect("clock")
}
#[test]
fn refresh_fence_begin_needs_the_live_lease_of_this_store() {
    let (directory, store) = fenced_store();
    let mut connection = store.take_credential_connection().expect("connection");
    let _admission = admit(directory.path(), &mut connection);
    let input = Uuid::new_v4();
    seed(&mut connection, CredentialSlot::OwnerApi, input, b"fence-refresh-lease").expect("pair");
    let lease_lost = |result: Result<OutboundRequestReceiptId, StoreError>| matches!(result, Err(StoreError::SupervisedCollectorLeaseLost));
    assert!(lease_lost(connection.begin_credential_refresh(CredentialSlot::OwnerApi, input)), "no lease, no refresh");
    // Another handle's lease is another process's collector, not this store's.
    let other = HubStore::initialize(directory.path()).expect("second handle");
    let foreign = other.acquire_supervised_collector_lease(now_ms()).expect("foreign lease");
    assert!(lease_lost(connection.begin_credential_refresh(CredentialSlot::OwnerApi, input)), "a foreign lease does not admit");
    other.release_supervised_collector_lease(foreign).expect("foreign release");
    // This store's own lease row, once expired, does not admit either.
    let _stale = store.acquire_supervised_collector_lease(now_ms() - 86_400_000).expect("expired lease");
    assert!(lease_lost(connection.begin_credential_refresh(CredentialSlot::OwnerApi, input)), "an expired lease does not admit");
    assert_eq!(attempts(&connection), 0, "a refused begin writes no attempt");
    // A clone of the store shares the capability; its live lease admits one begin.
    let lease = store.clone().acquire_supervised_collector_lease(now_ms()).expect("live lease");
    let receipt = connection.begin_credential_refresh(CredentialSlot::OwnerApi, input).expect("begin under the live lease");
    connection.cancel_unsent_credential_refresh(receipt, input).expect("cancel unsent");
    store.release_supervised_collector_lease(lease).expect("release");
    assert!(lease_lost(connection.begin_credential_refresh(CredentialSlot::OwnerApi, input)), "a released lease ends the right to refresh");
    assert_eq!(attempts(&connection), 0, "the one admitted attempt was cancelled unsent; the refused begin added none");
}

#[test]
fn refresh_fence_lost_lease_withdraws_the_capability() {
    let (directory, store) = fenced_store();
    let mut connection = store.take_credential_connection().expect("connection");
    let _admission = admit(directory.path(), &mut connection);
    let input = Uuid::new_v4();
    seed(&mut connection, CredentialSlot::FleetApi, input, b"fence-refresh-lost").expect("pair");
    let expired = store.acquire_supervised_collector_lease(now_ms() - 86_400_000).expect("expiring lease");
    let other = HubStore::initialize(directory.path()).expect("second handle");
    let successor = other.acquire_supervised_collector_lease(now_ms()).expect("successor lease");
    assert!(matches!(
        store.heartbeat_supervised_collector_lease(expired, SupervisedCollectorState::Active, now_ms()),
        Err(StoreError::SupervisedCollectorLeaseLost)
    ));
    assert!(store.refresh_fence.slot().is_none(), "the lost lease is withdrawn");
    assert!(matches!(connection.begin_credential_refresh(CredentialSlot::FleetApi, input), Err(StoreError::SupervisedCollectorLeaseLost)));
    other.release_supervised_collector_lease(successor).expect("release");
}

#[test]
fn refresh_fence_begin_needs_the_process_lock() {
    let (_directory, store) = fenced_store();
    let mut connection = store.take_credential_connection().expect("connection");
    let input = Uuid::new_v4();
    seed(&mut connection, CredentialSlot::OwnerApi, input, b"fence-refresh-lock").expect("pair");
    let lease = store.acquire_supervised_collector_lease(now_ms()).expect("lease");
    assert!(matches!(
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, input),
        Err(StoreError::WriteRefused(REFRESH_FENCE_PROCESS_LOCK))
    ));
    assert_eq!(attempts(&connection), 0);
    store.release_supervised_collector_lease(lease).expect("release");
}

#[test]
fn refresh_fence_serialises_legacy_and_fleet() {
    let (directory, store) = fenced_store();
    let mut connection = store.take_credential_connection().expect("connection");
    let _admission = admit(directory.path(), &mut connection);
    let owner = Uuid::new_v4();
    let fleet = Uuid::new_v4();
    seed(&mut connection, CredentialSlot::OwnerApi, owner, b"fence-refresh-owner").expect("owner pair");
    seed(&mut connection, CredentialSlot::FleetApi, fleet, b"fence-refresh-fleet").expect("Fleet pair");
    let lease = store.acquire_supervised_collector_lease(now_ms()).expect("lease");
    let other_origin = |result: Result<OutboundRequestReceiptId, StoreError>| matches!(result, Err(StoreError::WriteRefused(REFRESH_FENCE_OTHER_ORIGIN)));

    let owner_receipt = connection.begin_credential_refresh(CredentialSlot::OwnerApi, owner).expect("Legacy begin");
    assert!(other_origin(connection.begin_credential_refresh(CredentialSlot::FleetApi, fleet)), "Fleet waits for the Legacy refresh");
    assert_eq!(attempts(&connection), 1);
    connection.cancel_unsent_credential_refresh(owner_receipt, owner).expect("Legacy resolved");

    let fleet_receipt = connection.begin_credential_refresh(CredentialSlot::FleetApi, fleet).expect("Fleet begin once Legacy is resolved");
    assert!(other_origin(connection.begin_credential_refresh(CredentialSlot::OwnerApi, owner)), "Legacy waits for the Fleet refresh");
    assert!(matches!(
        connection.begin_credential_refresh(CredentialSlot::FleetApi, fleet),
        Err(StoreError::RefreshOutcomeUnknown { reason: BeginRefusal::AttemptInFlight })
    ), "the own origin keeps its in-flight refusal");
    connection.cancel_unsent_credential_refresh(fleet_receipt, fleet).expect("Fleet resolved");
    assert_eq!(attempts(&connection), 0, "both admitted attempts were cancelled unsent; no refused begin added one");
    store.release_supervised_collector_lease(lease).expect("release");
}

#[test]
fn refresh_reuse_rejected_token_is_never_presented_again() {
    let (directory, store) = fenced_store();
    let mut connection = store.take_credential_connection().expect("connection");
    let _admission = admit(directory.path(), &mut connection);
    let lease = store.acquire_supervised_collector_lease(now_ms()).expect("lease");
    for (slot, refresh) in [(CredentialSlot::OwnerApi, &b"reuse-refresh-owner"[..]), (CredentialSlot::FleetApi, &b"reuse-refresh-fleet"[..])] {
        let input = Uuid::new_v4();
        seed(&mut connection, slot, input, refresh).expect("pair");
        let receipt = connection.begin_credential_refresh(slot, input).expect("begin");
        connection
            .close_credential_refresh_attempt(receipt, input, OutboundRequestOutcome::AuthenticationRejected, Some(401), None, false)
            .expect("rejected answer closed");
        assert!(!connection.has_unresolved_credential_refresh(slot).expect("resolved"));
        assert!(matches!(
            connection.begin_credential_refresh(slot, input),
            Err(StoreError::RefreshOutcomeUnknown { reason: BeginRefusal::TokenSpent })
        ), "{slot:?}: the rejected input is fenced");
        // The same refresh token offered again under a new generation is a reuse.
        assert!(matches!(
            seed(&mut connection, slot, Uuid::new_v4(), refresh),
            Err(StoreError::RefreshTokenSpent)
        ), "{slot:?}: a reused token is refused at write");
    }
    store.release_supervised_collector_lease(lease).expect("release");
}
