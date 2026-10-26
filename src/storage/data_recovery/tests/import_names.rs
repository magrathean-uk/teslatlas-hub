// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::db::import_names_test_support as names_fixture;

fn preceding_names_backup(
    temporary: &Path,
    collision: Option<bool>,
) -> (PathBuf, names_fixture::Rows) {
    // Use an ordinary v4 envelope, then replace its catalogue with one built
    // by the real preceding-version hook and reseal the declared member.
    let live = temporary.join("envelope-source");
    let store = HubStore::initialize(&live).unwrap();
    let installation = store.installation_id().unwrap();
    let backup = temporary.join("preceding-backup");
    create_data_backup(&store, &backup).unwrap();
    let catalogue = backup.join(CATALOGUE_MEMBER);
    fs::remove_file(&catalogue).unwrap();
    let connection = Connection::open(&catalogue).unwrap();
    names_fixture::preceding(&connection);
    connection
        .execute(
            "INSERT INTO hub_metadata VALUES ('installation_id',?1)",
            [installation.to_string()],
        )
        .unwrap();
    // A preceding release has already completed its one-time identity tidy.
    connection
        .execute(
            "INSERT INTO store_data_steps VALUES ('vehicle_identity_tidy_v1',1000,'{}')",
            [],
        )
        .unwrap();
    if let Some(only) = collision {
        if only {
            connection
                .execute(
                    "UPDATE source_identities SET source_key='default' WHERE source_id=?1",
                    [names_fixture::SOURCE],
                )
                .unwrap();
        } else {
            connection.execute("UPDATE source_identities SET source_key='default' WHERE source_key='retained-custom-key'",[]).unwrap();
        }
    }
    let expected = names_fixture::expected(names_fixture::snapshot(&connection));
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .unwrap();
    drop(connection);
    set_mode(
        &catalogue,
        PRIVATE_FILE_MODE,
        "synthetic preceding catalogue",
    )
    .unwrap();
    reseal_backup_manifest(&backup, 77);
    verify_data_backup(&backup).unwrap();
    (backup, expected)
}

#[test]
fn mt10_pre_step_backup_restores_with_new_names() {
    let temporary = crate::private_tempdir().unwrap();
    let (backup, expected) = preceding_names_backup(temporary.path(), None);
    let before = tree_snapshot(&backup);
    let destination = temporary.path().join("restored");
    restore_data_backup(&backup, &destination).unwrap();
    assert_eq!(tree_snapshot(&backup), before);
    let connection = open_immutable_catalogue(&destination).unwrap();
    names_fixture::assert_expected(&connection, &expected);
}

#[test]
fn mt10b_refused_shape_backup_leaves_no_partial_store() {
    for only in [false, true] {
        let temporary = crate::private_tempdir().unwrap();
        let (backup, _) = preceding_names_backup(temporary.path(), Some(only));
        let before = tree_snapshot(&backup);
        let destination = temporary.path().join("refused-restore");
        assert!(restore_data_backup(&backup, &destination).is_err());
        assert!(!destination.exists());
        assert_eq!(tree_snapshot(&backup), before);
        let unexpected = fs::read_dir(temporary.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("staging"))
            .collect::<Vec<_>>();
        assert!(
            unexpected.is_empty(),
            "no unpublished restore stage remains: {unexpected:?}"
        );
    }
}
