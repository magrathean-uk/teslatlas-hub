// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #60: an import interrupted between its lineage commit and its
//! physical publication must not leave the two phone routes serving
//! different history. The import records a capture and an intent before it
//! commits; the commit makes it the vehicle's import revision, which refuses
//! the older physical head; a restart finishes the publication from the
//! capture alone. Each case stops at a durable boundary, drops everything
//! the run held in memory and resumes with only the store, the cursor key
//! and the publication gate: no source.

use std::path::Path;

use super::seam_physical_route::{PhysicalClient, history_stage, paired, publish_physical};
use super::seam_repair_packs::{capture_everything, client};
use super::*;

/// The schema-2.2 source facts of the selected car, as a direct capture
/// leaves them.
fn updates_source() -> DirectUpdatesSourceV2_2 {
    DirectUpdatesSourceV2_2 {
        postgres_snapshot_sha256: crate::updates_logical::hex_sha256(Uuid::new_v4().as_bytes()),
        schema: crate::source_db_reader::SourceDbSchemaInfo {
            observed_migration_version: crate::source_db_schema::MAX_VALIDATED_MIGRATION,
            observed_migration_count: crate::source_db_schema::SOURCE_DB_V4_MIGRATION_COUNT,
            minimum_supported_migration_version: crate::source_db_schema::MIN_SUPPORTED_MIGRATION,
            maximum_validated_migration_version: crate::source_db_schema::MAX_VALIDATED_MIGRATION,
            pinned_source_revision: crate::source_db_schema::SOURCE_DB_V4_SOURCE_REVISION,
            pinned_migration_set_sha256: crate::source_db_schema::SOURCE_DB_V4_MIGRATION_SET_SHA256,
            fingerprint: crate::updates_logical::hex_sha256(b"design-later-60-schema"),
        },
        global_settings: crate::source_db_projection::SourceDbSettingsPhysicalV2_2 {
            id: 1,
            unit_of_length: crate::hub_pack::ProjectionUnitOfLengthV2_2::Kilometers,
            unit_of_temperature: crate::hub_pack::ProjectionUnitOfTemperatureV2_2::Celsius,
            unit_of_pressure: crate::hub_pack::ProjectionUnitOfPressureV2_2::Bar,
            preferred_range: crate::hub_pack::ProjectionPreferredRangeV2_2::Rated,
            base_url: None,
            grafana_url: None,
            language: "en".into(),
            theme_mode: "system".into(),
            inserted_at_pg_us: 0,
            updated_at_pg_us: 0,
        },
        car: crate::source_db_projection::SourceDbCarPhysicalV2_2 {
            id: 1,
            eid: 88,
            vid: 99,
            vin: Some("5YJTESTVIN1234567".into()),
            name: Some("Road car".into()),
            model: Some("Model 3".into()),
            efficiency: Some(0.145),
            trim_badging: Some("74d".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            display_priority: 0,
            inserted_at_pg_us: 0,
            updated_at_pg_us: 0,
            settings_id: 1,
        },
        car_settings: crate::source_db_projection::SourceDbCarSettingsPhysicalV2_2 {
            id: 1,
            suspend_min: 21,
            suspend_after_idle_min: 15,
            req_not_unlocked: false,
            free_supercharging: false,
            use_streaming_api: true,
            enabled: true,
            lfp_battery: false,
        },
        updates: Vec::new(),
    }
}

/// One import of `history` up to its lineage commit: the capture and its
/// intent first, then the planned successor. Returns the intent, the stage
/// the run still holds and the seam the commit proved.
fn commit_captured_import(
    fixture: &Fixture,
    root: &Path,
    name: &str,
    history: &SourceDbHistory,
    at_ms: i64,
) -> (
    crate::db::ImportPublicationIntent,
    crate::source_db_stage::OwnedSourceDbStage,
    crate::import::source_db::seam::SeamPackPlan,
) {
    let projected = project_vehicle(history, CAR).expect("projection");
    let mut planned = plan(fixture, &projected);
    let run_id = begin_run(fixture, at_ms);
    let mut stage = history_stage(root, name, history);
    let intent = capture_import_publication(
        &fixture.store,
        &fixture.gate,
        run_id,
        &fixture.binding,
        stage.stage_mut(),
        &projected.snapshot.drives,
        &projected.snapshot.charges,
        &updates_source(),
    )
    .expect("capture before the commit");
    let capture = capture_everything(fixture, run_id, &projected);
    publish_captured(fixture, &projected, run_id, capture, &mut planned).expect("lineage commit");
    (intent, stage, planned.seam.packs)
}

fn resume(fixture: &Fixture) -> Vec<(Uuid, ImportPublicationOutcome)> {
    resume_import_publications(&fixture.store, &fixture.cursor_key, &fixture.gate)
        .expect("resume")
}

fn capture_files(fixture: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(import_publication_dir(&fixture.store))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Runs `run` with the fixture's publication gate released (a backup or a
/// reclamation takes the gate itself), then takes the gate back.
fn with_gate_released<T>(fixture: Fixture, run: impl FnOnce(&HubStore) -> T) -> (Fixture, T) {
    let Fixture { _temporary, store, binding, car, gate, cursor_key } = fixture;
    drop(gate);
    let result = run(&store);
    let gate = store.try_acquire_publication_gate().expect("gate taken back");
    (Fixture { _temporary, store, binding, car, gate, cursor_key }, result)
}

/// The 1.4 client and the schema-2.1 lineage hold the same drives.
fn assert_routes_agree(fixture: &Fixture, at: &str) {
    let env = paired(fixture);
    let physical = PhysicalClient::bootstrap(&env, fixture)
        .unwrap_or_else(|status| panic!("{at}: the 1.4 head is served, not {status}"));
    let lineage = client(fixture);
    assert_eq!(physical.mirror.drives, lineage.drives, "{at}: both routes hold one history");
}

/// The issue's case without the seam and with unchanged coverage: a
/// successor with three more drives commits, the run ends before its
/// physical publication, and the reopened Hub refuses the older 1.4 head
/// instead of serving it beside the newer schema-2.1 lineage; the restart
/// then finishes schema 2.2 and the physical head from the capture, and
/// both routes serve the same drives. A second restart changes nothing.
#[test]
fn design_later_60_interrupted_reimport_serves_one_history_on_both_routes_after_restart() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
        .expect("base head")
        .expect("served base");
    assert_routes_agree(&fixture, "before the re-import");

    let (intent, stage, _) =
        commit_captured_import(&fixture, root.path(), "successor", &successor_history(), T + 3 * HOUR);
    drop(stage);
    assert!(!fixture.store.import_seam_present(fixture.binding.vehicle_id).expect("seam"));
    assert_eq!(client(&fixture).drives.len(), 4, "the lineage committed the three new drives");
    let env = paired(&fixture);
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE),
        "the older 1.4 head is refused once the newer import committed"
    );
    assert!(
        fixture
            .store
            .import_publication_intent(fixture.binding.vehicle_id)
            .expect("intent")
            .is_some_and(|recorded| recorded.committed && recorded.run_id == intent.run_id)
    );

    let outcomes = resume(&fixture);
    assert!(
        matches!(outcomes.as_slice(), [(vehicle, ImportPublicationOutcome::Finished(Some(_)))]
            if *vehicle == fixture.binding.vehicle_id),
        "{outcomes:?}"
    );
    assert_routes_agree(&fixture, "after the restart");
    assert!(
        production_updates_head(&fixture.store, fixture.binding.vehicle_id)
            .expect("schema-2.2 head")
            .is_some(),
        "schema 2.2 was finished from the capture"
    );
    assert_eq!(
        fixture.store.import_publication_intent(fixture.binding.vehicle_id).expect("intent"),
        None
    );
    assert!(capture_files(&fixture).is_empty(), "{:?}", capture_files(&fixture));
    assert!(resume(&fixture).is_empty(), "a second restart has nothing to finish");
    assert_routes_agree(&fixture, "after a second restart");
}

/// A run that ends after its capture but before its commit changed no
/// route: the older head keeps being served, and the restart drops the
/// intent and its capture.
#[test]
fn design_later_60_interruption_before_the_commit_drops_the_capture_and_keeps_the_head() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
        .expect("base head")
        .expect("served base");
    let history = successor_history();
    let projected = project_vehicle(&history, CAR).expect("projection");
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    let mut stage = history_stage(root.path(), "uncommitted", &history);
    capture_import_publication(
        &fixture.store,
        &fixture.gate,
        run_id,
        &fixture.binding,
        stage.stage_mut(),
        &projected.snapshot.drives,
        &projected.snapshot.charges,
        &updates_source(),
    )
    .expect("capture");
    drop(stage);
    assert!(!capture_files(&fixture).is_empty());
    assert_routes_agree(&fixture, "an uncommitted capture changes no route");

    assert_eq!(
        resume(&fixture),
        vec![(fixture.binding.vehicle_id, ImportPublicationOutcome::Abandoned)]
    );
    assert_eq!(
        fixture.store.import_publication_intent(fixture.binding.vehicle_id).expect("intent"),
        None
    );
    assert!(capture_files(&fixture).is_empty(), "{:?}", capture_files(&fixture));
    assert_routes_agree(&fixture, "after the restart");

    // An aborted generation takes its uncommitted intent with it.
    let run_id = begin_run(&fixture, T + 4 * HOUR);
    let mut stage = history_stage(root.path(), "aborted", &history);
    capture_import_publication(
        &fixture.store,
        &fixture.gate,
        run_id,
        &fixture.binding,
        stage.stage_mut(),
        &projected.snapshot.drives,
        &projected.snapshot.charges,
        &updates_source(),
    )
    .expect("capture");
    fixture.store.abort_import_generation(run_id).expect("abort");
    assert_eq!(
        fixture.store.import_publication_intent(fixture.binding.vehicle_id).expect("intent"),
        None
    );
}

/// A missing or damaged capture is refused clearly at the restart: the
/// older head stays refused rather than served as current, and the next
/// import, which records its own capture, publishes a coherent head.
#[test]
fn design_later_60_missing_or_damaged_capture_is_a_clear_recoverable_refusal() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
        .expect("base head")
        .expect("served base");
    let history = successor_history();
    let (intent, stage, _) =
        commit_captured_import(&fixture, root.path(), "successor", &history, T + 3 * HOUR);
    drop(stage);
    let generation = intent.kept.archive.as_ref().expect("archive").generation;
    let updates = import_publication_dir(&fixture.store)
        .join(import_publication_updates_name(fixture.binding.vehicle_id, generation));
    std::fs::write(&updates, b"{}").expect("damage the capture");

    let outcomes = resume(&fixture);
    let [(_, ImportPublicationOutcome::Refused(reason))] = outcomes.as_slice() else {
        panic!("a damaged capture is refused: {outcomes:?}");
    };
    assert!(reason.contains("schema-2.2 facts differ"), "{reason}");
    let env = paired(&fixture);
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE),
        "the older head is not served after the refusal"
    );
    std::fs::remove_file(&updates).expect("lose the capture");
    let outcomes = resume(&fixture);
    let [(_, ImportPublicationOutcome::Refused(reason))] = outcomes.as_slice() else {
        panic!("a missing capture is refused: {outcomes:?}");
    };
    assert!(reason.contains("updates.json"), "{reason}");

    // The next import supersedes the refused intent with its own capture.
    let (intent, stage, seam) =
        commit_captured_import(&fixture, root.path(), "recovery", &history, T + 4 * HOUR);
    finish_import_physical(
        &fixture.store,
        &fixture.cursor_key,
        &fixture.gate,
        &intent,
        Some(stage),
        Some(&seam),
    )
    .expect("publication")
    .expect("physical head");
    assert_routes_agree(&fixture, "after the recovering import");
    assert!(resume(&fixture).is_empty());
}

/// With the seam (a window collected before the re-import), the run ends
/// after the kept record names the capture, or after the head is published
/// but before the intent is cleared: each restart finishes idempotently and
/// both routes agree; the later handback plans the adopted base.
#[test]
fn design_later_60_seam_interruptions_after_adoption_and_publication_resume_idempotently() {
    for point in ["adopted", "published"] {
        let fixture = fixture();
        let root = crate::private_tempdir().expect("stage root");
        publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
            .expect("base head")
            .expect("served base");
        collect(&fixture);
        let history = successor_history();
        let (intent, stage, seam) =
            commit_captured_import(&fixture, root.path(), point, &history, T + 3 * HOUR);
        assert!(fixture.store.import_seam_present(fixture.binding.vehicle_id).expect("seam"));
        IMPORT_PUBLICATION_CRASH_AT.with(|at| at.set(Some(point)));
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            finish_import_physical(
                &fixture.store,
                &fixture.cursor_key,
                &fixture.gate,
                &intent,
                Some(stage),
                Some(&seam),
            )
        }));
        assert!(crashed.is_err(), "{point}: the run ended at the boundary");
        assert!(
            fixture.store.import_publication_intent(fixture.binding.vehicle_id).expect("intent").is_some(),
            "{point}: the intent survives the interruption"
        );
        let outcomes = resume(&fixture);
        assert!(
            matches!(outcomes.as_slice(), [(_, ImportPublicationOutcome::Finished(Some(_)))]),
            "{point}: {outcomes:?}"
        );
        assert_routes_agree(&fixture, point);
        assert!(resume(&fixture).is_empty(), "{point}");
        let revision = fixture
            .store
            .physical_v3_import_revision(fixture.binding.vehicle_id)
            .expect("revision")
            .expect("recorded");
        assert_eq!(revision.run_id, intent.run_id);
        assert_eq!(
            revision.kept,
            intent.kept.archive.as_ref().map(|archive| archive.generation),
            "{point}: the adopted capture is the kept base"
        );
        assert_eq!(
            publish_physical_after_handback(&fixture.store, &fixture.cursor_key, &fixture.gate)
                .expect("handback"),
            vec![(fixture.binding.vehicle_id, HandbackPhysicalOutcome::Current)],
            "{point}: the handback finds the import's head current"
        );
    }
}

/// Review finding 1: a backup taken after a final import committed but
/// before its publication finished would carry the intent and the revision
/// without the capture, so the restored Hub could never finish it. Both
/// formats share the refusal at `begin_backup_snapshot`, under the gate,
/// naming the vehicle; once the restart finishes the publication the backup
/// goes ahead.
#[test]
fn design_later_60_backup_waits_for_a_pending_import_publication() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
        .expect("base head")
        .expect("served base");
    let (_, stage, _) =
        commit_captured_import(&fixture, root.path(), "successor", &successor_history(), T + 3 * HOUR);
    drop(stage);
    let vehicle = fixture.binding.vehicle_id.to_string();
    let outside = crate::private_tempdir().expect("backup root");
    let refused = outside.path().join("refused");
    let (fixture, errors) = with_gate_released(fixture, |store| {
        [
            store.begin_backup_snapshot().err().map(|error| error.to_string()),
            crate::data_recovery::create_data_backup(store, &refused)
                .err()
                .map(|error| error.to_string()),
        ]
    });
    for error in errors {
        let error = error.expect("the backup waits for the publication");
        assert!(error.contains(&vehicle), "{error}");
        assert!(error.contains("finishes the publication locally"), "{error}");
    }
    assert!(!refused.exists(), "nothing is published");

    assert!(
        matches!(resume(&fixture).as_slice(), [(_, ImportPublicationOutcome::Finished(Some(_)))]),
        "the restart finishes the publication"
    );
    let finished = outside.path().join("finished");
    let (_fixture, created) = with_gate_released(fixture, |store| {
        crate::data_recovery::create_data_backup(store, &finished).map(|_| ())
    });
    created.expect("the backup goes ahead once the publication finished");
}

/// Runs `finish_import_physical` for `intent` until the simulated crash at
/// `point`.
fn finish_until_crash(
    fixture: &Fixture,
    intent: &crate::db::ImportPublicationIntent,
    stage: crate::source_db_stage::OwnedSourceDbStage,
    point: &'static str,
) {
    IMPORT_PUBLICATION_CRASH_AT.with(|at| at.set(Some(point)));
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        finish_import_physical(
            &fixture.store,
            &fixture.cursor_key,
            &fixture.gate,
            intent,
            Some(stage),
            None,
        )
    }));
    assert!(crashed.is_err(), "the run ended at {point}");
}

/// The capture's archive and source sessions in the kept directory.
fn kept_capture_paths(
    fixture: &Fixture,
    intent: &crate::db::ImportPublicationIntent,
) -> [std::path::PathBuf; 2] {
    let vehicle = fixture.binding.vehicle_id;
    let generation = intent.kept.archive.as_ref().expect("archive").generation;
    let kept = physical_kept_dir(&fixture.store);
    [
        kept.join(kept_archive_name(vehicle, generation)),
        kept.join(kept_generation_source_name(vehicle, generation)),
    ]
}

/// Review finding 2: the run ends after its capture was moved into the kept
/// directory but before the kept swap names it. Neither `storage-census
/// --reclaim` nor the kept-base pruning a hand-back or a conversion runs may
/// unlink it: the pending intent holds it, and the restart finishes the
/// publication from it.
#[test]
fn design_later_60_reclaim_and_kept_sweeps_hold_a_capture_moved_before_its_swap() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
        .expect("base head")
        .expect("served base");
    let (intent, stage, _) =
        commit_captured_import(&fixture, root.path(), "successor", &successor_history(), T + 3 * HOUR);
    finish_until_crash(&fixture, &intent, stage, "moved");
    let moved = kept_capture_paths(&fixture, &intent);
    assert!(moved.iter().all(|path| path.is_file()), "both files were moved");
    assert_eq!(fixture.store.physical_v3_kept(fixture.binding.vehicle_id).expect("kept"), None);

    let (fixture, reclaimed) = with_gate_released(fixture, |store| store.reclaim_storage());
    let report = reclaimed.expect("reclaim");
    assert!(report.before.import_bases.files >= 2, "{report:?}");
    forget_physical_kept(&fixture.store, &fixture.gate, fixture.binding.vehicle_id)
        .expect("kept-base pruning");
    remove_superseded_kept_files(&fixture.store, fixture.binding.vehicle_id, None);
    assert!(moved.iter().all(|path| path.is_file()), "the held capture survived every sweep");

    let outcomes = resume(&fixture);
    assert!(
        matches!(outcomes.as_slice(), [(_, ImportPublicationOutcome::Finished(Some(_)))]),
        "{outcomes:?}"
    );
    assert_routes_agree(&fixture, "after the restart");
}

/// Review finding 5: a first import ends after its capture became the kept
/// base but before any physical head exists, and the restart's publication
/// is refused for a reason that passes (here its schema-2.2 facts are
/// briefly out of reach). The hand-back after it must not prune the kept
/// base the pending intent needs; once the reason passes, the next restart
/// finishes the publication.
#[test]
fn design_later_60_handback_keeps_the_adopted_capture_of_a_pending_first_head() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    let vehicle = fixture.binding.vehicle_id;
    let (intent, stage, _) =
        commit_captured_import(&fixture, root.path(), "first", &successor_history(), T + 3 * HOUR);
    finish_until_crash(&fixture, &intent, stage, "adopted");
    assert_eq!(
        fixture.store.physical_v3_kept(vehicle).expect("kept").as_ref(),
        Some(&intent.kept),
        "the capture is the kept base"
    );
    assert_eq!(fixture.store.physical_v3_current_head_served(vehicle).expect("head"), None);

    let generation = intent.kept.archive.as_ref().expect("archive").generation;
    let updates = import_publication_dir(&fixture.store)
        .join(import_publication_updates_name(vehicle, generation));
    let aside = root.path().join("updates-aside.json");
    std::fs::rename(&updates, &aside).expect("facts out of reach");
    let outcomes =
        publish_physical_after_handback(&fixture.store, &fixture.cursor_key, &fixture.gate)
            .expect("handback");
    assert!(
        matches!(outcomes.as_slice(), [(id, HandbackPhysicalOutcome::Withheld(_))] if *id == vehicle),
        "{outcomes:?}"
    );
    assert!(
        kept_capture_paths(&fixture, &intent).iter().all(|path| path.is_file()),
        "the hand-back kept the pending capture"
    );
    assert_eq!(
        fixture.store.physical_v3_kept(vehicle).expect("kept").as_ref(),
        Some(&intent.kept)
    );

    std::fs::rename(&aside, &updates).expect("facts back");
    let outcomes = resume(&fixture);
    assert!(
        matches!(outcomes.as_slice(), [(_, ImportPublicationOutcome::Finished(Some(_)))]),
        "{outcomes:?}"
    );
    assert_routes_agree(&fixture, "after the restart");
}

/// Review finding 6: the archive is moved but moving the source sessions
/// fails. The run reports the failure, not a publication: the intent and
/// both capture files stay, the older head stays refused, and the restart
/// adopts the capture again and finishes the publication.
#[test]
fn design_later_60_failed_adoption_keeps_the_capture_and_reports_no_publication() {
    use std::os::unix::fs::DirBuilderExt;
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    let vehicle = fixture.binding.vehicle_id;
    publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
        .expect("base head")
        .expect("served base");
    let (intent, stage, seam) =
        commit_captured_import(&fixture, root.path(), "successor", &successor_history(), T + 3 * HOUR);
    let [archive, source] = kept_capture_paths(&fixture, &intent);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&source)
        .expect("a directory in the way of the source sessions");

    let error = finish_import_physical(
        &fixture.store,
        &fixture.cursor_key,
        &fixture.gate,
        &intent,
        Some(stage),
        Some(&seam),
    )
    .expect_err("a failed adoption is not a publication");
    assert!(
        matches!(&error, SourceDbImportError::PublicationAdoption { vehicle_id, .. } if *vehicle_id == vehicle),
        "{error}"
    );
    assert!(archive.is_file(), "the moved archive stays");
    let generation = intent.kept.archive.as_ref().expect("archive").generation;
    assert!(
        import_publication_dir(&fixture.store)
            .join(kept_generation_source_name(vehicle, generation))
            .is_file(),
        "the unmoved source sessions stay"
    );
    assert!(
        fixture
            .store
            .import_publication_intent(vehicle)
            .expect("intent")
            .is_some_and(|recorded| recorded.committed && recorded.run_id == intent.run_id),
        "the committed intent stays"
    );
    let env = paired(&fixture);
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE),
        "the older head stays refused"
    );

    std::fs::remove_dir(&source).expect("the obstacle goes");
    let outcomes = resume(&fixture);
    assert!(
        matches!(outcomes.as_slice(), [(_, ImportPublicationOutcome::Finished(Some(_)))]),
        "{outcomes:?}"
    );
    assert_routes_agree(&fixture, "after the restart");
}

/// Review finding 7 (the P3): a run killed while compacting its capture leaves a
/// raw candidate in the capture staging folder that no intent names. The
/// next sweep under the gate removes the candidate and its SQLite side
/// files, and leaves any other name.
#[test]
fn design_later_60_interrupted_capture_candidate_is_swept() {
    let fixture = fixture();
    let staging = import_publication_dir(&fixture.store).join(".staging");
    std::fs::create_dir_all(&staging).expect("capture staging");
    let candidate = format!("{}.sqlite", Uuid::new_v4());
    for name in [candidate.clone(), format!("{candidate}-journal")] {
        std::fs::write(staging.join(name), b"raw stage").expect("candidate");
    }
    std::fs::write(staging.join("notes.txt"), b"other").expect("other name");

    assert!(resume(&fixture).is_empty());
    let mut left: Vec<String> = std::fs::read_dir(&staging)
        .expect("staging")
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();
    left.sort();
    assert_eq!(left, vec!["notes.txt".to_owned()]);
}
