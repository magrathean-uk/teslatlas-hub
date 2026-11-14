// SPDX-License-Identifier: AGPL-3.0-only

//! The seam repair on the pack side (M03-T19, review REV-CX-M03-T16): the
//! negotiated 1.4 route refuses while it is unplanned, already packed
//! positions follow changed seam decisions, a source deletion never removes a
//! session the seam keeps, and the seam's sync becomes visible only together
//! with the import that carries its replacements.

use super::*;

/// Every entity of `projected` recorded into a successor capture, in the
/// order the pack sink records a fragment, then sealed.
fn capture_everything(
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

/// One later import of `history`, planned, written and published as the
/// importer does.
fn import(
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
fn window(fixture: &Fixture, seq: i64, start_ms: i64, end_ms: i64) {
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
fn hub_rows(fixture: &Fixture, drives: &[(i64, i64, i64)], positions: &[(i64, i64, i64)]) {
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
fn client(fixture: &Fixture) -> Client {
    let mut client = Client::default();
    for (_, rows) in heads(&fixture.store, fixture.binding.vehicle_id) {
        client.apply(&rows);
    }
    client
}

/// Every position a client holds belongs to a drive the client holds.
fn assert_positions_have_their_drives(client: &Client, at: &str) {
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
fn rows_since(fixture: &Fixture, from: usize) -> PackRows {
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
fn assert_replay_publishes_nothing(fixture: &Fixture, history: &SourceDbHistory, at_ms: i64) {
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
fn served(
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

/// Review finding 1, the importer's side: the physical stage the 1.4 route
/// would publish is withheld, and discarded, once the Hub has collected the
/// vehicle; before that it is published as it is.
#[test]
fn seam_repair_packs_physical_stage_is_withheld_for_a_seam_vehicle() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let root = crate::private_tempdir().expect("stage root");
    let kept = physical_stage_for_publication(
        &fixture.store,
        vehicle_id,
        Some(physical_stage(root.path().join("before"))),
    )
    .expect("route");
    assert!(kept.is_some(), "before the Hub collects, the stage is published");
    drop(kept);

    collect(&fixture);
    let path = root.path().join("after");
    let withheld =
        physical_stage_for_publication(&fixture.store, vehicle_id, Some(physical_stage(path.clone())))
            .expect("route");
    assert!(withheld.is_none(), "after the Hub collects, the stage is withheld");
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
