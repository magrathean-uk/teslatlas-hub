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
    as_put_pair(&store, Some(lc_gen(1)));
    (directory, store)
}

#[test]
fn lc1_a_closed_attempt_with_release_records_the_answer_and_frees_the_input() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
    })
    .expect("begin");
    assert!(
        s_test_with_credential_connection(&store, |connection| connection
            .has_unresolved_credential_refresh(CredentialSlot::OwnerApi))
        .expect("unresolved")
    );
    s_test_with_credential_connection(&store, |connection| {
        connection.close_credential_refresh_attempt(
            receipt,
            lc_gen(1),
            OutboundRequestOutcome::HttpError,
            Some(400),
            Some(7),
            true,
        )
    })
    .expect("close with release");
    assert_eq!(
        s_test_with_credential_connection(&store, |connection| {
            let mut statement = connection
                .connection
                .prepare(
                    "SELECT r.outcome, r.http_status, r.retry_after_seconds,
                        EXISTS(SELECT 1 FROM credential_refresh_attempts a
                               WHERE a.receipt_id = r.id AND a.fence_id IS NOT NULL)
                   FROM outbound_request_receipts r ORDER BY r.id",
                )
                .map_err(StoreError::CredentialRefreshReceipt)?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                })
                .map_err(StoreError::CredentialRefreshReceipt)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::CredentialRefreshReceipt)
        })
        .expect("receipts"),
        vec![("http_error".to_owned(), Some(400), Some(7), false)]
    );
    assert!(
        s_test_with_credential_connection(&store, |connection| {
            let mut statement = connection
                .connection
                .prepare(
                    "SELECT a.input_generation FROM credential_refresh_attempts a
                   JOIN credential_refresh_fences f ON f.fence_id = a.fence_id
                  WHERE a.slot = 'owner_api' ORDER BY a.receipt_id",
                )
                .map_err(StoreError::CredentialRefreshReceipt)?;
            let generations = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(StoreError::CredentialRefreshReceipt)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::CredentialRefreshReceipt)?;
            Ok::<Vec<Uuid>, StoreError>(
                generations
                    .into_iter()
                    .map(|value| {
                        Uuid::parse_str(&value).expect("schema-checked synthetic generation")
                    })
                    .collect(),
            )
        })
        .expect("fences")
        .is_empty()
    );
    assert!(
        !s_test_with_credential_connection(&store, |connection| connection
            .has_unresolved_credential_refresh(CredentialSlot::OwnerApi))
        .expect("unresolved")
    );
    // The input may be sent again: the kept reservation accepts it.
    let again = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
    })
    .expect("begin again");
    s_test_with_credential_connection(&store, |connection| {
        connection.cancel_unsent_credential_refresh(again, lc_gen(1))
    })
    .expect("cleanup");
}

#[test]
fn lc2_a_closed_attempt_without_release_keeps_the_input_fenced_for_ever() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
    })
    .expect("begin");
    s_test_with_credential_connection(&store, |connection| {
        connection.close_credential_refresh_attempt(
            receipt,
            lc_gen(1),
            OutboundRequestOutcome::AuthenticationRejected,
            Some(400),
            None,
            false,
        )
    })
    .expect("close without release");
    assert_eq!(
        s_test_with_credential_connection(&store, |connection| {
            let mut statement = connection
                .connection
                .prepare(
                    "SELECT r.outcome, r.http_status, r.retry_after_seconds,
                        EXISTS(SELECT 1 FROM credential_refresh_attempts a
                               WHERE a.receipt_id = r.id AND a.fence_id IS NOT NULL)
                   FROM outbound_request_receipts r ORDER BY r.id",
                )
                .map_err(StoreError::CredentialRefreshReceipt)?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                })
                .map_err(StoreError::CredentialRefreshReceipt)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::CredentialRefreshReceipt)
        })
        .expect("receipts"),
        vec![("authentication_rejected".to_owned(), Some(400), None, true)]
    );
    assert_eq!(
        s_test_with_credential_connection(&store, |connection| {
            let mut statement = connection
                .connection
                .prepare(
                    "SELECT a.input_generation FROM credential_refresh_attempts a
                   JOIN credential_refresh_fences f ON f.fence_id = a.fence_id
                  WHERE a.slot = 'owner_api' ORDER BY a.receipt_id",
                )
                .map_err(StoreError::CredentialRefreshReceipt)?;
            let generations = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(StoreError::CredentialRefreshReceipt)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::CredentialRefreshReceipt)?;
            Ok::<Vec<Uuid>, StoreError>(
                generations
                    .into_iter()
                    .map(|value| {
                        Uuid::parse_str(&value).expect("schema-checked synthetic generation")
                    })
                    .collect(),
            )
        })
        .expect("fences"),
        vec![lc_gen(1)]
    );
    // The attempt is closed, so it is not unresolved; the input is still dead.
    assert!(
        !s_test_with_credential_connection(&store, |connection| connection
            .has_unresolved_credential_refresh(CredentialSlot::OwnerApi))
        .expect("unresolved")
    );
    assert!(matches!(
        s_test_with_credential_connection(&store, |connection| connection
            .begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))),
        Err(StoreError::RefreshOutcomeUnknown { .. })
    ));
    // K4: the same pair cannot be imported again, a different one can.
    let same = s_test_with_credential_connection(&store, |connection| {
        s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(1),
            b"synthetic-access",
            format!("synthetic-refresh-{}", lc_gen(1)).as_bytes(),
            0,
            0,
        )
    })
    .expect("generation");
    assert!(matches!(
        s_test_with_credential_connection(&store,|connection|connection.replace_credential_pair(
            &same,&EntryProvenance::Supplied,&LegacyCandidates(Vec::new()),ReplacePolicy::ReplaceAny,
        )).map_err(|error|match crate::credentials::CredentialError::from(crate::auth::credential_vault::CredentialStoreError::from(error)){
            crate::credentials::CredentialError::LegacyOwnerTokenStore(error)=>error,
            _=>StoreError::WriteRefused("credential_vault_unavailable"),
        }),
        Err(StoreError::RefreshOutcomeUnknown { .. })
    ));
    let other = s_test_with_credential_connection(&store, |connection| {
        s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(2),
            b"synthetic-access",
            format!("synthetic-refresh-{}", lc_gen(2)).as_bytes(),
            0,
            0,
        )
    })
    .expect("generation");
    s_test_with_credential_connection(&store, |connection| {
        connection.replace_credential_pair(
            &other,
            &EntryProvenance::Supplied,
            &LegacyCandidates(Vec::new()),
            ReplacePolicy::ReplaceAny,
        )
    })
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
        let receipt = s_test_with_credential_connection(&store, |connection| {
            connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
        })
        .expect("begin");
        s_test_with_credential_connection(&store, |connection| {
            connection.close_credential_refresh_attempt(
                receipt,
                lc_gen(1),
                outcome,
                status,
                None,
                false,
            )
        })
        .unwrap_or_else(|error| panic!("{outcome:?} was refused: {error:?}"));
    }
    for outcome in [
        OutboundRequestOutcome::Cancelled,
        OutboundRequestOutcome::ProtocolError,
        OutboundRequestOutcome::ResponseTooLarge,
    ] {
        let (_directory, store) = lc_store_with_pair();
        let receipt = s_test_with_credential_connection(&store, |connection| {
            connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
        })
        .expect("begin");
        assert!(
            s_test_with_credential_connection(&store, |connection| connection
                .close_credential_refresh_attempt(receipt, lc_gen(1), outcome, None, None, false))
            .is_err(),
            "{outcome:?} must be refused"
        );
        // The refusal left the attempt as it was: still unresolved.
        assert!(
            s_test_with_credential_connection(&store, |connection| connection
                .has_unresolved_credential_refresh(CredentialSlot::OwnerApi))
            .expect("unresolved")
        );
    }
}

#[test]
fn lc4_only_a_started_attempt_bound_to_the_input_can_be_closed() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
    })
    .expect("begin");
    // Another input generation.
    assert!(matches!(
        s_test_with_credential_connection(&store, |connection| connection
            .close_credential_refresh_attempt(
                receipt,
                lc_gen(2),
                OutboundRequestOutcome::HttpError,
                Some(400),
                None,
                true
            )),
        Err(StoreError::RefreshOutcomeUnknown { .. })
    ));
    s_test_with_credential_connection(&store, |connection| {
        connection.close_credential_refresh_attempt(
            receipt,
            lc_gen(1),
            OutboundRequestOutcome::HttpError,
            Some(400),
            None,
            false,
        )
    })
    .expect("close");
    // Closing twice is refused: the receipt is no longer started, so the fence
    // that a release would delete cannot be freed by a second call.
    assert!(matches!(
        s_test_with_credential_connection(&store, |connection| connection
            .close_credential_refresh_attempt(
                receipt,
                lc_gen(1),
                OutboundRequestOutcome::HttpError,
                Some(400),
                None,
                true
            )),
        Err(StoreError::RefreshOutcomeUnknown { .. })
    ));
    assert_eq!(
        s_test_with_credential_connection(&store, |connection| {
            let mut statement = connection
                .connection
                .prepare(
                    "SELECT a.input_generation FROM credential_refresh_attempts a
                   JOIN credential_refresh_fences f ON f.fence_id = a.fence_id
                  WHERE a.slot = 'owner_api' ORDER BY a.receipt_id",
                )
                .map_err(StoreError::CredentialRefreshReceipt)?;
            let generations = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(StoreError::CredentialRefreshReceipt)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::CredentialRefreshReceipt)?;
            Ok::<Vec<Uuid>, StoreError>(
                generations
                    .into_iter()
                    .map(|value| {
                        Uuid::parse_str(&value).expect("schema-checked synthetic generation")
                    })
                    .collect(),
            )
        })
        .expect("fences"),
        vec![lc_gen(1)]
    );
    // A receipt that never existed.
    assert!(matches!(
        s_test_with_credential_connection(&store, |connection| connection
            .close_credential_refresh_attempt(
                OutboundRequestReceiptId(i64::MAX),
                lc_gen(1),
                OutboundRequestOutcome::HttpError,
                Some(400),
                None,
                true
            )),
        Err(StoreError::RefreshOutcomeUnknown { .. })
    ));
    // Out-of-range arguments are typed errors before any transaction.
    assert!(matches!(
        s_test_with_credential_connection(&store, |connection| connection
            .close_credential_refresh_attempt(
                receipt,
                lc_gen(1),
                OutboundRequestOutcome::HttpError,
                Some(99),
                None,
                true
            )),
        Err(StoreError::InvalidOutboundRequestHttpStatus)
    ));
    assert!(matches!(
        s_test_with_credential_connection(&store, |connection| connection
            .close_credential_refresh_attempt(
                receipt,
                lc_gen(1),
                OutboundRequestOutcome::ProtocolError,
                Some(400),
                None,
                true
            )),
        Err(StoreError::CredentialPairInvalid)
    ));
}

// ---------------------------------------------------------------------------
// `complete_unrotated_legacy_refresh` (C5 R45 row 3; Codex R03)
// ---------------------------------------------------------------------------



#[test]
fn lc5_an_unrotated_refresh_writes_the_pair_and_closes_the_receipt_in_one_step() {
    // X8's retained pre-C10 equal-generation component contract. No V1
    // manager or vault is constructed by this case.
    use crate::auth::legacy_auth::state_fixture as fixture;
    let (_directory,store)=as_store();
    fixture::write(&store,&fixture::input(lc_gen(1))).unwrap();
    let receipt=fixture::begin(&store,lc_gen(1)).expect("begin");
    let successor=fixture::row(Some(lc_gen(1)),vec![9;8],vec![0;8],(AS_T0+28_800,AS_T0+21_600));
    fixture::finish(&store,receipt,lc_gen(1),&successor).expect("complete");
    let receipts:Vec<(String,Option<i64>,Option<i64>,bool)> = {
        let db=store.open().unwrap();
        let mut query=db.prepare("SELECT r.outcome,r.http_status,r.retry_after_seconds,EXISTS(SELECT 1 FROM legacy_refresh_input_fences f JOIN legacy_refresh_receipt_bindings b ON b.input_credential_generation=f.input_credential_generation WHERE b.receipt_id=r.id) FROM outbound_request_receipts r ORDER BY r.id").unwrap();
        query.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap().collect::<Result<_,_>>().unwrap()
    };
    assert_eq!(receipts,vec![("success".to_owned(),Some(200),None,false)]);
    let fences:i64=store.open().unwrap().query_row("SELECT count(*) FROM legacy_refresh_input_fences",[],|r|r.get(0)).unwrap();
    assert_eq!(fences,0);
    assert!(!fixture::unresolved(&store).expect("unresolved"));
    let stored=fixture::load(&store).expect("load").expect("a row");
    assert_eq!(stored.0.as_slice(),&[9;8]);
    assert_eq!(stored.4,Some(lc_gen(1)));
    assert_eq!(stored.2,AS_T0+28_800);
    let again=fixture::begin(&store,lc_gen(1)).expect("begin again");
    fixture::finish(&store,again,lc_gen(1),&successor).expect("cleanup");
}

#[test]
fn lc6_an_external_write_that_came_first_is_never_overwritten_by_the_older_refresh() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
    })
    .expect("begin");
    as_put_pair(&store, Some(lc_gen(9)));
    s_test_with_credential_connection(&store, |connection| {
        let successor = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(2),
            b"access-successor",
            b"refresh-successor",
            AS_T0 + 28_800,
            AS_T0 + 21_600,
        )
        .expect("successor");
        assert!(matches!(
            connection.complete_credential_refresh(
                receipt,
                lc_gen(1),
                &successor,
                &LegacyCandidates(Vec::new()),
            ),
            Err(StoreError::RefreshOutcomeUnknown {
                reason: BeginRefusal::RowChanged
            })
        ));
        let stored = connection
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap();
        let keys = crate::auth::credential_keys::KeyringHandle::new(
            crate::auth::credential_keys::Keyring {
                current: crate::auth::credential_keys::KeyMaterial {
                    id: crate::auth::credential_envelope::KeyId([0x53; 8]),
                    created_at_s: 1_800_000_000,
                    key: Zeroizing::new([0x53; 32]),
                },
                previous: None,
            },
        );
        let access = stored
            .open(&keys)
            .map(|(access, _refresh)| access.to_vec())
            .map_err(|_| "stored_access_open_failed");
        assert_eq!(
            (stored.generation, access),
            (lc_gen(9), Ok(b"synthetic-access".to_vec())),
            "INTENT-LC6-REPLACEMENT: refused older completion preserves the replacement generation and access material"
        );
    });
}

#[test]
fn lc7_a_row_that_changed_under_a_started_attempt_leaves_the_receipt_started() {
    let (_directory, store) = lc_store_with_pair();
    s_test_with_credential_connection(&store, |connection| {
        let receipt = connection
            .begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
            .expect("begin");
        let input = connection
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap();
        let fence_id: i64 = connection
            .connection
            .query_row(
                "SELECT fence_id FROM credential_refresh_attempts WHERE receipt_id = ?1",
                [receipt.0],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .connection
            .execute(
                "UPDATE credential_pairs SET generation = ?1 WHERE slot = 'owner_api'",
                params![lc_gen(9).to_string()],
            )
            .unwrap();
        let successor = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(2),
            b"access-successor",
            b"refresh-successor",
            AS_T0 + 28_800,
            AS_T0 + 21_600,
        )
        .expect("successor");
        assert!(matches!(
            connection.complete_credential_refresh(
                receipt,
                lc_gen(1),
                &successor,
                &LegacyCandidates(Vec::new()),
            ),
            Err(StoreError::RefreshOutcomeUnknown {
                reason: BeginRefusal::RowChanged
            })
        ));
        let state = connection
            .connection
            .query_row(
                "SELECT p.access_envelope, r.outcome, r.http_status,
                        r.retry_after_seconds, a.fence_id, f.fingerprint, f.kind
                   FROM credential_pairs p
                   JOIN outbound_request_receipts r ON r.id = ?1
                   LEFT JOIN credential_refresh_attempts a ON a.receipt_id = r.id
                   LEFT JOIN credential_refresh_fences f ON f.fence_id = a.fence_id
                  WHERE p.slot = 'owner_api'",
                [receipt.0],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<Vec<u8>>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            state,
            (
                input.access_envelope,
                "started".to_owned(),
                None,
                None,
                Some(fence_id),
                input.refresh_fingerprint.map(|value| value.to_vec()),
                Some("attempt".to_owned()),
            ),
            "INTENT-LC7-UNCHANGED: refused row-change completion preserves access bytes, started receipt, absent HTTP/retry and the same input fence"
        );
    });
}

#[test]
fn lc8_a_pair_of_another_generation_or_a_stranger_receipt_is_refused() {
    let (_directory, store) = lc_store_with_pair();
    s_test_with_credential_connection(&store, |connection| {
        let receipt = connection
            .begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
            .expect("begin");
        let input = connection
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap();
        let fence_id: i64 = connection
            .connection
            .query_row(
                "SELECT fence_id FROM credential_refresh_attempts WHERE receipt_id = ?1",
                [receipt.0],
                |row| row.get(0),
            )
            .unwrap();
        let receipt_state = |connection: &CredentialConnection| {
            connection
                .connection
                .query_row(
                    "SELECT r.outcome, r.http_status, r.retry_after_seconds,
                            a.fence_id, f.fingerprint, f.kind
                       FROM outbound_request_receipts r
                       LEFT JOIN credential_refresh_attempts a ON a.receipt_id = r.id
                       LEFT JOIN credential_refresh_fences f ON f.fence_id = a.fence_id
                      WHERE r.id = ?1",
                    [receipt.0],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, Option<Vec<u8>>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                        ))
                    },
                )
                .unwrap()
        };
        let successor = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(2),
            b"access-successor",
            b"refresh-successor",
            AS_T0 + 28_800,
            AS_T0 + 21_600,
        )
        .expect("successor");
        // R31 step 2 replaces the old mechanism with a successor generation
        // different from the input; a valid envelope for the input is invalid here.
        let invalid_successor = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(1),
            b"access-invalid-successor",
            b"refresh-invalid-successor",
            AS_T0 + 28_800,
            AS_T0 + 21_600,
        )
        .expect("sealed input-generation successor");
        assert!(
            matches!(
                connection.complete_credential_refresh(
                    receipt,
                    lc_gen(1),
                    &invalid_successor,
                    &LegacyCandidates(Vec::new()),
                ),
                Err(StoreError::RefreshOutcomeUnknown {
                    reason: BeginRefusal::RowChanged
                })
            ),
            "INTENT-LC8-SUCCESSOR: C10 refuses a successor that reuses the input generation"
        );
        let mut refused_states = vec![receipt_state(connection)];
        assert!(matches!(
            connection.complete_credential_refresh(
                receipt,
                lc_gen(9),
                &successor,
                &LegacyCandidates(Vec::new()),
            ),
            Err(StoreError::RefreshOutcomeUnknown {
                reason: BeginRefusal::RowChanged
            })
        ));
        refused_states.push(receipt_state(connection));
        assert!(matches!(
            connection.complete_credential_refresh(
                OutboundRequestReceiptId(i64::MAX),
                lc_gen(1),
                &successor,
                &LegacyCandidates(Vec::new()),
            ),
            Err(StoreError::RefreshOutcomeUnknown {
                reason: BeginRefusal::RowChanged
            })
        ));
        refused_states.push(receipt_state(connection));
        let expected_state = (
            "started".to_owned(),
            None,
            None,
            Some(fence_id),
            input.refresh_fingerprint.map(|value| value.to_vec()),
            Some("attempt".to_owned()),
        );
        assert_eq!(
            refused_states,
            vec![expected_state; 3],
            "INTENT-LC8-UNCHANGED: invalid successor, wrong input and stranger receipt preserve started receipt, absent HTTP/retry and the same input fence"
        );
        assert!(
            connection
                .has_unresolved_credential_refresh(CredentialSlot::OwnerApi)
                .unwrap()
        );
    });
}
