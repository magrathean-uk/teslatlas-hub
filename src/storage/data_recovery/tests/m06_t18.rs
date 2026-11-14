//! M06-T18 (DEC-139): macOS Finder metadata at the checked roots is ignored,
//! never restored; any other unknown member is still refused.

use super::*;

fn drop_junk(directory: &Path) {
    fs::write(directory.join(".DS_Store"), b"finder").unwrap();
    fs::write(directory.join("._x"), b"appledouble").unwrap();
}

#[test]
fn data_recovery_m06_t18_finder_metadata_is_ignored_and_never_restored() {
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let backup = temporary.path().join("backup");
    let created = create_data_backup(&store, &backup).expect("backup");
    drop_junk(&backup);
    drop_junk(&backup.join("data"));
    drop_junk(&backup.join("data").join("packs"));

    let verified = verify_data_backup(&backup).expect("verify ignores Finder metadata");
    assert_eq!(verified.member_count, created.member_count);

    let restored_path = temporary.path().join("restored");
    let restored = restore_data_backup(&backup, &restored_path).expect("restore ignores it");
    assert_eq!(restored.member_count, created.member_count);
    let mut names: Vec<String> = fs::read_dir(&restored_path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["hub.sqlite", "packs", "secrets"], "no Finder metadata is restored");
    assert!(!restored_path.join("packs").join(".DS_Store").exists());
}

#[test]
fn data_recovery_m06_t18_other_unknown_members_are_still_refused() {
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let backup = temporary.path().join("backup");
    create_data_backup(&store, &backup).expect("backup");
    drop_junk(&backup.join("data"));
    fs::write(backup.join("data").join("extra.txt"), b"x").unwrap();
    let error = verify_data_backup(&backup).expect_err("extra.txt refused");
    assert!(error.to_string().contains("unknown or missing members"), "{error}");
}

#[test]
fn data_recovery_m06_t18_finder_names_that_are_not_regular_files_are_refused() {
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let backup = temporary.path().join("backup");
    create_data_backup(&store, &backup).expect("backup");
    let data = backup.join("data");

    let directory = data.join(".DS_Store");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("payload"), b"x").unwrap();
    assert!(verify_data_backup(&backup).is_err(), "directory .DS_Store refused");
    fs::remove_dir_all(&directory).unwrap();

    let link = data.join("._x");
    std::os::unix::fs::symlink("/etc/hosts", &link).unwrap();
    assert!(verify_data_backup(&backup).is_err(), "symlink ._x refused");
    fs::remove_file(&link).unwrap();
    verify_data_backup(&backup).expect("clean again");
}

#[test]
fn data_recovery_m06_t18_pack_and_noop_walkers_skip_regular_finder_files_only() {
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let backup = temporary.path().join("backup");
    let created = create_data_backup(&store, &backup).expect("backup");
    let packs = backup.join("data").join("packs");
    for sub in ["sha256", "noop"] {
        let dir = packs.join(sub);
        if dir.is_dir() {
            fs::write(dir.join(".DS_Store"), b"finder").unwrap();
            fs::write(dir.join("._x"), b"apple").unwrap();
        }
    }
    let verified = verify_data_backup(&backup).expect("verify skips regular Finder files");
    assert_eq!(verified.member_count, created.member_count);
    let restored_path = temporary.path().join("restored");
    restore_data_backup(&backup, &restored_path).expect("restore skips them");
    let restored_packs = restored_path.join("packs").join("sha256");
    assert!(!restored_packs.join(".DS_Store").exists());
    assert!(!restored_packs.join("._x").exists());

    let link = packs.join("sha256").join("._link");
    std::os::unix::fs::symlink("/etc/hosts", &link).unwrap();
    assert!(verify_data_backup(&backup).is_err(), "pack-directory symlink refused");
}
