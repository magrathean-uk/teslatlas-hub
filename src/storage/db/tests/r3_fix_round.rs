// SPDX-License-Identifier: AGPL-3.0-only

// Tests of the B2 round-3 fix round (coordinator rulings of 2026-10-15) for the
// catalogue store. Synthetic data only, in a temporary store; no network.

fn r3_take_credential_commit_pragmas() -> Vec<(i64, i64)> {
    CREDENTIAL_COMMIT_PRAGMAS.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

#[cfg(target_os = "macos")]
#[test]
fn c5_r3_03_the_credential_commit_connections_ask_for_a_full_flush_on_macos() {
    // A plain fsync does not flush the drive's cache on macOS. The commits that record
    // that a refresh token was presented, and what came back, run on connections that
    // ask SQLite for F_FULLFSYNC at commit and at checkpoint.
    let (_temporary, store) = as_store();
    let g0 = as_gen(10);
    let g1 = as_gen(11);
    as_put_pair(&store, Some(g0));
    let _ = r3_take_credential_commit_pragmas();
    as_refresh(&store, g0, g1);
    let seen = r3_take_credential_commit_pragmas();
    assert_eq!(seen, vec![(1, 1), (1, 1)], "Legacy begin and complete");

    let f0 = Uuid::from_u128(201);
    let f1 = Uuid::from_u128(202);
    let pair = |generation: Uuid, expires: i64| {
        s_test_with_credential_connection(&store, |connection| {
            s_test_synthetic_pair(
                connection,
                CredentialSlot::FleetApi,
                generation,
                b"r3-fleet-access",
                format!("r3-fleet-refresh-{generation}").as_bytes(),
                expires,
                expires - 1,
            )
        })
        .expect("Fleet tokens")
    };
    {
        let input_pair = pair(f0, AS_T0 + 28_800);
        s_test_with_credential_connection(&store, |connection| {
            connection.replace_credential_pair(
                &input_pair,
                &EntryProvenance::Supplied,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny,
            )
        })
    }
        .expect("input pair");
    let _ = r3_take_credential_commit_pragmas();
    let receipt = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::FleetApi, f0)
    })
    .expect("begin");
    {
        let successor = pair(f1, AS_T0 + 57_600);
        s_test_with_credential_connection(&store, |connection| {
            connection.complete_credential_refresh(
                receipt,
                f0,
                &successor,
                &LegacyCandidates(Vec::new()),
            )
        })
    }
        .expect("complete");
    let seen = r3_take_credential_commit_pragmas();
    assert_eq!(seen, vec![(1, 1), (1, 1)], "Fleet begin and complete");

    // The ordinary connection keeps the default at commit: the per-commit flush is for
    // these commits only. It flushes at checkpoint, like every writable connection (B2
    // apply, R3C-02): the checkpoint that copies a credential commit's frames into the
    // database file is usually run by an ordinary connection.
    let ordinary = store.open().expect("connection");
    let fullfsync: i64 = ordinary
        .query_row("PRAGMA fullfsync", [], |row| row.get(0))
        .expect("pragma");
    assert_eq!(fullfsync, 0);
    let checkpoint: i64 = ordinary
        .query_row("PRAGMA checkpoint_fullfsync", [], |row| row.get(0))
        .expect("pragma");
    assert_eq!(
        checkpoint, 1,
        "an ordinary writable connection flushes at checkpoint"
    );
}

#[test]
fn sec_r3_n2_the_debug_text_of_an_auth_row_shows_whether_a_generation_is_present_not_the_generation()
 {
    // A generation is a digest that identifies a refresh token (R53): it is never
    // formatted, in the row or in the effective status that wraps it.
    let stopped = Uuid::from_u128(0x5ec0_0000_0000_0000_0000_0000_0000_0001);
    let fused = Uuid::from_u128(0x5ec0_0000_0000_0000_0000_0000_0000_0002);
    let mut row = CollectorAuthRow {
        state: "signed_out".to_owned(),
        reason: Some("access_token_rejected".to_owned()),
        collecting: false,
        retry_at: None,
        stopped_generation: Some(stopped),
        stopped_mode: Some("legacy".to_owned()),
        fuse_consecutive: 3,
        fuse_generation: Some(fused),
        fuse_last_rejection_at: Some(1_800_000_000),
        fuse_pause_until: None,
        fuse_rejected_access_digest: None,
        clock_skew: false,
    };
    let shown = format!("{row:?} {:#?}", row.clone());
    for secret in [stopped.to_string(), fused.to_string()] {
        assert!(!shown.contains(&secret), "{shown}");
        assert!(
            !shown.contains(&secret.replace('-', "")),
            "no simple form either: {shown}"
        );
    }
    assert!(shown.contains("signed_out"), "{shown}");
    assert!(shown.contains("has_stopped_generation: true"), "{shown}");
    assert!(shown.contains("has_fuse_generation: true"), "{shown}");
    row.stopped_generation = None;
    row.fuse_generation = None;
    let shown = format!("{row:?}");
    assert!(shown.contains("has_stopped_generation: false"), "{shown}");
    assert!(shown.contains("has_fuse_generation: false"), "{shown}");
    // Equality still compares the generations themselves.
    let mut other = row.clone();
    other.fuse_generation = Some(fused);
    assert_ne!(row, other);
}

// ---- Hub fix pass 3: the digest of the rejected access token in the credential status ---

/// A count-1 row of a managed unit, with the digest of the token it rejected.
fn f3_counted_row() -> CollectorAuthRow {
    CollectorAuthRow {
        fuse_consecutive: 1,
        fuse_generation: Some(lc_gen(1)),
        fuse_last_rejection_at: Some(AS_T0),
        fuse_rejected_access_digest: Some(as_gen(77)),
        ..as_row("ready", None, true)
    }
}

#[test]
fn f3_the_rejected_access_digest_is_stored_and_no_refresh_rewrites_it() {
    let (_directory, store) = lc_store_with_pair();
    let counted = f3_counted_row();
    assert!(
        store
            .write_collector_auth_row(&counted, AS_T0 * 1_000)
            .unwrap()
    );
    assert_eq!(
        store.load_collector_auth_row().unwrap(),
        Some(counted.clone())
    );
    assert!(
        !store
            .write_collector_auth_row(&counted, AS_T0 * 1_000 + 1)
            .unwrap(),
        "an unchanged row costs no write"
    );
    // The debug text of the row shows whether the digest is present, never the digest.
    let shown = format!("{counted:?}");
    assert!(
        shown.contains("has_fuse_rejected_access_digest: true"),
        "{shown}"
    );
    assert!(!shown.contains(&as_gen(77).to_string()), "{shown}");

    // The decision is derived from the stored pair, so no refresh writes the status row:
    // an answer that renewed nothing, a cancelled attempt, an unrotated completion and a
    // rotation leave the row exactly as it was.
    let receipt = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
    })
    .expect("begin");
    s_test_with_credential_connection(&store, |connection| {
        connection.close_credential_refresh_attempt(
            receipt,
            lc_gen(1),
            OutboundRequestOutcome::Success,
            Some(200),
            None,
            true,
        )
    })
        .expect("close as a success with release");
    let receipt = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
    })
    .expect("begin");
    s_test_with_credential_connection(&store, |connection| {
        connection.cancel_unsent_credential_refresh(receipt, lc_gen(1))
    })
        .expect("cancel");
    let receipt=s_test_with_credential_connection(&store,|connection|connection.begin_credential_refresh(CredentialSlot::OwnerApi,lc_gen(1))).expect("begin");
    s_test_with_credential_connection(&store,|connection|{
        let successor=s_test_synthetic_pair(connection,CredentialSlot::OwnerApi,as_gen(6),b"synthetic-unrotated-access",format!("synthetic-refresh-{}",lc_gen(1)).as_bytes(),AS_T0+28_800,AS_T0+21_600)?;
        connection.complete_credential_refresh(receipt,lc_gen(1),&successor,&LegacyCandidates(Vec::new()))
    }).expect("complete");
    assert_eq!(
        store.load_collector_auth_row().unwrap(),
        Some(counted.clone())
    );
    as_refresh(&store, as_gen(6), as_gen(7));
    assert_eq!(store.load_collector_auth_row().unwrap(), Some(counted));

    // The column holds a 36-character digest or nothing.
    let connection = store.open().unwrap();
    assert!(
        connection
            .execute(
                "UPDATE collector_auth_state SET fuse_rejected_access_digest = 'short'",
                []
            )
            .is_err()
    );
}

// ---- Hub fix pass 4: the fill of a missing digest is one conditional, durable write ------

#[test]
fn f4_the_fill_of_a_missing_digest_is_conditional_on_the_row_the_fuse_and_the_pair() {
    let (_directory, store) = lc_store_with_pair();
    let missing = CollectorAuthRow {
        fuse_rejected_access_digest: None,
        ..f3_counted_row()
    };
    assert!(
        store
            .write_collector_auth_row(&missing, AS_T0 * 1_000)
            .unwrap()
    );
    let pair = s_test_with_credential_connection(&store, |connection| {
        connection.load_credential_pair(CredentialSlot::OwnerApi)
    })
        .unwrap()
        .expect("the pair row");
    let digest = as_gen(78);

    // Another fuse (count or generation), or another stored pair: nothing is written.
    assert!(
        s_test_with_credential_connection(&store,|connection|connection.fill_credential_rejected_access_digest(pair.generation,digest,Some(lc_gen(1)),2))
            .is_err()
    );
    assert!(
        s_test_with_credential_connection(&store,|connection|connection.fill_credential_rejected_access_digest(pair.generation,digest,Some(as_gen(99)),1))
            .is_err()
    );
    assert!(
        s_test_with_credential_connection(&store,|connection|connection.fill_credential_rejected_access_digest(as_gen(98),digest,Some(lc_gen(1)),1))
            .is_err()
    );
    let other_pair =s_test_with_credential_connection(&store,|connection|s_test_synthetic_pair(connection,CredentialSlot::OwnerApi,as_gen(97),b"another-access",b"another-refresh",0,0))
        .unwrap();
    assert!(
        s_test_with_credential_connection(&store,|connection|connection.fill_credential_rejected_access_digest(other_pair.generation,digest,Some(lc_gen(1)),1))
            .is_err()
    );
    assert_eq!(
        store.load_collector_auth_row().unwrap(),
        Some(missing.clone())
    );

    // The matching call fills the digest and nothing else.
    s_test_with_credential_connection(&store,|connection|connection.fill_credential_rejected_access_digest(pair.generation,digest,Some(lc_gen(1)),1))
        .expect("the fill");
    assert_eq!(
        store.load_collector_auth_row().unwrap(),
        Some(CollectorAuthRow {
            fuse_rejected_access_digest: Some(digest),
            ..missing
        })
    );
    // A digest that is present is never replaced by a fill.
    assert!(
        s_test_with_credential_connection(&store,|connection|connection.fill_credential_rejected_access_digest(pair.generation,as_gen(79),Some(lc_gen(1)),1))
            .is_err()
    );
    assert_eq!(
        store
            .load_collector_auth_row()
            .unwrap()
            .and_then(|row| row.fuse_rejected_access_digest),
        Some(digest)
    );
}
