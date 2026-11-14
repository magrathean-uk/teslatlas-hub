// SPDX-License-Identifier: AGPL-3.0-only

//! The seam repair on the pack side (M03-T19, review REV-CX-M03-T16): the
//! negotiated 1.4 route refuses while it is unplanned, already packed
//! positions follow changed seam decisions, a source deletion never removes a
//! session the seam keeps, and the seam's sync becomes visible only together
//! with the import that carries its replacements.

use super::*;

/// Every entity of `projected` recorded into a successor capture, in the
/// order the pack sink records a fragment, then sealed.
pub(super) fn capture_everything(
    fixture: &Fixture,
    run_id: Uuid,
    projected: &SourceDbProjection,
) -> SourceDbProjectionStateCapture {
    let binding = &fixture.binding;
    let mut capture = direct_projection_state_capture(
        &fixture.store,
        &fixture.gate,
        run_id,
        binding.vehicle_id,
        binding.account_id,
        CAR,
        true,
        state_limits(),
    )
    .expect("capture");
    note_source_sessions(&mut capture, projected);
    for car in &projected.snapshot.cars {
        capture.record_car(car).expect("car");
    }
    for drive in &projected.snapshot.drives {
        capture.record_drive(drive).expect("drive");
    }
    for position in &projected.snapshot.positions {
        capture.record_position(position).expect("position");
    }
    for charge in &projected.snapshot.charges {
        capture.record_charge(charge).expect("charge");
    }
    for sample in &projected.snapshot.charge_samples {
        capture.record_charge_sample(CAR, sample).expect("charge sample");
    }
    capture.seal().expect("seal");
    capture
}

/// Gives a successor capture every source drive and charge before any child
/// is recorded, as the importer does once it holds them (M03-T21).
pub(super) fn note_source_sessions(
    capture: &mut SourceDbProjectionStateCapture,
    projected: &SourceDbProjection,
) {
    use crate::import::source_db::seam::SessionRow;
    let drives: Vec<SessionRow> = projected
        .snapshot
        .drives
        .iter()
        .map(|drive| SessionRow {
            id: drive.id,
            start_ms: drive.start_date_ms,
            end_ms: Some(drive.end_date_ms),
        })
        .collect();
    let charges: Vec<SessionRow> = projected
        .snapshot
        .charges
        .iter()
        .map(|charge| SessionRow {
            id: charge.id,
            start_ms: charge.start_date_ms,
            end_ms: charge.end_date_ms,
        })
        .collect();
    capture.note_seam_source_sessions(&drives, &charges);
}

/// One later import of `history`, planned, written and published as the
/// importer does.
pub(super) fn import(
    fixture: &Fixture,
    history: &SourceDbHistory,
    at_ms: i64,
) -> Result<SourceDbSeamSkipped, SourceDbImportError> {
    let projected = project_vehicle(history, CAR).expect("projection");
    let mut planned = plan(fixture, &projected);
    let run_id = begin_run(fixture, at_ms);
    let capture = capture_everything(fixture, run_id, &projected);
    publish_captured(fixture, &projected, run_id, capture, &mut planned)?;
    Ok(planned.seam.skipped)
}

/// A window `[start, end)` handed back, as the store records it.
pub(super) fn window(fixture: &Fixture, seq: i64, start_ms: i64, end_ms: i64) {
    fixture
        .store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
             VALUES (?1, ?2, ?3, ?4, '[]', 'handback')",
            params![seq, format!("grant-{seq}"), start_ms, end_ms],
        )
        .expect("coverage");
}

/// The Hub's drives (id, start, end) and positions (id, drive, instant), as
/// the collector stores and journals them, then published as live sync.
pub(super) fn hub_rows(fixture: &Fixture, drives: &[(i64, i64, i64)], positions: &[(i64, i64, i64)]) {
    let template = project_vehicle(&base_history(), CAR).expect("projection").snapshot.drives[0]
        .clone();
    let vehicle = fixture.binding.vehicle_id.to_string();
    let mut connection = fixture.store.open().expect("catalogue");
    let transaction = connection.transaction().expect("transaction");
    let journal = |entity: &str, id: i64, json: &str| {
        let revision: i64 = transaction
            .query_row(
                "INSERT INTO sync_mutation_sequences(vehicle_id, next_revision) VALUES (?1, 2)
                 ON CONFLICT(vehicle_id) DO UPDATE SET next_revision = next_revision + 1
                 RETURNING next_revision - 1",
                params![vehicle.as_str()],
                |row| row.get(0),
            )
            .expect("revision");
        transaction
            .execute(
                "INSERT INTO sync_mutations(vehicle_id, revision, entity, entity_id, car_id,
                    operation, payload_json, published, claimed_until_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'upsert', ?6, 0, 0)",
                params![vehicle.as_str(), revision, entity, id, CAR, json],
            )
            .expect("mutation");
    };
    for (id, start_ms, end_ms) in drives {
        let mut drive = template.clone();
        drive.id = *id;
        drive.start_date_ms = *start_ms;
        drive.end_date_ms = *end_ms;
        let json = serde_json::to_string(&drive).expect("drive json");
        transaction
            .execute(
                "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![vehicle.as_str(), id, CAR, json],
            )
            .expect("Hub drive");
        journal("drive", *id, &json);
    }
    for (id, drive_id, date_ms) in positions {
        let mut position =
            crate::lifecycle::imported_position(&source_position(*id, Some(*drive_id), *date_ms));
        position.drive_id = Some(*drive_id);
        let json = serde_json::to_string(&position).expect("position json");
        transaction
            .execute(
                "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![vehicle.as_str(), id, drive_id, CAR, json],
            )
            .expect("Hub position");
        journal("position", *id, &json);
    }
    transaction.commit().expect("commit");
    publish_sync_mutations_ahead_of_packs(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        fixture.binding.vehicle_id,
    )
    .expect("live sync of the Hub's collection");
}

/// A client that applied every advertised head.
pub(super) fn client(fixture: &Fixture) -> Client {
    let mut client = Client::default();
    for (_, rows) in heads(&fixture.store, fixture.binding.vehicle_id) {
        client.apply(&rows);
    }
    client
}

/// Every position a client holds belongs to a drive the client holds.
pub(super) fn assert_positions_have_their_drives(client: &Client, at: &str) {
    for (id, (drive, _)) in &client.positions {
        if let Some(drive) = drive {
            assert!(
                client.drives.contains_key(drive),
                "{at}: position {id} points at drive {drive}, which the client does not hold"
            );
        }
    }
}

/// The rows of the heads from `from` on.
pub(super) fn rows_since(fixture: &Fixture, from: usize) -> PackRows {
    let mut all = PackRows::default();
    for (_, rows) in heads(&fixture.store, fixture.binding.vehicle_id)
        .into_iter()
        .skip(from)
    {
        all.drives.extend(rows.drives);
        all.positions.extend(rows.positions);
        all.tombstones.extend(rows.tombstones);
    }
    all
}

/// An unchanged replay of `history` publishes no drive, position or
/// tombstone.
pub(super) fn assert_replay_publishes_nothing(fixture: &Fixture, history: &SourceDbHistory, at_ms: i64) {
    let before = heads(&fixture.store, fixture.binding.vehicle_id).len();
    let skipped = import(fixture, history, at_ms).expect("replay");
    let replayed = rows_since(fixture, before);
    assert!(
        replayed.drives.is_empty() && replayed.positions.is_empty() && replayed.tombstones.is_empty(),
        "an unchanged replay published {replayed:?}"
    );
    assert_eq!(skipped, SourceDbSeamSkipped::default());
}

/// Review finding 2: drive 14 and its positions were packed before the
/// window. The Hub's drive H3 then wins the seam over drive 14 although no
/// source row changed: clients receive position 1401 (outside coverage)
/// under H3 and lose position 1402 (inside coverage), and an unchanged replay
/// publishes nothing.
#[test]
fn seam_repair_packs_parent_skipped_remaps_packed_position() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let mut drives = base_history().drives;
    drives.push(source_drive(14, T - 10 * MINUTE, T + 20 * MINUTE));
    let history = history(
        drives,
        vec![
            source_position(1401, Some(14), T - 5 * MINUTE),
            source_position(1402, Some(14), T + 5 * MINUTE),
        ],
    );
    import(&fixture, &history, T - 30 * MINUTE).expect("import before the window");
    let packed = client(&fixture);
    assert_eq!(packed.positions.get(&1401), Some(&(Some(14), T - 5 * MINUTE)));
    assert!(packed.positions.contains_key(&1402));

    let h3 = HUB_ID_BASE + 3;
    window(&fixture, 1, T, T + HOUR);
    hub_rows(&fixture, &[(h3, T, T + 50 * MINUTE)], &[(HUB_ID_BASE + 3, h3, T + 30 * MINUTE)]);
    let existing_at = heads(&fixture.store, vehicle_id).len();
    import(&fixture, &history, T + 3 * HOUR).expect("the same source after the window");

    let client = client(&fixture);
    assert!(!client.drives.contains_key(&14), "drive 14 lost to H3");
    assert_eq!(
        client.positions.get(&1401),
        Some(&(Some(h3), T - 5 * MINUTE)),
        "the packed position follows its drive's winner"
    );
    assert!(!client.positions.contains_key(&1402), "inside coverage the Hub's rows stand");
    assert_eq!(client.drives, store_drives(&fixture.store, vehicle_id));
    assert_positions_have_their_drives(&client, "fresh client");
    let all = heads(&fixture.store, vehicle_id);
    let mut existing = Client::default();
    for (_, rows) in &all[..existing_at] {
        existing.apply(rows);
    }
    for (sequence, rows) in &all[existing_at..] {
        existing.apply(rows);
        existing.assert_each_session_once(&format!("existing client at head {sequence}"));
    }
    assert_eq!(existing, client);
    assert_replay_publishes_nothing(&fixture, &history, T + 4 * HOUR);
}

/// Review finding 2: the source corrects an already packed position's
/// instant into coverage. Clients receive its tombstone, and an unchanged
/// replay publishes nothing.
#[test]
fn seam_repair_packs_position_corrected_into_coverage_is_tombstoned() {
    let fixture = fixture();
    collect(&fixture);
    let mut history = successor_history();
    import(&fixture, &history, T + 3 * HOUR).expect("import after the window");
    assert!(client(&fixture).positions.contains_key(&1201));

    for position in &mut history.positions {
        if position.id == 1201 {
            position.date_ms = T + 30 * MINUTE;
        }
    }
    import(&fixture, &history, T + 4 * HOUR).expect("the corrected source");
    let client = client(&fixture);
    assert!(
        !client.positions.contains_key(&1201),
        "a position corrected into coverage leaves the clients"
    );
    assert_positions_have_their_drives(&client, "after the correction");
    assert_replay_publishes_nothing(&fixture, &history, T + 5 * HOUR);
}

/// Review finding 3: the seam moved the Hub's position onto source drive 13,
/// which the source then deletes. The store keeps drive 13 for its Hub
/// position, so clients keep it too; an unchanged replay publishes nothing.
#[test]
fn seam_repair_packs_source_deletion_keeps_a_retained_drive() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(&fixture);
    import(&fixture, &successor_history(), T + 3 * HOUR).expect("import after the window");
    assert_eq!(
        store_hub_positions(&fixture.store, vehicle_id)
            .get(&(HUB_ID_BASE + 2))
            .map(|row| row.0),
        Some(Some(13))
    );

    let mut history = successor_history();
    history.drives.retain(|drive| drive.id != 13);
    history.positions.retain(|position| position.drive_id != Some(13));
    import(&fixture, &history, T + 4 * HOUR).expect("the source deleted drive 13");
    assert!(
        store_drives(&fixture.store, vehicle_id).contains_key(&13),
        "the store keeps drive 13 for the Hub's position"
    );
    let client = client(&fixture);
    assert_eq!(client.drives, store_drives(&fixture.store, vehicle_id));
    assert_eq!(
        client.positions.get(&(HUB_ID_BASE + 2)).map(|row| row.0),
        Some(Some(13))
    );
    assert_positions_have_their_drives(&client, "after the source deletion");
    assert_replay_publishes_nothing(&fixture, &history, T + 5 * HOUR);
}

/// What a client holds and what the store holds of the Hub's side, for
/// "the previous complete session is still in sync".
fn session_state(fixture: &Fixture) -> (Client, BTreeMap<i64, (i64, i64)>, i64, i64) {
    let vehicle_id = fixture.binding.vehicle_id;
    let unpublished = fixture
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1 AND published = 0",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("journal");
    (
        client(fixture),
        store_drives(&fixture.store, vehicle_id),
        mutation_count(&fixture.store, vehicle_id),
        unpublished,
    )
}

/// Review finding 4: planning goes ahead of nothing. After the plan (a
/// crash there) and after a pack-write failure, the Hub's drive H2 is still
/// in the store and on clients and nothing waits to be published; the next
/// import carries the seam and its replacement together.
#[test]
fn seam_repair_packs_pack_write_failure_keeps_the_previous_session() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let (_, h2) = collect(&fixture);
    let before = session_state(&fixture);
    assert!(before.0.drives.contains_key(&h2));
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let mut planned = plan(&fixture, &projected);
    assert_eq!(session_state(&fixture), before, "a crash after planning loses nothing");

    let run_id = begin_run(&fixture, T + 3 * HOUR);
    let capture = capture_everything(&fixture, run_id, &projected);
    let base_snapshot_id = fixture.store.v2_head(vehicle_id).expect("head").expect("base").0;
    let failed = publish_planned_direct_successor(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        PlannedSuccessorRun {
            run_id,
            source_id: fixture.binding.account_id,
            imported_at_ms: T + 3 * HOUR,
            base_snapshot_id,
            // No pack fits: every pack write fails.
            minimum_free_bytes: 1 << 62,
            fingerprint: Sha256Digest::of_bytes(b"seam-repair-packs-failed"),
            geofences: &[],
            materialised_car: &fixture.car,
            materialised_drives: &projected.snapshot.drives,
            materialised_charges: &projected.snapshot.charges,
        },
        &fixture.binding,
        &fixture.car,
        capture,
        &mut planned,
    );
    assert!(failed.is_err(), "the pack writes fail");
    fixture.store.abort_import_generation(run_id).expect("abort");
    assert_eq!(session_state(&fixture), before, "a failed import leaves the session in sync");

    import(&fixture, &successor_history(), T + 4 * HOUR).expect("the next import");
    let client = client(&fixture);
    assert!(!client.drives.contains_key(&h2) && client.drives.contains_key(&13));
    client.assert_each_session_once("after the next import");
    assert_eq!(client.drives, store_drives(&fixture.store, vehicle_id));
}

/// Review finding 4: publication refuses because the store changed after the
/// plan (a second window). The Hub's drive H2 is still in the store and on
/// clients, and nothing waits to be published.
#[test]
fn seam_repair_packs_refused_finalisation_keeps_the_previous_session() {
    let fixture = fixture();
    let (_, h2) = collect(&fixture);
    let before = session_state(&fixture);
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let mut planned = plan(&fixture, &projected);
    window(&fixture, 2, T + 2 * HOUR - 5 * MINUTE, T + 3 * HOUR);
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    let refused = publish(&fixture, &projected, run_id, &mut planned);
    assert!(
        matches!(
            refused,
            Err(SourceDbImportError::Store(StoreError::ImportGenerationConflict))
        ),
        "{refused:?}"
    );
    let after = session_state(&fixture);
    assert!(after.0.drives.contains_key(&h2), "clients keep H2");
    assert_eq!(after, before, "the refused import changed nothing");
}

/// A request to the paired router, answered on `runtime`.
pub(super) fn served(
    runtime: &tokio::runtime::Runtime,
    app: &axum::Router,
    bearer: &str,
    profile: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (axum::http::StatusCode, Vec<u8>) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let mut request = axum::http::Request::builder()
        .uri(uri)
        .header(axum::http::header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(crate::server::SYNC_PROFILE_HEADER, profile)
        .header(crate::server::SUPPORTED_SCHEMAS_HEADER, "2.1,2.2");
    let body = match body {
        Some(value) => {
            request = request
                .method("POST")
                .header(axum::http::header::CONTENT_TYPE, "application/json");
            axum::body::Body::from(serde_json::to_vec(&value).expect("request json"))
        }
        None => axum::body::Body::empty(),
    };
    runtime.block_on(async {
        let response = app
            .clone()
            .oneshot(request.body(body).expect("request"))
            .await
            .expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes()
            .to_vec();
        (status, bytes)
    })
}

/// A 1.3 client walks the schema-2.1 route from a fresh bootstrap: every
/// pack it is served, in order.
fn served_21_packs(
    runtime: &tokio::runtime::Runtime,
    app: &axum::Router,
    bearer: &str,
    fixture: &Fixture,
) -> Vec<Sha256Digest> {
    let vehicle_id = fixture.binding.vehicle_id;
    let profile = "hub-sync-v1@1.3.0";
    let (status, raw) = served(
        runtime,
        app,
        bearer,
        profile,
        &format!("/v1/vehicles/{vehicle_id}/sync/manifest"),
        None,
    );
    assert_eq!(status, axum::http::StatusCode::OK);
    let manifest: serde_json::Value = serde_json::from_slice(&raw).expect("manifest");
    assert_eq!(manifest["schema_version"], "2.1", "the 1.3 client gets the planned lineage");
    let mut packs = vec![manifest["pack"]["sha256"].as_str().expect("pack").to_owned()];
    let mut receipt = manifest["receipt_id"].as_str().expect("receipt").to_owned();
    let mut sequence = manifest["sequence"].as_u64().expect("sequence");
    loop {
        let (status, raw) = served(
            runtime,
            app,
            bearer,
            profile,
            &format!("/v1/vehicles/{vehicle_id}/sync/changes-since"),
            Some(serde_json::json!({
                "base_receipt_id": receipt, "base_manifest_schema": "2.1",
                "from_sequence": sequence,
                "schema_version_range": {"minimum": "2.1", "maximum": "2.2"},
            })),
        );
        assert_eq!(status, axum::http::StatusCode::OK);
        let answer: serde_json::Value = serde_json::from_slice(&raw).expect("changes");
        if answer["kind"] == "no_op" {
            break;
        }
        packs.push(answer["pack"]["sha256"].as_str().expect("pack").to_owned());
        receipt = answer["receipt_id"].as_str().expect("receipt").to_owned();
        sequence = answer["to_sequence"].as_u64().expect("sequence");
    }
    packs
        .into_iter()
        .map(|digest| {
            let (status, bytes) = served(
                runtime,
                app,
                bearer,
                profile,
                &format!("/v1/packs/sha256/{digest}.sqlite.zst"),
                None,
            );
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_eq!(Sha256Digest::of_bytes(&bytes).to_string(), digest);
            digest.parse().expect("digest")
        })
        .collect()
}

/// Review finding 1: the negotiated 1.4 route serves a raw copy of the
/// source with no Hub row and no seam decision. Once the Hub has collected
/// the vehicle it refuses: a fresh 1.4 client finds no physical base, an
/// existing one is told its schema range has no supported version, and a
/// 1.3 client is served the planned lineage, whose packs hold each session
/// once with the Hub's rows.
#[test]
fn seam_repair_packs_1_4_route_refuses_while_unplanned() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("clock");
    let stage_root = crate::private_tempdir().expect("stage root");
    let mut stage = crate::source_db_stage::SourceDbStage::create_physical_v3(
        stage_root.path().join("physical"),
        crate::sync::physical_fragments::tests::stage_limits(),
    )
    .expect("physical stage");
    crate::sync::physical_fragments::tests::seed_roots(&mut stage);
    stage.seal().expect("seal");
    let head = crate::physical_publication::publish_sealed_physical_v3_stage_with_gate_at(
        &fixture.store,
        &fixture.cursor_key,
        fixture.binding.clone(),
        stage,
        &fixture.gate,
        now_ms,
    )
    .expect("physical head")
    .admission;
    let invitation = fixture
        .store
        .create_pairing("seam repair packs", 0, i64::MAX)
        .expect("pairing");
    let access = fixture
        .store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "reference client", now_ms)
        .expect("claim");
    let bearer = access.access_token.as_bearer();
    let app = crate::server::paired_router(fixture.store.clone(), &fixture.cursor_key);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let manifest_uri = format!("/v1/vehicles/{vehicle_id}/sync/manifest");
    let (status, _) = served(&runtime, &app, bearer, "hub-sync-v1@1.4.0", &manifest_uri, None);
    assert_eq!(status, axum::http::StatusCode::OK, "before the Hub collects, 1.4 is served");

    let (h1, _) = collect(&fixture);
    import(&fixture, &successor_history(), T + 3 * HOUR).expect("import after the window");

    // A fresh 1.4 client: no physical base.
    let (status, _) = served(&runtime, &app, bearer, "hub-sync-v1@1.4.0", &manifest_uri, None);
    assert_eq!(status, axum::http::StatusCode::NOT_ACCEPTABLE);
    // An existing 1.4 client at the head it held.
    let signed = crate::manifest_signing::ManifestSigning::from_cursor_key(&fixture.cursor_key)
        .signed_physical_manifest_document(&head)
        .expect("signed head");
    let (status, raw) = served(
        &runtime,
        &app,
        bearer,
        "hub-sync-v1@1.4.0",
        &format!("/v1/vehicles/{vehicle_id}/sync/changes-since"),
        Some(serde_json::json!({
            "base_receipt_id": head.receipt_id, "base_manifest_schema": "2.2",
            "from_sequence": head.head_sequence,
            "schema_version_range": {"minimum": "2.2", "maximum": "2.2"},
            "base_manifest_id": head.snapshot_id.to_string(),
            "base_manifest_sha256": Sha256Digest::of_bytes(&signed).to_string(),
            "source": {
                "installation_id": head.installation_id, "account_id": head.account_id,
                "vehicle_id": head.vehicle_id, "generation": head.manifest.generation,
                "selected_car_id": head.selected_car_id,
            },
            "accepted_changed_set_formats": ["teslatlas-physical-v3-delta-v1"],
        })),
    );
    assert_eq!(status, axum::http::StatusCode::NOT_ACCEPTABLE);
    let answer: serde_json::Value = serde_json::from_slice(&raw).expect("error");
    assert_eq!(answer["code"], "schema_range_unsupported");

    // A fresh 1.3 client: the planned lineage, through the packs it is served.
    let mut client = Client::default();
    for digest in served_21_packs(&runtime, &app, bearer, &fixture) {
        client.apply(&pack_rows(&fixture.store, digest));
    }
    client.assert_each_session_once("fresh 1.3 client");
    assert!(client.drives.contains_key(&h1) && !client.drives.contains_key(&11));
    assert_eq!(client.drives, store_drives(&fixture.store, vehicle_id));
}

/// A sealed physical stage at `path`, as the source capture leaves it.
fn physical_stage(path: std::path::PathBuf) -> crate::source_db_stage::OwnedSourceDbStage {
    let mut stage = crate::source_db_stage::SourceDbStage::create_physical_v3(
        path,
        crate::sync::physical_fragments::tests::stage_limits(),
    )
    .expect("physical stage");
    crate::sync::physical_fragments::tests::seed_roots(&mut stage);
    stage.seal().expect("seal");
    crate::source_db_stage::OwnedSourceDbStage::new(stage)
}

/// Review finding 1, the importer's side: before the Hub collects the
/// vehicle its physical stage is published as captured; once it has, a stage
/// that reaches publication without a seam plan cannot be planned, so it is
/// withheld and discarded (M03-T21 keeps this refusal for exactly that case).
#[test]
fn seam_repair_packs_physical_stage_is_withheld_for_a_seam_vehicle() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    let route = |stage| {
        publish_physical_route(
            &fixture.store,
            &fixture.cursor_key,
            fixture.binding.clone(),
            Some(stage),
            &fixture.gate,
            None,
            None,
        )
        .expect("route")
    };
    assert!(
        route(physical_stage(root.path().join("before"))).is_some(),
        "before the Hub collects, the stage is published"
    );

    collect(&fixture);
    let path = root.path().join("after");
    assert!(
        route(physical_stage(path.clone())).is_none(),
        "after the Hub collects, a stage without a plan is withheld"
    );
    assert!(
        stage_files(&path).is_empty(),
        "the withheld stage is discarded: {:?}",
        stage_files(&path)
    );
}

/// Every file left under a stage's imports directory.
fn stage_files(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("stage entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

/// Review finding 2, the importer's side: for a vehicle with the seam, the
/// fingerprint an unchanged source is compared by also covers the seam's
/// plan, so a new window publishes and an unchanged source and plan do not.
#[test]
fn seam_repair_packs_unchanged_source_after_a_window_still_publishes() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let source = Sha256Digest::of_bytes(b"seam-repair-packs source");
    let folded = || {
        seam_folded_fingerprint(
            &fixture.store,
            &fixture.gate,
            vehicle_id,
            CAR,
            &projected.snapshot.drives,
            &projected.snapshot.charges,
            source,
        )
    };
    assert_eq!(folded(), source, "without the seam the source fingerprint stands");
    collect(&fixture);
    let first = folded();
    assert_ne!(first, source, "the seam's plan is part of the fingerprint");
    assert_eq!(folded(), first, "the same source and plan give the same fingerprint");
    window(&fixture, 2, T + 2 * HOUR - 5 * MINUTE, T + 3 * HOUR);
    assert_ne!(folded(), first, "a new window changes it");
}

/// The fingerprint the importer records for a source whose digest is
/// `source`: folded with the seam's plan for a vehicle with the seam.
pub(super) fn folded_fingerprint(
    fixture: &Fixture,
    projected: &SourceDbProjection,
    source: Sha256Digest,
) -> Sha256Digest {
    seam_folded_fingerprint(
        &fixture.store,
        &fixture.gate,
        fixture.binding.vehicle_id,
        CAR,
        &projected.snapshot.drives,
        &projected.snapshot.charges,
        source,
    )
}

/// One later import of `history`, whose source digest is `source`, recorded
/// under the fingerprint the importer folds for it, so that a replay of the
/// same source can take the importer's fingerprint path.
fn import_through_fingerprint(
    fixture: &Fixture,
    history: &SourceDbHistory,
    source: Sha256Digest,
    at_ms: i64,
) {
    let projected = project_vehicle(history, CAR).expect("projection");
    let fingerprint = folded_fingerprint(fixture, &projected, source);
    let mut planned = plan(fixture, &projected);
    let run_id = begin_run(fixture, at_ms);
    let capture = capture_everything(fixture, run_id, &projected);
    let base_snapshot_id = fixture
        .store
        .v2_head(fixture.binding.vehicle_id)
        .expect("head")
        .expect("base")
        .0;
    publish_planned_direct_successor(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        PlannedSuccessorRun {
            run_id,
            source_id: fixture.binding.account_id,
            imported_at_ms: at_ms,
            base_snapshot_id,
            minimum_free_bytes: 0,
            fingerprint,
            geofences: &[],
            materialised_car: &fixture.car,
            materialised_drives: &projected.snapshot.drives,
            materialised_charges: &projected.snapshot.charges,
        },
        &fixture.binding,
        &fixture.car,
        capture,
        &mut planned,
    )
    .expect("publication");
}

/// The review's three-session case (REV-CX-M03-T19 finding 2), for drives
/// and positions: source drive B (-15 to +5 min, a position at -10) is
/// packed first. The Hub then covers +10 to +60 with drive H, and the source
/// adds drive A (0 to +20): B overlaps A, A overlaps H, and H wins the group
/// although B comes nowhere near H or the coverage. B leaves the clients and
/// its packed position follows H at once, on a fresh and on an existing
/// client; an unchanged replay takes the fingerprint path and a forced one
/// publishes nothing, so no change is left for later.
#[test]
fn seam_repair_packs_transitive_overlap_moves_a_packed_position_at_once() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let (b, a, h) = (15, 16, HUB_ID_BASE + 15);
    let mut drives = base_history().drives;
    drives.push(source_drive(b, T - 15 * MINUTE, T + 5 * MINUTE));
    let first = history(drives.clone(), vec![source_position(1501, Some(b), T - 10 * MINUTE)]);
    import(&fixture, &first, T - 30 * MINUTE).expect("B packed before the window");
    assert_eq!(
        client(&fixture).positions.get(&1501),
        Some(&(Some(b), T - 10 * MINUTE))
    );
    let existing_at = heads(&fixture.store, vehicle_id).len();

    window(&fixture, 1, T + 10 * MINUTE, T + HOUR);
    hub_rows(
        &fixture,
        &[(h, T + 10 * MINUTE, T + HOUR)],
        &[(HUB_ID_BASE + 1501, h, T + 30 * MINUTE)],
    );
    drives.push(source_drive(a, T, T + 20 * MINUTE));
    let second = history(
        drives,
        vec![
            source_position(1501, Some(b), T - 10 * MINUTE),
            source_position(1601, Some(a), T + 5 * MINUTE),
        ],
    );
    let source = Sha256Digest::of_bytes(b"seam-repair-packs three sessions");
    import_through_fingerprint(&fixture, &second, source, T + 3 * HOUR);

    let fresh = client(&fixture);
    assert!(fresh.drives.contains_key(&h), "H won the group");
    assert!(
        !fresh.drives.contains_key(&b) && !fresh.drives.contains_key(&a),
        "B and A lost to H: {:?}",
        fresh.drives
    );
    assert_eq!(
        fresh.positions.get(&1501),
        Some(&(Some(h), T - 10 * MINUTE)),
        "B's packed position follows H at once"
    );
    assert_eq!(fresh.positions.get(&1601), Some(&(Some(h), T + 5 * MINUTE)));
    assert_eq!(fresh.drives, store_drives(&fixture.store, vehicle_id));
    assert_positions_have_their_drives(&fresh, "fresh client");
    let all = heads(&fixture.store, vehicle_id);
    let mut existing = Client::default();
    for (_, rows) in &all[..existing_at] {
        existing.apply(rows);
    }
    for (sequence, rows) in &all[existing_at..] {
        existing.apply(rows);
        existing.assert_each_session_once(&format!("existing client at head {sequence}"));
    }
    assert_eq!(existing, fresh, "the existing client ends where a fresh one does");

    let projected = project_vehicle(&second, CAR).expect("projection");
    assert!(
        fixture
            .store
            .source_fingerprint_matches(vehicle_id, folded_fingerprint(&fixture, &projected, source))
            .expect("fingerprint"),
        "an unchanged replay takes the fingerprint path and publishes nothing"
    );
    assert_replay_publishes_nothing(&fixture, &second, T + 4 * HOUR);
    assert_eq!(client(&fixture), fresh, "nothing was left for a later replay");
}

/// Review finding 4's guard, exercised where it stands (REV-CX-M03-T19):
/// the seam is staged, then the Hub journals one more change that leaves
/// the plan as it was. Publication applies the seam, finds a journal that
/// is not the staged one and refuses, inside finalisation; the clients keep
/// the previous complete session.
#[test]
fn seam_repair_packs_finalisation_refuses_a_journal_that_changed_after_staging() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let (h1, _) = collect(&fixture);
    let before = session_state(&fixture);
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let digest = |fixture: &Fixture| {
        fixture
            .store
            .dry_run_import_seam_ahead_of_packs(
                &fixture.gate,
                vehicle_id,
                CAR,
                &projected.snapshot.drives,
                &projected.snapshot.charges,
            )
            .expect("dry run")
            .ahead
            .packs
            .digest()
    };
    let staged_plan = digest(&fixture);
    let mut planned = plan(&fixture, &projected);
    assert!(!planned.seam_sync.is_empty(), "the seam stages its sync");

    // The Hub restates its own position H1 (another latitude): a new
    // journal entry that changes neither coverage nor any session's extent.
    let mut position = crate::lifecycle::imported_position(&source_position(
        HUB_ID_BASE + 1,
        Some(h1),
        T + 15 * MINUTE,
    ));
    position.drive_id = Some(h1);
    position.latitude = 47.6;
    let json = serde_json::to_string(&position).expect("position json");
    let vehicle = vehicle_id.to_string();
    let connection = fixture.store.open().expect("catalogue");
    connection
        .execute(
            "UPDATE materialised_positions SET position_json = ?3
              WHERE vehicle_id = ?1 AND position_id = ?2",
            params![vehicle.as_str(), position.id, json],
        )
        .expect("restated position");
    let revision: i64 = connection
        .query_row(
            "UPDATE sync_mutation_sequences SET next_revision = next_revision + 1
              WHERE vehicle_id = ?1 RETURNING next_revision - 1",
            params![vehicle.as_str()],
            |row| row.get(0),
        )
        .expect("revision");
    connection
        .execute(
            "INSERT INTO sync_mutations(vehicle_id, revision, entity, entity_id, car_id,
                operation, payload_json, published, claimed_until_ms)
             VALUES (?1, ?2, 'position', ?3, ?4, 'upsert', ?5, 0, 0)",
            params![vehicle.as_str(), revision, position.id, CAR, json],
        )
        .expect("journal");
    drop(connection);
    assert_eq!(digest(&fixture), staged_plan, "the plan itself did not change");

    let run_id = begin_run(&fixture, T + 3 * HOUR);
    let refused = publish(&fixture, &projected, run_id, &mut planned);
    assert!(
        matches!(
            refused,
            Err(SourceDbImportError::Store(StoreError::ImportGenerationConflict))
        ),
        "{refused:?}"
    );
    let after = session_state(&fixture);
    assert_eq!(after.0, before.0, "clients keep the previous complete session");
    assert_eq!(after.1, before.1, "the store's sessions are unchanged");
}

/// Review finding 1 on M03-T19: the snapshot and changed-set objects of a
/// physical head advertised before the Hub collected the vehicle are served
/// while the vehicle has none of the seam, and refused once it has, to a
/// client that holds their manifest or receipt as to any other; the control
/// route refuses as before.
#[test]
fn seam_repair_packs_1_4_objects_of_an_unplanned_head_are_refused() {
    use super::seam_physical_route::{history_stage, paired, publish_physical};
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_physical(&fixture, history_stage(root.path(), "first", &base_history()), None)
        .expect("first head");
    let mut moved = base_history();
    moved.drives[0].end_date_ms = Some(T - 5 * HOUR + 12 * MINUTE);
    publish_physical(&fixture, history_stage(root.path(), "second", &moved), None)
        .expect("second head");
    let connection = fixture.store.open().expect("catalogue");
    let digests = |sql: &str| -> Vec<String> {
        let mut statement = connection.prepare(sql).expect("objects");
        statement
            .query_map([], |row| row.get(0))
            .expect("object rows")
            .collect::<Result<_, _>>()
            .expect("object rows")
    };
    let snapshot = digests("SELECT sha256 FROM pending_physical_v3_packs ORDER BY ordinal");
    let changed_set = digests("SELECT sha256 FROM physical_v3_delta_packs ORDER BY ordinal");
    drop(connection);
    assert!(!snapshot.is_empty() && !changed_set.is_empty(), "both kinds advertised");
    let served_env = paired(&fixture);
    let fetch = |digest: &str| served_env.get(&format!("/v1/packs/sha256/{digest}.sqlite.zst")).0;
    for digest in snapshot.iter().chain(&changed_set) {
        assert_eq!(fetch(digest), axum::http::StatusCode::OK, "served without the seam");
    }

    collect(&fixture);
    for digest in snapshot.iter().chain(&changed_set) {
        assert_eq!(
            fetch(digest),
            axum::http::StatusCode::NOT_FOUND,
            "a source-only object is refused once the Hub has collected the vehicle"
        );
    }
    let manifest = served_env.get(&format!(
        "/v1/vehicles/{}/sync/manifest",
        fixture.binding.vehicle_id
    ));
    assert_eq!(manifest.0, axum::http::StatusCode::NOT_ACCEPTABLE);
}

/// Two physical heads of the source only, published before the Hub collects
/// the vehicle: the first is retained, the second is the current head and
/// carries the changed set from the first.
fn two_unplanned_physical_heads(fixture: &Fixture, root: &std::path::Path) {
    use super::seam_physical_route::{history_stage, publish_physical};
    publish_physical(fixture, history_stage(root, "first", &base_history()), None).expect("first head");
    let mut moved = base_history();
    moved.drives[0].end_date_ms = Some(T - 5 * HOUR + 12 * MINUTE);
    publish_physical(fixture, history_stage(root, "second", &moved), None).expect("second head");
}

/// The vehicle's planned set as stored, if any.
fn planned_set_value(fixture: &Fixture) -> Option<String> {
    use rusqlite::OptionalExtension;
    fixture
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT value FROM hub_metadata WHERE key = ?1",
            params![format!("physical_v3_seam_planned:{}", fixture.binding.vehicle_id)],
            |row| row.get(0),
        )
        .optional()
        .expect("planned set")
}

/// The snapshot id of the vehicle's head in `table`.
fn held_snapshot(fixture: &Fixture, table: &str) -> Uuid {
    let id: String = fixture
        .store
        .open()
        .expect("catalogue")
        .query_row(
            &format!("SELECT snapshot_id FROM {table} WHERE vehicle_id = ?1"),
            params![fixture.binding.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("one held head");
    id.parse().expect("snapshot id")
}

/// The changed-set receipt resolver applies the served check itself
/// (CHK-SEAM6; the route test reaches a refusal of the control head first):
/// the receipt of an unplanned head is served while the vehicle has none of
/// the seam, refused once the Hub has collected it, and served again once
/// the importer records that head as planned with the seam.
#[test]
fn seam_repair_packs_1_4_receipt_of_an_unplanned_head_is_refused() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let root = crate::private_tempdir().expect("stage root");
    two_unplanned_physical_heads(&fixture, root.path());
    let (base, target, snapshot, expires_at_ms): (String, String, String, i64) = fixture
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT delta.base_receipt_id, delta.target_receipt_id, target.snapshot_id,
                    base.expires_at_ms
               FROM physical_v3_delta_transitions AS delta
               JOIN pending_physical_v3_admissions AS target
                 ON target.receipt_id = delta.target_receipt_id
               JOIN retained_physical_v3_admissions AS base
                 ON base.receipt_id = delta.base_receipt_id
              WHERE delta.vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("one transition");
    let receipt = || {
        fixture
            .store
            .physical_v3_delta_receipt_for_base_at(vehicle_id, &base, &target, expires_at_ms - 1)
            .expect("receipt lookup")
    };
    assert!(receipt().is_some(), "served without the seam");

    collect(&fixture);
    assert!(
        receipt().is_none(),
        "the receipt of a head not planned with the seam is refused once the Hub has collected the vehicle"
    );
    fixture
        .store
        .record_physical_v3_seam_planned(&fixture.gate, vehicle_id, snapshot.parse().expect("snapshot id"))
        .expect("planned");
    assert!(receipt().is_some(), "a head planned with the seam is served");
}

/// The planned set is read strictly (CHK-SEAM6): for a vehicle with the seam
/// a malformed value refuses the served check and the next record with a
/// conflict, and the record leaves the value as it was. Without the seam the
/// set is never read, so the head is served as before.
#[test]
fn seam_repair_packs_malformed_planned_set_is_a_conflict() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let root = crate::private_tempdir().expect("stage root");
    two_unplanned_physical_heads(&fixture, root.path());
    fixture
        .store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO hub_metadata(key, value) VALUES (?1, 'not a set')",
            params![format!("physical_v3_seam_planned:{vehicle_id}")],
        )
        .expect("malformed planned set");
    assert!(
        fixture
            .store
            .pending_physical_v3_control_admission_for_vehicle(vehicle_id)
            .expect("control head")
            .is_some(),
        "without the seam the planned set is not read"
    );

    collect(&fixture);
    assert!(matches!(
        fixture.store.pending_physical_v3_control_admission_for_vehicle(vehicle_id),
        Err(StoreError::PhysicalV3AdmissionConflict)
    ));
    let pending = held_snapshot(&fixture, "pending_physical_v3_admissions");
    assert!(matches!(
        fixture.store.record_physical_v3_seam_planned(&fixture.gate, vehicle_id, pending),
        Err(StoreError::PhysicalV3AdmissionConflict)
    ));
    assert_eq!(planned_set_value(&fixture).as_deref(), Some("not a set"));
}

/// The planned set keeps only the heads the store still holds, and the one
/// being recorded (CHK-SEAM6): an id no head holds is dropped at the next
/// record, while the retained and the current head stay.
#[test]
fn seam_repair_packs_planned_set_keeps_only_held_heads() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let root = crate::private_tempdir().expect("stage root");
    two_unplanned_physical_heads(&fixture, root.path());
    let retained = held_snapshot(&fixture, "retained_physical_v3_admissions");
    let pending = held_snapshot(&fixture, "pending_physical_v3_admissions");
    let unheld = Uuid::new_v4();
    let planned = || -> std::collections::BTreeSet<Uuid> {
        serde_json::from_str::<std::collections::BTreeSet<String>>(
            &planned_set_value(&fixture).expect("planned set"),
        )
        .expect("planned set json")
        .iter()
        .map(|id| id.parse().expect("snapshot id"))
        .collect()
    };
    let record = |snapshot: Uuid| {
        fixture
            .store
            .record_physical_v3_seam_planned(&fixture.gate, vehicle_id, snapshot)
            .expect("record");
    };

    record(retained);
    record(unheld);
    assert_eq!(planned(), std::collections::BTreeSet::from([retained, unheld]));
    record(pending);
    assert_eq!(
        planned(),
        std::collections::BTreeSet::from([retained, pending]),
        "the id no head holds is dropped; the retained and current heads stay"
    );
}
