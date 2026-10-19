// SPDX-License-Identifier: AGPL-3.0-only

// Tests of `close_legacy_refresh_attempt` (C5 R51, decision D1; spec test 88 at
// the store). Synthetic pairs in a temporary store; no network. The attempt
// that uses it is tested with the manager in `auth/credentials/refresh_tests.rs`.

fn lc_gen(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

/// A store holding a pair of generation `lc_gen(1)`.
fn lc_store_with_pair() -> (tempfile::TempDir, HubStore) {
    let (directory, store) = cs_store();
    let pair = TeslaMateLegacyTokenStore::imported(vec![1; 8], vec![2; 8])
        .expect("placeholder pair")
        .with_credential_generation(lc_gen(1))
        .expect("generation");
    store
        .replace_teslamate_legacy_tokens(&pair)
        .expect("store pair");
    (directory, store)
}

#[test]
fn lc1_a_closed_attempt_with_release_records_the_answer_and_frees_the_input() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = store.begin_legacy_refresh(lc_gen(1)).expect("begin");
    assert!(store.has_unresolved_legacy_refresh().expect("unresolved"));
    store
        .close_legacy_refresh_attempt(
            receipt,
            lc_gen(1),
            OutboundRequestOutcome::HttpError,
            Some(400),
            Some(7),
            true,
        )
        .expect("close with release");
    assert_eq!(
        store.legacy_token_receipts_for_test().expect("receipts"),
        vec![("http_error".to_owned(), Some(400), Some(7), false)]
    );
    assert!(
        store
            .legacy_fenced_inputs_for_test()
            .expect("fences")
            .is_empty()
    );
    assert!(!store.has_unresolved_legacy_refresh().expect("unresolved"));
    // The input may be sent again: the kept reservation accepts it.
    let again = store.begin_legacy_refresh(lc_gen(1)).expect("begin again");
    store
        .cancel_unsent_legacy_refresh(again, lc_gen(1))
        .expect("cleanup");
}

#[test]
fn lc2_a_closed_attempt_without_release_keeps_the_input_fenced_for_ever() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = store.begin_legacy_refresh(lc_gen(1)).expect("begin");
    store
        .close_legacy_refresh_attempt(
            receipt,
            lc_gen(1),
            OutboundRequestOutcome::AuthenticationRejected,
            Some(400),
            None,
            false,
        )
        .expect("close without release");
    assert_eq!(
        store.legacy_token_receipts_for_test().expect("receipts"),
        vec![("authentication_rejected".to_owned(), Some(400), None, true)]
    );
    assert_eq!(
        store.legacy_fenced_inputs_for_test().expect("fences"),
        vec![lc_gen(1)]
    );
    // The attempt is closed, so it is not unresolved; the input is still dead.
    assert!(!store.has_unresolved_legacy_refresh().expect("unresolved"));
    assert!(matches!(
        store.begin_legacy_refresh(lc_gen(1)),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    // K4: the same pair cannot be imported again, a different one can.
    let same = TeslaMateLegacyTokenStore::imported(vec![1; 8], vec![2; 8])
        .expect("same pair")
        .with_credential_generation(lc_gen(1))
        .expect("generation");
    assert!(matches!(
        store.replace_teslamate_legacy_tokens(&same),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    let other = TeslaMateLegacyTokenStore::imported(vec![3; 8], vec![4; 8])
        .expect("other pair")
        .with_credential_generation(lc_gen(2))
        .expect("generation");
    store
        .replace_teslamate_legacy_tokens(&other)
        .expect("new login");
}

#[test]
fn lc3_the_five_outcomes_of_a_sent_request_are_accepted_and_no_other() {
    let accepted = [
        (OutboundRequestOutcome::TransportError, None),
        (OutboundRequestOutcome::Timeout, None),
        (OutboundRequestOutcome::HttpError, Some(503)),
        (OutboundRequestOutcome::AuthenticationRejected, Some(401)),
        (OutboundRequestOutcome::Success, Some(200)),
    ];
    for (outcome, status) in accepted {
        let (_directory, store) = lc_store_with_pair();
        let receipt = store.begin_legacy_refresh(lc_gen(1)).expect("begin");
        store
            .close_legacy_refresh_attempt(receipt, lc_gen(1), outcome, status, None, true)
            .unwrap_or_else(|error| panic!("{outcome:?} was refused: {error:?}"));
    }
    for outcome in [
        OutboundRequestOutcome::Cancelled,
        OutboundRequestOutcome::ProtocolError,
        OutboundRequestOutcome::ResponseTooLarge,
    ] {
        let (_directory, store) = lc_store_with_pair();
        let receipt = store.begin_legacy_refresh(lc_gen(1)).expect("begin");
        assert!(
            store
                .close_legacy_refresh_attempt(receipt, lc_gen(1), outcome, None, None, true)
                .is_err(),
            "{outcome:?} must be refused"
        );
        // The refusal left the attempt as it was: still unresolved.
        assert!(store.has_unresolved_legacy_refresh().expect("unresolved"));
    }
}

#[test]
fn lc4_only_a_started_attempt_bound_to_the_input_can_be_closed() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = store.begin_legacy_refresh(lc_gen(1)).expect("begin");
    // Another input generation.
    assert!(matches!(
        store.close_legacy_refresh_attempt(
            receipt,
            lc_gen(9),
            OutboundRequestOutcome::HttpError,
            Some(500),
            None,
            true,
        ),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    store
        .close_legacy_refresh_attempt(
            receipt,
            lc_gen(1),
            OutboundRequestOutcome::HttpError,
            Some(500),
            None,
            false,
        )
        .expect("close");
    // Closing twice is refused: the receipt is no longer started, so the fence
    // that a release would delete cannot be freed by a second call.
    assert!(matches!(
        store.close_legacy_refresh_attempt(
            receipt,
            lc_gen(1),
            OutboundRequestOutcome::HttpError,
            Some(500),
            None,
            true,
        ),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    assert_eq!(
        store.legacy_fenced_inputs_for_test().expect("fences"),
        vec![lc_gen(1)]
    );
    // A receipt that never existed.
    assert!(matches!(
        store.close_legacy_refresh_attempt(
            OutboundRequestReceiptId(9_999),
            lc_gen(1),
            OutboundRequestOutcome::HttpError,
            Some(500),
            None,
            true,
        ),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    // Out-of-range arguments are typed errors before any transaction.
    assert!(matches!(
        store.close_legacy_refresh_attempt(
            receipt,
            lc_gen(1),
            OutboundRequestOutcome::HttpError,
            Some(99),
            None,
            true,
        ),
        Err(StoreError::InvalidOutboundRequestHttpStatus)
    ));
    assert!(matches!(
        store.close_legacy_refresh_attempt(
            OutboundRequestReceiptId(0),
            lc_gen(1),
            OutboundRequestOutcome::HttpError,
            Some(500),
            None,
            true,
        ),
        Err(StoreError::InvalidLegacyRefreshGeneration)
    ));
}

// ---------------------------------------------------------------------------
// `complete_unrotated_legacy_refresh` (C5 R45 row 3; Codex R03)
// ---------------------------------------------------------------------------

/// The pair a refresh that kept its refresh token writes: a new access token, the
/// same generation.
fn lc_unrotated(generation: Uuid) -> TeslaMateLegacyTokenStore {
    TeslaMateLegacyTokenStore::refreshed(vec![9; 8], vec![2; 8], AS_T0 + 28_800, AS_T0 + 21_600)
        .expect("refreshed pair")
        .with_credential_generation(generation)
        .expect("generation")
}

#[test]
fn lc5_an_unrotated_refresh_writes_the_pair_and_closes_the_receipt_in_one_step() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = store.begin_legacy_refresh(lc_gen(1)).expect("begin");
    store
        .complete_unrotated_legacy_refresh(receipt, lc_gen(1), &lc_unrotated(lc_gen(1)))
        .expect("complete");
    // The receipt is a success with the input released: the refresh token was not
    // consumed, so the next attempt may present it again.
    assert_eq!(
        store.legacy_token_receipts_for_test().expect("receipts"),
        vec![("success".to_owned(), Some(200), None, false)]
    );
    assert!(store.legacy_fenced_inputs_for_test().expect("fences").is_empty());
    assert!(!store.has_unresolved_legacy_refresh().expect("unresolved"));
    let stored = store
        .load_teslamate_legacy_tokens()
        .expect("load")
        .expect("a row");
    assert_eq!(stored.access(), &[9; 8]);
    assert_eq!(stored.credential_generation(), Some(lc_gen(1)));
    assert_eq!(stored.expires_at(), AS_T0 + 28_800);
    let again = store.begin_legacy_refresh(lc_gen(1)).expect("begin again");
    store
        .cancel_unsent_legacy_refresh(again, lc_gen(1))
        .expect("cleanup");
}

#[test]
fn lc6_an_external_write_that_came_first_is_never_overwritten_by_the_older_refresh() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = store.begin_legacy_refresh(lc_gen(1)).expect("begin");
    // The owner enters a new login while the request is in flight. The kept
    // writer resolves the started attempt (K4) and stores its pair.
    let replacement = TeslaMateLegacyTokenStore::imported(vec![7; 8], vec![8; 8])
        .expect("replacement")
        .with_credential_generation(lc_gen(2))
        .expect("generation");
    store
        .replace_teslamate_legacy_tokens(&replacement)
        .expect("external write");
    // The older refresh finds its attempt closed and the row changed: nothing is
    // written, and the owner's pair stays.
    assert!(matches!(
        store.complete_unrotated_legacy_refresh(receipt, lc_gen(1), &lc_unrotated(lc_gen(1))),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    let stored = store
        .load_teslamate_legacy_tokens()
        .expect("load")
        .expect("a row");
    assert_eq!(stored.credential_generation(), Some(lc_gen(2)));
    assert_eq!(stored.access(), &[7; 8]);
}

#[test]
fn lc7_a_row_that_changed_under_a_started_attempt_leaves_the_receipt_started() {
    // The write and the close are one transaction: when the row does not hold the
    // input any more, the receipt is not closed either, so nothing is half done.
    let (_directory, store) = lc_store_with_pair();
    let receipt = store.begin_legacy_refresh(lc_gen(1)).expect("begin");
    store
        .open()
        .expect("connection")
        .execute(
            "UPDATE teslamate_legacy_tokens SET credential_generation = ?1",
            params![lc_gen(3).to_string()],
        )
        .expect("a row written by hand");
    assert!(matches!(
        store.complete_unrotated_legacy_refresh(receipt, lc_gen(1), &lc_unrotated(lc_gen(1))),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    assert_eq!(
        store.legacy_token_receipts_for_test().expect("receipts"),
        vec![("started".to_owned(), None, None, true)]
    );
    assert_eq!(
        store
            .load_teslamate_legacy_tokens()
            .expect("load")
            .expect("a row")
            .access(),
        &[1; 8],
        "the old access token is untouched"
    );
}

#[test]
fn lc8_a_pair_of_another_generation_or_a_stranger_receipt_is_refused() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = store.begin_legacy_refresh(lc_gen(1)).expect("begin");
    // The pair must carry the generation of the input.
    assert!(matches!(
        store.complete_unrotated_legacy_refresh(receipt, lc_gen(1), &lc_unrotated(lc_gen(2))),
        Err(StoreError::InvalidLegacyRefreshGeneration)
    ));
    // A receipt that is bound to another input is not this attempt's.
    assert!(matches!(
        store.complete_unrotated_legacy_refresh(receipt, lc_gen(5), &lc_unrotated(lc_gen(5))),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    assert_eq!(
        store.legacy_token_receipts_for_test().expect("receipts"),
        vec![("started".to_owned(), None, None, true)]
    );
}
