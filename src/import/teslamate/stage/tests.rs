// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Barrier},
};

use serde::{Deserialize, Serialize, ser::SerializeStruct};
use tempfile::tempdir;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Row {
    label: String,
    ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ChargeRow {
    charging_process_id: i64,
    label: String,
}

fn limits() -> TeslaMateStageLimits {
    TeslaMateStageLimits {
        max_rows: 10,
        max_stage_bytes: 512 * 1024,
        minimum_free_bytes: 0,
    }
}

fn private_imports(temporary: &tempfile::TempDir) -> PathBuf {
    temporary.path().join("imports")
}

#[test]
fn encoding_workers_are_bounded() {
    assert_eq!(stage_encoding_worker_count_for(0), 1);
    assert_eq!(stage_encoding_worker_count_for(1), 1);
    assert_eq!(stage_encoding_worker_count_for(4), 4);
    assert_eq!(stage_encoding_worker_count_for(64), MAX_ENCODING_WORKERS);
}

#[derive(Clone)]
struct BarrierRow {
    barrier: Arc<Barrier>,
    ordinal: u32,
}

impl Serialize for BarrierRow {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.barrier.wait();
        let mut row = serializer.serialize_struct("BarrierRow", 1)?;
        row.serialize_field("ordinal", &self.ordinal)?;
        row.end()
    }
}

#[test]
fn parallel_encoding_uses_multiple_workers_and_keeps_input_order() {
    let barrier = Arc::new(Barrier::new(2));
    let rows = vec![
        (
            2,
            BarrierRow {
                barrier: Arc::clone(&barrier),
                ordinal: 2,
            },
        ),
        (
            1,
            BarrierRow {
                barrier,
                ordinal: 1,
            },
        ),
    ];
    let encoded = encode_rows_parallel(rows, 2).expect("parallel encoding");
    assert_eq!(
        encoded.iter().map(|row| row.0).collect::<Vec<_>>(),
        vec![2, 1]
    );
}

#[test]
fn parallel_insert_preserves_deterministic_stored_order() {
    let temporary = tempdir().expect("temp dir");
    let mut stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    stage
        .insert_page_parallel(
            TeslaMateStageTable::Cars,
            vec![
                (
                    3,
                    Row {
                        label: "c".into(),
                        ordinal: 3,
                    },
                ),
                (
                    1,
                    Row {
                        label: "a".into(),
                        ordinal: 1,
                    },
                ),
                (
                    2,
                    Row {
                        label: "b".into(),
                        ordinal: 2,
                    },
                ),
            ],
        )
        .expect("insert");
    stage.seal().expect("seal");
    let page = stage
        .page::<Row>(TeslaMateStageTable::Cars, 0, 10)
        .expect("page");
    assert_eq!(
        page.rows
            .iter()
            .map(|row| row.source_id)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[test]
fn schema_commit_fault_never_creates_a_readable_sealed_stage() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    let temporary = tempdir().expect("temp dir");
    let imports = private_imports(&temporary);
    let error = {
        let _fault = inject(DurabilityFaultPoint::StageSchemaCommit);
        TeslaMateStage::create(&imports, limits()).expect_err("schema commit fault")
    };
    assert!(matches!(error, TeslaMateStageError::Durability(_)));

    let staging = imports.join(STAGING_DIRECTORY);
    let candidates = fs::read_dir(&staging)
        .expect("staging directory")
        .map(|entry| entry.expect("stage entry").path())
        .collect::<Vec<_>>();
    assert_eq!(candidates.len(), 1, "failed schema remains recoverable");
    assert!(matches!(
        TeslaMateStage::open_sealed(&candidates[0]),
        Err(TeslaMateStageError::MissingMetadata(META_STATE))
    ));
}

#[test]
fn page_commit_fault_rolls_back_rows_and_accounting() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    let temporary = tempdir().expect("temp dir");
    let mut stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    let row = Row {
        label: "candidate".into(),
        ordinal: 1,
    };
    let error = {
        let _fault = inject(DurabilityFaultPoint::StagePageCommit);
        stage
            .insert(TeslaMateStageTable::Cars, 1, &row)
            .expect_err("page commit fault")
    };
    assert!(matches!(error, TeslaMateStageError::Durability(_)));
    assert_eq!(stage.stats().expect("rolled back stats").row_count, 0);

    stage
        .insert(TeslaMateStageTable::Cars, 1, &row)
        .expect("retry page");
    stage.seal().expect("seal retry");
    let page = stage
        .page::<Row>(TeslaMateStageTable::Cars, 0, 10)
        .expect("committed page");
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].value, row);
}

#[test]
fn seal_commit_fault_leaves_the_stage_open_for_retry() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    let temporary = tempdir().expect("temp dir");
    let mut stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    let error = {
        let _fault = inject(DurabilityFaultPoint::StageSealCommit);
        stage.seal().expect_err("seal commit fault")
    };
    assert!(matches!(error, TeslaMateStageError::Durability(_)));
    assert_eq!(
        stage.stats().expect("open stats").state,
        TeslaMateStageState::Open
    );
    assert_eq!(
        stage.seal().expect("seal retry").state,
        TeslaMateStageState::Sealed
    );
}

#[test]
fn discard_faults_preserve_or_remove_only_the_admitted_stage() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    let temporary = tempdir().expect("temp dir");
    let stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    let path = stage.path().to_path_buf();
    let error = {
        let _fault = inject(DurabilityFaultPoint::StageDiscardUnlink);
        stage.discard().expect_err("unlink fault")
    };
    assert!(matches!(error, TeslaMateStageError::Durability(_)));
    assert!(path.is_file(), "pre-unlink fault preserves stage");

    let stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    let removed_path = stage.path().to_path_buf();
    let error = {
        let _fault = inject(DurabilityFaultPoint::StageDiscardDirectoryFsync);
        stage.discard().expect_err("directory fsync fault")
    };
    assert!(matches!(error, TeslaMateStageError::Durability(_)));
    assert!(
        matches!(fs::symlink_metadata(&removed_path), Err(source) if source.kind() == std::io::ErrorKind::NotFound),
        "post-unlink fault never restores a different path"
    );
}

struct PanickingRow;

impl Serialize for PanickingRow {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        panic!("test worker panic");
    }
}

#[test]
fn parallel_encoding_reports_worker_panics() {
    let error = encode_rows_parallel(vec![(1, PanickingRow)], 1).expect_err("panic error");
    assert!(matches!(error, TeslaMateStageError::EncodingWorkerPanicked));
}

#[test]
fn stages_typed_rows_in_private_paths_and_pages_only_when_sealed() {
    let temporary = tempdir().expect("temp dir");
    let imports = temporary.path().join("imports");
    let mut stage = TeslaMateStage::create(&imports, limits()).expect("stage");
    let path = stage.path().to_path_buf();
    assert_eq!(
        fs::metadata(&imports)
            .expect("imports mode")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(imports.join(STAGING_DIRECTORY))
            .expect("staging mode")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path)
            .expect("stage mode")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    stage
        .insert(
            TeslaMateStageTable::Positions,
            1,
            &Row {
                label: "first".to_owned(),
                ordinal: 1,
            },
        )
        .expect("first row");
    stage
        .insert(
            TeslaMateStageTable::Positions,
            2,
            &Row {
                label: "second".to_owned(),
                ordinal: 2,
            },
        )
        .expect("second row");
    stage
        .insert(
            TeslaMateStageTable::Positions,
            3,
            &Row {
                label: "third".to_owned(),
                ordinal: 3,
            },
        )
        .expect("third row");
    stage
        .insert(
            TeslaMateStageTable::Charges,
            11,
            &ChargeRow {
                charging_process_id: 7,
                label: "first sample".to_owned(),
            },
        )
        .expect("first sample");
    stage
        .insert(
            TeslaMateStageTable::Charges,
            12,
            &ChargeRow {
                charging_process_id: 7,
                label: "second sample".to_owned(),
            },
        )
        .expect("second sample");
    assert!(matches!(
        stage.page::<Row>(TeslaMateStageTable::Positions, 0, 2),
        Err(TeslaMateStageError::StageNotSealed)
    ));

    let stats = stage.seal().expect("sealed");
    assert_eq!(stats.state, TeslaMateStageState::Sealed);
    assert_eq!(stats.row_count, 5);
    stage.verify_integrity().expect("integrity");
    let first = stage
        .page::<Row>(TeslaMateStageTable::Positions, 0, 2)
        .expect("first page");
    assert_eq!(
        first
            .rows
            .iter()
            .map(|row| row.source_id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(first.next_after_id, Some(2));
    let second = stage
        .page::<Row>(
            TeslaMateStageTable::Positions,
            first.next_after_id.expect("cursor"),
            2,
        )
        .expect("second page");
    assert_eq!(
        second
            .rows
            .iter()
            .map(|row| row.source_id)
            .collect::<Vec<_>>(),
        [3]
    );
    assert_eq!(second.next_after_id, None);
    assert_eq!(
        stage
            .get::<ChargeRow>(TeslaMateStageTable::Charges, 11)
            .expect("sample lookup")
            .expect("sample"),
        ChargeRow {
            charging_process_id: 7,
            label: "first sample".to_owned(),
        }
    );
    let samples = stage
        .charge_samples_for_process::<ChargeRow>(7, 0, 1)
        .expect("sample page");
    assert_eq!(samples.rows.len(), 1);
    assert_eq!(samples.rows[0].source_id, 11);
    assert_eq!(samples.next_after_id, Some(11));
    assert!(matches!(
        stage.insert(
            TeslaMateStageTable::Positions,
            4,
            &Row {
                label: "forbidden".to_owned(),
                ordinal: 4,
            }
        ),
        Err(TeslaMateStageError::StageSealed)
    ));
    drop(stage);

    let mut reopened = TeslaMateStage::open_sealed(path).expect("reopened sealed");
    assert_eq!(reopened.stats().expect("stats").row_count, 5);
    assert!(matches!(
        reopened.insert(
            TeslaMateStageTable::Positions,
            4,
            &Row {
                label: "forbidden".to_owned(),
                ordinal: 4,
            }
        ),
        Err(TeslaMateStageError::StageReadOnly)
    ));
}

#[test]
fn charge_sample_page_uses_process_index() {
    let temporary = tempdir().expect("temp dir");
    let mut stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    stage
        .insert(
            TeslaMateStageTable::Charges,
            11,
            &ChargeRow {
                charging_process_id: 7,
                label: "wanted".to_owned(),
            },
        )
        .expect("wanted charge");
    stage
        .insert(
            TeslaMateStageTable::Charges,
            12,
            &ChargeRow {
                charging_process_id: 8,
                label: "other".to_owned(),
            },
        )
        .expect("other charge");
    stage.seal().expect("sealed");

    let sql = format!("EXPLAIN QUERY PLAN {CHARGE_SAMPLES_PAGE_SQL}");
    let mut statement = stage.connection.prepare(&sql).expect("plan statement");
    let plan = statement
        .query_map(rusqlite::params![7_i64, 0_i64, 2_i64], |row| {
            row.get::<_, String>(3)
        })
        .expect("plan query")
        .collect::<Result<Vec<_>, _>>()
        .expect("plan rows");
    assert!(
        plan.iter()
            .any(|detail| detail.contains("USING INDEX stage_charge_samples_by_process")),
        "unexpected charge-sample plan: {plan:?}"
    );
}

#[test]
fn rejects_bound_overrun_duplicate_ids_and_non_object_rows() {
    let temporary = tempdir().expect("temp dir");
    let mut stage = TeslaMateStage::create(
        private_imports(&temporary),
        TeslaMateStageLimits {
            max_rows: 1,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("stage");
    let first = Row {
        label: "one".to_owned(),
        ordinal: 1,
    };
    stage
        .insert(TeslaMateStageTable::Cars, 1, &first)
        .expect("one row");
    assert!(matches!(
        stage.insert(TeslaMateStageTable::Cars, 2, &first),
        Err(TeslaMateStageError::RowLimitExceeded { maximum: 1 })
    ));

    let temporary = tempdir().expect("temp dir");
    let mut byte_limited = TeslaMateStage::create(
        private_imports(&temporary),
        TeslaMateStageLimits {
            max_rows: 2,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("stage");
    let too_large = Row {
        label: "x".repeat(128 * 1024),
        ordinal: 1,
    };
    assert!(matches!(
        byte_limited.insert(TeslaMateStageTable::Cars, 1, &too_large),
        Err(TeslaMateStageError::PayloadByteLimitExceeded { .. })
    ));
    assert!(matches!(
        byte_limited.insert(TeslaMateStageTable::Cars, 0, &first),
        Err(TeslaMateStageError::InvalidSourceId)
    ));
    assert!(matches!(
        byte_limited.insert(TeslaMateStageTable::Cars, 1, &"scalar"),
        Err(TeslaMateStageError::RowMustBeJsonObject)
    ));
    byte_limited
        .insert(TeslaMateStageTable::Cars, 1, &first)
        .expect("row");
    assert!(matches!(
        byte_limited.insert(TeslaMateStageTable::Cars, 1, &first),
        Err(TeslaMateStageError::DuplicateSourceId { .. })
    ));
}

#[test]
fn refuses_open_or_symlinked_stages_as_complete_snapshots() {
    let temporary = tempdir().expect("temp dir");
    let stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    let path = stage.path().to_path_buf();
    drop(stage);
    assert!(matches!(
        TeslaMateStage::open_sealed(&path),
        Err(TeslaMateStageError::StageNotSealed)
    ));

    let target = temporary.path().join("target.sqlite");
    fs::write(&target, b"not a stage").expect("target");
    let link = path.parent().expect("stage parent").join("link.sqlite");
    std::os::unix::fs::symlink(&target, &link).expect("link");
    assert!(matches!(
        TeslaMateStage::open_sealed(link),
        Err(TeslaMateStageError::SymlinkPath(_))
    ));
}

#[test]
fn rejects_insecure_existing_directories_without_changing_their_modes() {
    for mode in [0o755, 0o770] {
        let temporary = tempdir().expect("temp dir");
        let imports = temporary.path().join("imports");
        fs::create_dir(&imports).expect("imports");
        fs::set_permissions(&imports, fs::Permissions::from_mode(mode)).expect("insecure mode");

        assert!(matches!(
            TeslaMateStage::create(&imports, limits()),
            Err(TeslaMateStageError::InsecurePermissions {
                expected: 0o700,
                actual,
                ..
            }) if actual == mode
        ));
        assert_eq!(
            fs::metadata(&imports)
                .expect("imports metadata")
                .permissions()
                .mode()
                & 0o777,
            mode
        );
    }

    let temporary = tempdir().expect("temp dir");
    let imports = temporary.path().join("imports");
    fs::create_dir(&imports).expect("imports");
    fs::set_permissions(&imports, fs::Permissions::from_mode(0o700)).expect("private imports");
    let staging = imports.join(STAGING_DIRECTORY);
    fs::create_dir(&staging).expect("staging");
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o770))
        .expect("insecure staging mode");
    assert!(matches!(
        TeslaMateStage::create(&imports, limits()),
        Err(TeslaMateStageError::InsecurePermissions {
            expected: 0o700,
            actual: 0o770,
            ..
        })
    ));
    assert_eq!(
        fs::metadata(staging)
            .expect("staging metadata")
            .permissions()
            .mode()
            & 0o777,
        0o770
    );
}

#[test]
fn rejects_insecure_existing_stage_files_without_changing_their_modes() {
    for mode in [0o644, 0o622] {
        let temporary = tempdir().expect("temp dir");
        let mut stage =
            TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
        stage.seal().expect("sealed");
        let path = stage.path().to_path_buf();
        drop(stage);
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("insecure stage mode");

        assert!(matches!(
            TeslaMateStage::open_sealed(&path),
            Err(TeslaMateStageError::InsecurePermissions {
                expected: 0o600,
                actual,
                ..
            }) if actual == mode
        ));
        assert_eq!(
            fs::metadata(path)
                .expect("stage metadata")
                .permissions()
                .mode()
                & 0o777,
            mode
        );
    }
}

#[test]
fn rejects_hard_linked_stage_files() {
    let temporary = tempdir().expect("temp dir");
    let mut stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    stage.seal().expect("sealed");
    let path = stage.path().to_path_buf();
    drop(stage);
    let second_link = path.with_extension("retained-link");
    fs::hard_link(&path, second_link).expect("second hard link");

    assert!(matches!(
        TeslaMateStage::open_sealed(path),
        Err(TeslaMateStageError::UnexpectedLinkCount { actual: 2, .. })
    ));
}

#[test]
fn rejects_stage_replacement_between_secure_and_sqlite_open() {
    let temporary = tempdir().expect("temp dir");
    let mut stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    stage.seal().expect("sealed");
    let path = stage.path().to_path_buf();
    let original = path.with_extension("original");
    drop(stage);

    let error = TeslaMateStage::open_sealed_with_hook(&path, || {
        fs::rename(&path, &original).expect("retain original inode");
        fs::copy(&original, &path).expect("replacement copy");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private replacement");
    })
    .expect_err("identity mismatch");
    assert!(matches!(
        error,
        TeslaMateStageError::StagePathIdentityChanged(ref changed) if changed == &path
    ));
}

#[test]
fn sealed_sqlite_connection_reads_the_admitted_descriptor_after_path_replacement() {
    let temporary = tempdir().expect("temp dir");
    let mut stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    stage.seal().expect("sealed");
    let path = stage.path().to_path_buf();
    drop(stage);

    let stage_path = ensure_private_stage_path(&path).expect("secure stage path");
    let (descriptor, _) = open_private_stage_file(&stage_path, false).expect("stage fd");
    let original = path.with_extension("descriptor-original");
    fs::rename(&path, &original).expect("retain admitted inode");
    fs::write(&path, b"not a SQLite database").expect("replacement");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private replacement");

    let connection = open_read_only_sqlite_from_descriptor(&descriptor).expect("fd SQLite");
    let state: String = connection
        .query_row(
            "SELECT value FROM stage_meta WHERE key = 'state'",
            [],
            |row| row.get(0),
        )
        .expect("query admitted stage inode");
    assert_eq!(state, TeslaMateStageState::Sealed.as_str());
}

#[test]
fn rejects_symlink_in_an_intermediate_stage_directory_component() {
    let temporary = tempdir().expect("temp dir");
    let real_root = temporary.path().join("real-root");
    fs::create_dir(&real_root).expect("real root");
    let mut stage = TeslaMateStage::create(real_root.join("imports"), limits()).expect("stage");
    stage.seal().expect("sealed");
    let path = stage.path().to_path_buf();
    let file_name = path.file_name().expect("file name").to_os_string();
    drop(stage);

    let alias = temporary.path().join("root-alias");
    std::os::unix::fs::symlink(&real_root, &alias).expect("intermediate symlink");
    let aliased_path = alias
        .join("imports")
        .join(STAGING_DIRECTORY)
        .join(file_name);
    assert!(matches!(
        TeslaMateStage::open_sealed(aliased_path),
        Err(TeslaMateStageError::SecureOpen { .. })
    ));
}

#[test]
fn rejects_stage_directory_replacement_between_secure_and_sqlite_open() {
    let temporary = tempdir().expect("temp dir");
    let mut stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    stage.seal().expect("sealed");
    let path = stage.path().to_path_buf();
    let staging = path.parent().expect("staging directory").to_path_buf();
    let displaced = staging.with_extension("original-directory");
    let file_name = path.file_name().expect("stage file").to_os_string();
    drop(stage);

    let error = TeslaMateStage::open_sealed_with_hook(&path, || {
        fs::rename(&staging, &displaced).expect("retain original directory");
        fs::create_dir(&staging).expect("replacement directory");
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700))
            .expect("private replacement directory");
        let replacement = staging.join(&file_name);
        fs::copy(displaced.join(&file_name), &replacement).expect("replacement stage");
        fs::set_permissions(replacement, fs::Permissions::from_mode(0o600))
            .expect("private replacement stage");
    })
    .expect_err("directory identity mismatch");
    assert!(matches!(
        error,
        TeslaMateStageError::DirectoryIdentityChanged(ref changed) if changed == &staging
    ));
}

#[test]
fn discards_only_its_exact_private_stage_file() {
    let temporary = tempdir().expect("temp dir");
    let stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    let path = stage.path().to_path_buf();
    let staging = path.parent().expect("staging directory").to_path_buf();
    stage.discard().expect("discard");
    assert!(
        matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(staging.is_dir());
}

#[test]
fn discard_rechecks_identity_and_preserves_a_racing_replacement() {
    let temporary = tempdir().expect("temp dir");
    let stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    let path = stage.path().to_path_buf();
    let original = path.with_extension("retained-original");

    let error = stage
        .discard_with_hook(|| {
            fs::rename(&path, &original).expect("retain original stage");
            fs::write(&path, b"replacement must survive").expect("replacement");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .expect("private replacement");
        })
        .expect_err("racing replacement rejected");
    assert!(matches!(
        error,
        TeslaMateStageError::StagePathIdentityChanged(ref changed) if changed == &path
    ));
    assert_eq!(
        fs::read(&path).expect("replacement remains"),
        b"replacement must survive"
    );
    assert!(original.is_file(), "original stage was not deleted by path");
}

#[test]
fn discard_rejects_parent_replacement_and_preserves_both_directories() {
    let temporary = tempdir().expect("temp dir");
    let stage = TeslaMateStage::create(private_imports(&temporary), limits()).expect("stage");
    let path = stage.path().to_path_buf();
    let staging = path.parent().expect("staging directory").to_path_buf();
    let displaced = staging.with_extension("discard-original-directory");
    let file_name = path.file_name().expect("stage file").to_os_string();

    let error = stage
        .discard_with_hook(|| {
            fs::rename(&staging, &displaced).expect("retain original directory");
            fs::create_dir(&staging).expect("replacement staging directory");
            fs::set_permissions(&staging, fs::Permissions::from_mode(0o700))
                .expect("private replacement directory");
            let replacement = staging.join(&file_name);
            fs::write(&replacement, b"replacement must survive").expect("replacement file");
            fs::set_permissions(replacement, fs::Permissions::from_mode(0o600))
                .expect("private replacement");
        })
        .expect_err("racing directory replacement rejected");
    assert!(matches!(
        error,
        TeslaMateStageError::DirectoryIdentityChanged(ref changed) if changed == &staging
    ));
    assert_eq!(
        fs::read(staging.join(&file_name)).expect("replacement remains"),
        b"replacement must survive"
    );
    assert!(
        displaced.join(file_name).is_file(),
        "original stage remains in its retained directory"
    );
}

#[test]
fn refuses_to_consume_the_host_disk_reserve() {
    let temporary = tempdir().expect("temp dir");
    let result = TeslaMateStage::create(
        private_imports(&temporary),
        TeslaMateStageLimits {
            max_rows: 1,
            max_stage_bytes: MIN_STAGE_BYTES,
            minimum_free_bytes: u64::MAX - MIN_STAGE_BYTES,
        },
    );
    assert!(matches!(
        result,
        Err(TeslaMateStageError::InsufficientFreeSpace { .. })
    ));
}
