// SPDX-License-Identifier: AGPL-3.0-only

//! The compact kept archive (DEC-250): each import keeps one compressed,
//! versioned archive of its sealed physical stage instead of the raw stage;
//! a changed handback decodes it into one owned working stage and publishes
//! exactly what the legacy raw stage's handback publishes. Legacy bases stay
//! readable and are converted after their handback; every crash point leaves
//! the old or the new pair named, never half.

use super::*;

/// The vehicle's kept files: names in the kept directory and in its
/// `.staging`, sorted.
fn kept_files(fixture: &Fixture) -> (Vec<String>, Vec<String>) {
    let list = |path: std::path::PathBuf| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path)
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
    };
    let directory = physical_kept_dir(&fixture.store);
    (list(directory.clone()), list(directory.join(".staging")))
}

fn kept_record(fixture: &Fixture) -> crate::db::PhysicalV3Kept {
    fixture
        .store
        .physical_v3_kept(fixture.binding.vehicle_id)
        .expect("record")
        .expect("a kept base")
}

/// The record's kept pair, and nothing else, is in the kept directory.
fn assert_only_the_recorded_pair(fixture: &Fixture) {
    let vehicle = fixture.binding.vehicle_id;
    let kept = kept_record(fixture);
    let (files, staging) = kept_files(fixture);
    match &kept.archive {
        Some(archive) => {
            let mut expected = vec![
                kept_archive_name(vehicle, archive.generation),
                kept_generation_source_name(vehicle, archive.generation),
            ];
            expected.sort();
            assert_eq!(files, expected);
            assert!(staging.is_empty(), "no stage is kept: {staging:?}");
        }
        None => {
            assert_eq!(files, vec![format!("{vehicle}.source.json")]);
            assert_eq!(staging, vec![format!("{vehicle}.sqlite")]);
        }
    }
}

/// Turns the vehicle's kept archive into the legacy raw stage and source
/// file an earlier build kept, with the same logical digest, as a store
/// upgraded from that build holds.
pub(super) fn make_legacy(fixture: &Fixture) {
    let store = &fixture.store;
    let vehicle = fixture.binding.vehicle_id;
    let kept = kept_record(fixture);
    let archive = kept.archive.clone().expect("an archive");
    let (working, _) =
        open_kept_working(store, vehicle, &kept, &fixture.binding).expect("decoded");
    let stage = working.take();
    let path = stage.path().to_path_buf();
    drop(stage);
    std::fs::rename(&path, kept_stage_path(store, vehicle)).expect("the legacy stage");
    std::fs::copy(
        physical_kept_dir(store).join(kept_generation_source_name(vehicle, archive.generation)),
        legacy_kept_source_path(store, vehicle),
    )
    .expect("the legacy source");
    let legacy = crate::db::PhysicalV3Kept {
        archive: None,
        ..kept.clone()
    };
    assert!(
        store
            .swap_physical_v3_kept(&fixture.gate, vehicle, Some(&kept), Some(&legacy), false)
            .expect("legacy record")
    );
    remove_superseded_kept_files(store, vehicle, Some(&legacy));
    assert_only_the_recorded_pair(fixture);
}

/// Copies a directory tree with its permissions.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("directory");
    for entry in std::fs::read_dir(from).expect("read") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        let kind = entry.file_type().expect("type");
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).expect("copy");
        }
    }
    std::fs::set_permissions(to, std::fs::metadata(from).expect("mode").permissions())
        .expect("mode");
}

/// An identical second store: the same catalogue, packs and kept files.
fn clone_fixture(fixture: &Fixture) -> Fixture {
    let temporary = crate::private_tempdir().expect("copy");
    let root = temporary.path().join("store");
    copy_tree(fixture._temporary.path(), &root);
    let store = HubStore::open_existing(&root).expect("copied store");
    let gate = store.try_acquire_publication_gate().expect("gate");
    Fixture {
        _temporary: temporary,
        store,
        binding: fixture.binding.clone(),
        car: fixture.car.clone(),
        gate,
        cursor_key: CursorKey::from_bytes([41; 32]),
    }
}

/// What a handback published, without receipt ids or times: the head, its
/// packs and each changed set's packs and counts.
fn published_state(fixture: &Fixture) -> (Uuid, Vec<String>, Vec<(i64, String, i64, i64, i64)>) {
    let head = current_snapshot(fixture);
    let connection = fixture.store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT pack.ordinal, pack.sha256, transition.total_rows,
                    transition.total_uncompressed_bytes, transition.impacted_roots_count
               FROM physical_v3_delta_packs AS pack
               JOIN physical_v3_delta_transitions AS transition
                 ON transition.target_receipt_id = pack.target_receipt_id
              WHERE transition.vehicle_id = ?1
              ORDER BY transition.target_manifest_signed_sha256, pack.ordinal",
        )
        .expect("deltas");
    let deltas = statement
        .query_map([fixture.binding.vehicle_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })
        .expect("deltas")
        .collect::<Result<_, _>>()
        .expect("deltas");
    (head, head_packs(&fixture.store, head), deltas)
}

/// The handback planned from the kept archive publishes what the legacy
/// raw stage's handback publishes on an identical store: the same outcome,
/// head, packs and changed sets. The legacy base is converted after it.
fn assert_archive_matches_legacy(fixture: &Fixture) -> HandbackPhysicalOutcome {
    let legacy = clone_fixture(fixture);
    make_legacy(&legacy);
    reset_check_counters();
    let archive_outcome = handback(fixture);
    let decodes = crate::hub_pack::verifications()
        .iter()
        .filter(|verification| verification.place == "kept-archive-decode" && verification.ok)
        .count();
    assert_eq!(decodes, 1, "the changed handback decodes the archive once");
    let legacy_outcome = handback(&legacy);
    assert_eq!(archive_outcome, legacy_outcome);
    assert_eq!(published_state(fixture), published_state(&legacy));
    assert!(kept_record(&legacy).archive.is_some(), "the legacy base is converted");
    assert_eq!(kept_record(&legacy).stage, kept_record(fixture).stage);
    assert_only_the_recorded_pair(&legacy);
    assert_only_the_recorded_pair(fixture);
    archive_outcome
}

/// An import keeps one compact archive and its generation's source file;
/// no raw stage stays.
#[test]
fn handback_kept_archive_replaces_the_raw_kept_stage() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_kept(&fixture, root.path(), "base", &base_with_positions(), None);
    let kept = kept_record(&fixture);
    let archive = kept.archive.as_ref().expect("an archive");
    assert_eq!(archive.format, crate::source_db_stage::KEPT_ARCHIVE_FORMAT);
    assert!(archive.bytes < archive.decoded_bytes, "compressed");
    assert_only_the_recorded_pair(&fixture);
    // A second import replaces the pair and removes the first.
    publish_kept(&fixture, root.path(), "again", &base_with_positions(), None);
    let again = kept_record(&fixture);
    assert_ne!(again.archive.as_ref().unwrap().generation, archive.generation);
    assert_eq!(again.stage, kept.stage, "the same rows, the same logical digest");
    assert_only_the_recorded_pair(&fixture);
}

/// Old against new with a window's drive: the archive's head and changed
/// set are the legacy path's.
#[test]
fn handback_kept_archive_matches_the_legacy_path_with_a_changed_drive() {
    let (fixture, _root, env) = planned_then_refused(true);
    assert_eq!(
        assert_archive_matches_legacy(&fixture),
        published(PhysicalV3PublicationKind::Rotation)
    );
    assert!(current_head_has_a_changed_set(&fixture));
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("served");
    assert!(fresh.mirror.drives.contains_key(&(HUB_ID_BASE + 102)));
}

/// Old against new where coverage meets the kept stage's source children
/// across the seam: the covered source position goes on both paths.
#[test]
fn handback_kept_archive_matches_the_legacy_path_across_the_seam() {
    let (fixture, _root, env) = planned_then_refused(false);
    {
        let connection = fixture.store.open().expect("catalogue");
        connection
            .execute(
                "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
                 VALUES (3, 'grant-3', ?1, ?2, '[]', 'handback')",
                params![T - 8 * HOUR - MINUTE, T - 8 * HOUR + MINUTE],
            )
            .expect("coverage over a source position");
    }
    assert_eq!(
        assert_archive_matches_legacy(&fixture),
        published(PhysicalV3PublicationKind::Rotation)
    );
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("served");
    assert!(!fresh.mirror.positions.contains_key(&1003));
    assert!(fresh.mirror.positions.contains_key(&1001));
}

/// Old against new without recorded inputs: both plan in full and publish
/// the same unchanged head; the month retry then prepares it from the
/// archive alone.
#[test]
fn handback_kept_archive_matches_the_legacy_path_unchanged_and_month_retry() {
    let (fixture, _root, env) = planned_then_refused(false);
    let vehicle = fixture.binding.vehicle_id;
    let head = current_snapshot(&fixture);
    fixture
        .store
        .record_physical_v3_handback_plan(&fixture.gate, vehicle, None)
        .expect("forget the inputs");
    crate::physical_publication::FAIL_NEXT_PREPARATION.with(|fail| fail.set(true));
    assert_eq!(
        assert_archive_matches_legacy(&fixture),
        published(PhysicalV3PublicationKind::Unchanged)
    );
    assert_eq!(current_snapshot(&fixture), head);
    fixture
        .store
        .open()
        .expect("catalogue")
        .execute("DELETE FROM prepared_map_months WHERE vehicle_id = ?1", [vehicle.to_string()])
        .expect("the prepared map is missing");
    close_window(&fixture, 3, T + 4 * HOUR, false);
    reset_check_counters();
    assert_eq!(handback(&fixture), published(PhysicalV3PublicationKind::Unchanged));
    let log = crate::hub_pack::verifications();
    assert_eq!(
        log.iter().filter(|verification| verification.place == "kept-archive-decode").count(),
        1,
        "the month retry decodes the archive once: {log:#?}"
    );
    assert!(
        fixture.store.physical_v3_handback_plan(vehicle).expect("record").expect("inputs").prepared
    );
    PhysicalClient::bootstrap(&env, &fixture).expect("served");
}

/// The served head and a restating plan never decode the archive.
#[test]
fn handback_kept_archive_is_not_decoded_for_a_served_head() {
    let (fixture, _root, _env) = planned_then_refused(false);
    assert_eq!(handback(&fixture), published(PhysicalV3PublicationKind::Unchanged));
    assert_eq!(handback(&fixture), HandbackPhysicalOutcome::Current);
    assert!(
        !crate::hub_pack::verifications()
            .iter()
            .any(|verification| verification.place == "kept-archive-decode"),
        "no decode"
    );
}

/// A kept archive whose bytes changed, or that is missing, is refused: the
/// head stays refused, nothing is decoded into place.
#[test]
fn handback_kept_archive_refuses_a_corrupt_or_missing_archive() {
    for damage in ["flip", "truncate", "missing"] {
        let (fixture, _root, env) = planned_then_refused(true);
        let vehicle = fixture.binding.vehicle_id;
        let archive = kept_record(&fixture).archive.expect("an archive");
        let path = physical_kept_dir(&fixture.store).join(kept_archive_name(vehicle, archive.generation));
        let mut bytes = std::fs::read(&path).expect("archive");
        match damage {
            "flip" => {
                let middle = bytes.len() / 2;
                bytes[middle] ^= 0x55;
                std::fs::write(&path, &bytes).expect("damage");
            }
            "truncate" => std::fs::write(&path, &bytes[..bytes.len() - 7]).expect("damage"),
            _ => std::fs::remove_file(&path).expect("remove"),
        }
        let head = current_snapshot(&fixture);
        match handback(&fixture) {
            HandbackPhysicalOutcome::Withheld(reason) => {
                assert!(reason.contains("the kept archive"), "{damage}: {reason}");
            }
            other => panic!("{damage}: a damaged archive was planned: {other:?}"),
        }
        assert_eq!(current_snapshot(&fixture), head);
        assert!(kept_files(&fixture).1.is_empty(), "{damage}: no working stage is left");
        assert_eq!(
            PhysicalClient::bootstrap(&env, &fixture).err(),
            Some(axum::http::StatusCode::NOT_ACCEPTABLE)
        );
    }
}

/// Removes the interrupted compaction candidates a crash left in the kept
/// `.staging` (the census's orphan shape: a UUID no vehicle has), as its
/// reclaim does, and returns how many there were.
fn reclaim_orphan_candidates(fixture: &Fixture) -> usize {
    let (_, staging) = kept_files(fixture);
    let vehicle = format!("{}.sqlite", fixture.binding.vehicle_id);
    let mut removed = 0;
    for name in staging {
        if name == vehicle {
            continue;
        }
        let orphan = name
                .strip_suffix(".sqlite")
                .is_some_and(|stem| Uuid::parse_str(stem).is_ok());
        assert!(orphan, "an unexpected staging file: {name}");
        std::fs::remove_file(physical_kept_dir(&fixture.store).join(".staging").join(name))
            .expect("reclaim");
        removed += 1;
    }
    removed
}

fn crash_at(point: &'static str, run: impl FnOnce()) {
    crate::source_db_stage::KEPT_ARCHIVE_CRASH_AT.with(|at| at.set(Some(point)));
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
    crate::source_db_stage::KEPT_ARCHIVE_CRASH_AT.with(|at| at.set(None));
    assert!(crashed.is_err(), "the run crashed at {point}");
}

/// A legacy base is planned and then converted; a crash at any point of the
/// conversion leaves the legacy pair or the new archive pair named, never
/// half, and the next handback plans from it and tidies the other.
#[test]
fn handback_kept_archive_legacy_conversion_crash_points() {
    for point in ["candidate", "partial", "files", "swap"] {
        let (fixture, _root, env) = planned_then_refused(true);
        make_legacy(&fixture);
        let kept = kept_record(&fixture);
        let source = std::fs::read(legacy_kept_source_path(&fixture.store, fixture.binding.vehicle_id))
            .expect("legacy source");
        let legacy = SourceDbStage::open_sealed_to_copy(kept_stage_path(
            &fixture.store,
            fixture.binding.vehicle_id,
        ))
        .expect("legacy stage");
        crash_at(point, || {
            convert_legacy_kept(&fixture.store, &fixture.gate, &kept, &fixture.binding, &legacy, &source);
        });
        drop(legacy);
        assert_eq!(
            reclaim_orphan_candidates(&fixture),
            usize::from(matches!(point, "candidate" | "partial")),
            "{point}"
        );
        let after = kept_record(&fixture);
        assert_eq!(after.stage, kept.stage, "{point}");
        assert_eq!(after.archive.is_some(), point == "swap", "{point}: old or new, never half");
        // The restart's handback plans from the named pair and tidies.
        assert_eq!(handback(&fixture), published(PhysicalV3PublicationKind::Rotation), "{point}");
        assert!(kept_record(&fixture).archive.is_some(), "{point}: converted by now");
        assert_only_the_recorded_pair(&fixture);
        PhysicalClient::bootstrap(&env, &fixture).expect("served");
    }
}

/// An import's keep that crashes leaves no base (the record goes first,
/// never an older import's) or the new complete pair; the files it left are
/// removed by the next keep or handback.
#[test]
fn handback_kept_archive_import_keep_crash_points() {
    for point in ["candidate", "partial", "files", "swap"] {
        let fixture = fixture();
        let root = crate::private_tempdir().expect("stage root");
        publish_kept(&fixture, root.path(), "base", &base_with_positions(), None);
        let before = kept_record(&fixture);
        let history = base_with_positions();
        let projected = project_vehicle(&history, CAR).expect("projection");
        let mut stage = history_stage(root.path(), "next", &history);
        crash_at(point, || {
            keep_physical_source(
                &fixture.store,
                &fixture.gate,
                &fixture.binding,
                stage.stage_mut(),
                &projected.snapshot.drives,
                &projected.snapshot.charges,
            );
        });
        let after = fixture.store.physical_v3_kept(fixture.binding.vehicle_id).expect("record");
        match point {
            "swap" => {
                let after = after.expect("the new pair is named");
                assert_ne!(after.archive, before.archive);
                remove_superseded_kept_files(&fixture.store, fixture.binding.vehicle_id, Some(&after));
                assert_only_the_recorded_pair(&fixture);
            }
            _ => assert_eq!(after, None, "{point}: nothing to plan, never the older base"),
        }
        assert_eq!(
            reclaim_orphan_candidates(&fixture),
            usize::from(matches!(point, "candidate" | "partial")),
            "{point}"
        );
        let mut stage = history_stage(root.path(), "retry", &history);
        assert!(
            keep_physical_source(
                &fixture.store,
                &fixture.gate,
                &fixture.binding,
                stage.stage_mut(),
                &projected.snapshot.drives,
                &projected.snapshot.charges,
            )
            .is_some()
        );
        assert_only_the_recorded_pair(&fixture);
    }
}
