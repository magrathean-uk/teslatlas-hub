// SPDX-License-Identifier: AGPL-3.0-only

//! C2-C store-level tests (C2 section 18.2, C2 R80 and R81; C2 T85, C2 T86 and
//! C2 T90, with the writer's own report): every import-watermark domain is
//! written by one raise-only merge, so a seed or a promotion whose pair is
//! absent or not above the stored pair leaves the stored pair, a repeat of
//! such a seed is a no-op, and a session imported open carries the kind and
//! type the shared fold gives. C2 T87 is a review check in the receipt. The
//! snapshot S and its helpers are T69's (`seed_noop.rs`). Synthetic data only.

use super::*;

// The importer's snapshot types, named once here.
type ImportedWatermark = crate::teslamate_projection::TeslaMateSourceWatermark;
type ImportedOpenSession = crate::teslamate_projection::TeslaMateOpenSession;
type ImportedCharge = crate::teslamate_projection::TeslaMateCharge;

const T_HI: i64 = T69_T0 - 10 * T69_MIN;
const T_MID: i64 = T69_T0 - 15 * T69_MIN;
const T_LO: i64 = T69_T0 - 20 * T69_MIN;

fn pair(id: Option<i64>, timestamp_ms: Option<i64>) -> WatermarkPair {
    WatermarkPair {
        max_source_row_id: id,
        max_timestamp_ms: timestamp_ms,
    }
}

#[test]
fn c2_t85_watermark_merge() {
    // (stored, incoming, result) of C2 T85's table.
    let cases = [
        (
            pair(Some(500), Some(T_HI)),
            pair(Some(300), Some(T_LO)),
            pair(Some(500), Some(T_HI)),
        ),
        (
            pair(Some(500), Some(T_HI)),
            pair(None, None),
            pair(Some(500), Some(T_HI)),
        ),
        (
            pair(Some(500), Some(T_HI)),
            pair(Some(800), Some(T_MID)),
            pair(Some(800), Some(T_HI)),
        ),
        (
            pair(Some(500), Some(T_HI)),
            pair(Some(500), Some(T_HI)),
            pair(Some(500), Some(T_HI)),
        ),
        (
            pair(None, None),
            pair(Some(300), Some(T_LO)),
            pair(Some(300), Some(T_LO)),
        ),
        (
            pair(None, Some(T_HI)),
            pair(Some(300), None),
            pair(Some(300), Some(T_HI)),
        ),
        (
            pair(Some(500), None),
            pair(None, Some(T_LO)),
            pair(Some(500), Some(T_LO)),
        ),
        (pair(None, None), pair(None, None), pair(None, None)),
    ];
    for (stored, incoming, want) in cases {
        assert_eq!(
            merge_watermark(stored, incoming),
            want,
            "{stored:?} + {incoming:?}"
        );
        // Repeating the incoming pair changes nothing (idempotent), and the order
        // does not matter (commutative).
        assert_eq!(
            merge_watermark(merge_watermark(stored, incoming), incoming),
            want,
            "{stored:?} + {incoming:?} twice"
        );
        assert_eq!(
            merge_watermark(incoming, stored),
            want,
            "{incoming:?} + {stored:?}"
        );
    }
}

/// Snapshot S of T69 with watermark domain `domain` set to `p`; the other five
/// pairs are S's (`charging_processes` (50, T_hi), `charges` (102, T_hi), the
/// rest (None, None)).
fn s_d(domain: WatermarkDomain, p: WatermarkPair) -> ImportedOpenSession {
    let mut session = t69_s((Some(50), Some(T_HI)));
    let value = ImportedWatermark::from(p);
    let watermarks = &mut session.watermarks;
    match domain {
        WatermarkDomain::Drives => watermarks.drives = value,
        WatermarkDomain::Positions => watermarks.positions = value,
        WatermarkDomain::ChargingProcesses => watermarks.charging_processes = value,
        WatermarkDomain::Charges => watermarks.charges = value,
        WatermarkDomain::States => watermarks.states = value,
        WatermarkDomain::Updates => watermarks.updates = value,
    }
    session
}

/// S's own pair of a domain.
fn s_pair(domain: WatermarkDomain) -> WatermarkPair {
    match domain {
        WatermarkDomain::ChargingProcesses => pair(Some(50), Some(T_HI)),
        WatermarkDomain::Charges => pair(Some(102), Some(T_HI)),
        _ => pair(None, None),
    }
}

/// Every stored watermark row of the vehicle, by domain.
fn stored_pairs(
    store: &HubStore,
    vehicle: Uuid,
) -> std::collections::BTreeMap<String, WatermarkPair> {
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(
            "SELECT domain, max_source_row_id, max_timestamp_ms
             FROM lifecycle_source_watermarks WHERE vehicle_id = ?1",
        )
        .expect("statement");
    statement
        .query_map(params![vehicle.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, pair(row.get(1)?, row.get(2)?)))
        })
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("collect")
}

fn promote(
    store: &HubStore,
    source: &SourceRecord,
    vehicle: &VehicleRecord,
    session: &ImportedOpenSession,
    at_ms: i64,
) {
    let mut connection = store.open().expect("connection");
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("begin");
    promote_imported_open_session_in_transaction(
        &transaction,
        source.source_id,
        vehicle.vehicle_id,
        1,
        session,
        at_ms,
        None,
    )
    .expect("promote");
    transaction.commit().expect("commit");
}

/// The six cursors of C1 R84, which match the domains in
/// [`WatermarkDomain::ALL`] order.
fn cursor_of(cursors: [i64; 6], domain: WatermarkDomain) -> i64 {
    cursors[WatermarkDomain::ALL
        .iter()
        .position(|candidate| *candidate == domain)
        .expect("domain")]
}

#[test]
fn c2_t86_every_domain_seed_promotion() {
    let snapshots = [
        pair(Some(500), Some(T_HI)),
        pair(Some(300), Some(T_LO)),
        pair(None, None),
        pair(Some(800), Some(T_MID)),
        pair(Some(800), Some(T_MID)),
    ];
    // d's stored pair after each call under the writer seed and under the
    // promotion, and its cursor.
    let seed_rows = [
        pair(Some(500), Some(T_HI)),
        pair(Some(500), Some(T_HI)),
        pair(Some(500), Some(T_HI)),
        pair(Some(800), Some(T_HI)),
        pair(Some(800), Some(T_HI)),
    ];
    let cursor_after = [501, 501, 501, 801, 801];
    let seed_no_op = [false, true, true, false, true];
    for domain in WatermarkDomain::ALL {
        for path in ["seed", "promote"] {
            let label = format!("{} {path}", domain.as_str());
            let (_directory, store, source, vehicle) = t69_store(&format!("t86-{label}"));
            let v = vehicle.vehicle_id;
            let r0 = t69_dirty_revision(&store, v);
            let mut after_first: Option<(Vec<Vec<String>>, [i64; 6], i64)> = None;
            for (index, snapshot) in snapshots.iter().enumerate() {
                let call = index + 1;
                let at_ms = T69_T0 + call as i64 * T69_MIN;
                let session = s_d(domain, *snapshot);
                if path == "seed" {
                    let report = t69_seed(&store, &source, &vehicle, &session, at_ms);
                    assert_eq!(
                        report.no_op, seed_no_op[index],
                        "{label} call {call}: no_op"
                    );
                } else {
                    promote(&store, &source, &vehicle, &session, at_ms);
                }
                // d's pair: the merge of every snapshot so far.
                let rows = stored_pairs(&store, v);
                assert_eq!(rows.len(), 6, "{label} call {call}: one row per domain");
                assert_eq!(
                    rows[domain.as_str()],
                    seed_rows[index],
                    "{label} call {call}: the stored pair of the domain"
                );
                // The other five domains hold S's pairs.
                for other in WatermarkDomain::ALL
                    .into_iter()
                    .filter(|other| *other != domain)
                {
                    assert_eq!(
                        rows[other.as_str()],
                        s_pair(other),
                        "{label} call {call}: {}",
                        other.as_str()
                    );
                }
                // The matching cursor follows the stored maximum, never lowered;
                // the other five keep their values after call 1.
                let cursors = id_cursors(&store, v);
                assert_eq!(
                    cursor_of(cursors, domain),
                    cursor_after[index],
                    "{label} call {call}: cursor"
                );
                match &after_first {
                    None => {
                        after_first =
                            Some((t69_rows(&store, v), cursors, t69_dirty_revision(&store, v)));
                        assert_eq!(
                            after_first.as_ref().unwrap().2,
                            r0 + 1,
                            "{label}: first dirty"
                        );
                    }
                    Some((first_rows, first_cursors, first_revision)) => {
                        for other in WatermarkDomain::ALL
                            .into_iter()
                            .filter(|other| *other != domain)
                        {
                            assert_eq!(
                                cursor_of(cursors, other),
                                cursor_of(*first_cursors, other),
                                "{label} call {call}: cursor of {}",
                                other.as_str()
                            );
                        }
                        if path == "seed" && matches!(call, 2 | 3) {
                            // A true no-op leaves every row and the dirty
                            // revision where call 1 left them.
                            assert_eq!(
                                &t69_rows(&store, v),
                                first_rows,
                                "{label} call {call}: rows"
                            );
                            assert_eq!(
                                t69_dirty_revision(&store, v),
                                *first_revision,
                                "{label} call {call}: dirty revision"
                            );
                        }
                        if path == "seed" && call == 4 {
                            assert_eq!(
                                t69_dirty_revision(&store, v),
                                first_revision + 1,
                                "{label} call 4: dirty revision"
                            );
                        }
                    }
                }
                if path == "seed" && call == 5 {
                    // The repeat of call 4 changes nothing.
                    assert_eq!(t69_dirty_revision(&store, v), r0 + 2, "{label} call 5");
                }
            }
        }
    }
}

#[test]
fn c2_r80_d_lower_pairs_in_every_domain_are_one_no_op() {
    // The no-op comparison reads every domain's merge, not the snapshot's own
    // pair: a snapshot whose only difference from the stored state is a lower or
    // absent pair is a no-op in every domain at once, and one raised pair among
    // them is written once (C2 R80 (d), (e)).
    let (_directory, store, source, vehicle) = t69_store("t86-all-domains");
    let v = vehicle.vehicle_id;
    let high = |session: &mut ImportedOpenSession| {
        for domain in WatermarkDomain::ALL {
            let value = ImportedWatermark::from(pair(Some(900), Some(T_HI)));
            let watermarks = &mut session.watermarks;
            match domain {
                WatermarkDomain::Drives => watermarks.drives = value,
                WatermarkDomain::Positions => watermarks.positions = value,
                WatermarkDomain::ChargingProcesses => watermarks.charging_processes = value,
                WatermarkDomain::Charges => watermarks.charges = value,
                WatermarkDomain::States => watermarks.states = value,
                WatermarkDomain::Updates => watermarks.updates = value,
            }
        }
    };
    let mut first = t69_s((None, None));
    high(&mut first);
    assert!(!t69_seed(&store, &source, &vehicle, &first, T69_T0).no_op);
    let after_first = t69_rows(&store, v);
    let revision = t69_dirty_revision(&store, v);
    let lower = t69_s((Some(1), Some(T_LO)));
    assert!(t69_seed(&store, &source, &vehicle, &lower, T69_T0 + T69_MIN).no_op);
    assert_eq!(t69_rows(&store, v), after_first);
    assert_eq!(t69_dirty_revision(&store, v), revision);
    // One pair rises in one field only: written once, the other fields kept.
    let mut raised = t69_s((None, None));
    raised.watermarks.states.max_timestamp_ms = Some(T69_T0);
    let report = t69_seed(&store, &source, &vehicle, &raised, T69_T0 + 2 * T69_MIN);
    assert!(!report.no_op);
    assert_eq!(
        stored_pairs(&store, v)["states"],
        pair(Some(900), Some(T69_T0)),
        "the time rises, the id stays"
    );
    assert!(
        t69_seed(&store, &source, &vehicle, &raised, T69_T0 + 2 * T69_MIN).no_op,
        "repeating it is a no-op"
    );
}

#[test]
fn c2_r80_writer_reports_changed_domains_in_canonical_order() {
    let (_directory, store, source, vehicle) = t69_store("r80-writer");
    let other = store
        .register_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, "r80-writer-second"),
            1_002,
        )
        .expect("second source");
    let v = vehicle.vehicle_id;
    let write = |source: &SourceRecord, pairs: &[(WatermarkDomain, WatermarkPair)]| {
        let mut connection = store.open().expect("connection");
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("begin");
        let changed =
            raise_source_watermarks_in_transaction(&transaction, source.source_id, v, pairs)
                .expect("write");
        transaction.commit().expect("commit");
        changed
    };
    // Listed out of order: the result is in canonical order; the all-null pair
    // inserts its row but is not a changed boundary.
    let changed = write(
        &source,
        &[
            (WatermarkDomain::Updates, pair(None, None)),
            (WatermarkDomain::Charges, pair(Some(5), Some(T_LO))),
            (WatermarkDomain::Drives, pair(Some(3), None)),
            (WatermarkDomain::States, pair(None, None)),
        ],
    );
    assert_eq!(
        changed,
        vec![WatermarkDomain::Drives, WatermarkDomain::Charges]
    );
    let rows = stored_pairs(&store, v);
    assert_eq!(rows.len(), 4, "the all-null rows exist");
    assert_eq!(rows["updates"], pair(None, None));
    assert_eq!(rows["states"], pair(None, None));
    // The same pairs again, lower pairs and absent pairs change nothing.
    let unchanged = write(
        &source,
        &[
            (WatermarkDomain::Drives, pair(Some(3), None)),
            (WatermarkDomain::Charges, pair(Some(1), Some(T_LO - 1))),
            (WatermarkDomain::States, pair(None, None)),
        ],
    );
    assert!(unchanged.is_empty(), "{unchanged:?}");
    assert_eq!(stored_pairs(&store, v), rows);
    // A raised time alone, a raised id alone, and a domain listed twice (merged
    // in order).
    let raised = write(
        &source,
        &[
            (WatermarkDomain::Positions, pair(Some(7), Some(T_MID))),
            (WatermarkDomain::Positions, pair(Some(9), None)),
            (WatermarkDomain::Drives, pair(None, Some(T_HI))),
            (WatermarkDomain::Charges, pair(Some(6), None)),
        ],
    );
    assert_eq!(
        raised,
        vec![
            WatermarkDomain::Drives,
            WatermarkDomain::Positions,
            WatermarkDomain::Charges
        ]
    );
    let rows = stored_pairs(&store, v);
    assert_eq!(rows["positions"], pair(Some(9), Some(T_MID)));
    assert_eq!(rows["drives"], pair(Some(3), Some(T_HI)));
    assert_eq!(rows["charges"], pair(Some(6), Some(T_LO)));
    // Another source's row of the same vehicle is its own pair; the cursor raise
    // (C1 R84) reads the larger of the two.
    let second = write(&other, &[(WatermarkDomain::Drives, pair(Some(2), None))]);
    assert_eq!(
        second,
        vec![WatermarkDomain::Drives],
        "a new row with a known field"
    );
    let connection = store.open().expect("connection");
    let floors = watermark_id_floors(&connection, v).expect("floors");
    assert_eq!(floors.drives, 3, "the larger source row wins");
    let own: WatermarkPair = connection
        .query_row(
            "SELECT max_source_row_id, max_timestamp_ms FROM lifecycle_source_watermarks
             WHERE source_id = ?1 AND vehicle_id = ?2 AND domain = 'drives'",
            params![other.source_id.to_string(), v.to_string()],
            |row| Ok(pair(row.get(0)?, row.get(1)?)),
        )
        .expect("second source's row");
    assert_eq!(own, pair(Some(2), None));
}

/// The kind and type that `charge_session_kind` gives the importer's samples in
/// sample order.
fn kind_of_imported(samples: &[ImportedCharge]) -> crate::lifecycle::ChargeSessionKind {
    let mut ordered = samples.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|sample| (sample.date_ms, sample.id));
    let projected = ordered
        .into_iter()
        .map(crate::teslamate_projection::project_charge_sample)
        .collect::<Vec<_>>();
    crate::lifecycle::charge_session_kind(&projected)
}

/// The seeded open charge's `(is_dc, fast_charger_type)` read back from the
/// stored lifecycle state.
fn stored_kind(store: &HubStore, vehicle: Uuid) -> (Option<bool>, Option<String>) {
    let record = store
        .load_lifecycle_state(vehicle)
        .expect("state")
        .expect("seeded state");
    let state =
        crate::lifecycle::OpenSessionState::decode(&record.open_session_json).expect("decode");
    let open = state.open_charge.expect("the imported open charge");
    (open.is_dc, open.fast_charger_type)
}

#[test]
fn c2_t90_seed_kind_agrees() {
    // (a) DC samples at t0 - 20 min (id 101) and t0 - 10 min (id 102): the
    // second names its type.
    let (_directory, store, source, vehicle) = t69_store("t90-a");
    let mut session = t69_s((Some(50), Some(T_HI)));
    let dc = |sample: &mut ImportedCharge, kind: &str, energy: f64| {
        sample.charger_power_kw = Some(120.0);
        sample.charger_voltage = Some(2.0);
        sample.charger_actual_current = Some(0.0);
        sample.charger_phases = None;
        sample.fast_charger_present = Some(true);
        sample.fast_charger_type = Some(kind.to_owned());
        sample.fast_charger_brand = Some("Tesla".to_owned());
        sample.charge_energy_added_kwh = Some(energy);
    };
    dc(&mut session.charge_samples[0], "<invalid>", 1.00);
    dc(&mut session.charge_samples[1], "Combo", 2.00);
    assert_eq!(
        (session.charge_samples[0].id, session.charge_samples[1].id),
        (101, 102)
    );
    t69_seed(&store, &source, &vehicle, &session, T69_T0);
    let function = kind_of_imported(&session.charge_samples);
    assert_eq!(function.is_dc, Some(true));
    assert_eq!(function.fast_charger_type.as_deref(), Some("Combo"));
    assert_eq!(
        stored_kind(&store, vehicle.vehicle_id),
        (function.is_dc, function.fast_charger_type.clone()),
        "(a) the seeded state equals the function"
    );

    // (b) Two A7 samples (7 kW at 240 V, 30 A, one phase) of types
    // `MCSingleWireCAN` and then `Tesla`.
    let (_directory, store, source, vehicle) = t69_store("t90-b");
    let mut session = t69_s((Some(50), Some(T_HI)));
    let a7 = |sample: &mut ImportedCharge, kind: &str| {
        sample.charger_power_kw = Some(7.0);
        sample.charger_voltage = Some(240.0);
        sample.charger_actual_current = Some(30.0);
        sample.charger_phases = Some(1);
        sample.fast_charger_present = None;
        sample.fast_charger_brand = None;
        sample.fast_charger_type = Some(kind.to_owned());
    };
    a7(&mut session.charge_samples[0], "MCSingleWireCAN");
    a7(&mut session.charge_samples[1], "Tesla");
    t69_seed(&store, &source, &vehicle, &session, T69_T0);
    let function = kind_of_imported(&session.charge_samples);
    assert_eq!(function.is_dc, Some(false));
    assert_eq!(
        function.fast_charger_type.as_deref(),
        Some("MCSingleWireCAN")
    );
    assert_eq!(
        stored_kind(&store, vehicle.vehicle_id),
        (function.is_dc, function.fast_charger_type.clone()),
        "(b) the seeded state equals the function"
    );
}
