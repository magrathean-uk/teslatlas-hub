// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::test_assertions::LabelledResult;

// G2-4 M1-T6
#[test]
fn m1_t6_restore_refuses_below_floor_without_writes() {
    let test = "m1_t6_restore_refuses_below_floor_without_writes";
    let (temporary, store) = create_fixture();
    let backup = temporary.path().join("below-floor-bundle");
    create_data_backup(&store, &backup).unwrap();
    let catalogue = backup.join(CATALOGUE_MEMBER);
    fs::write(
        &catalogue,
        zstd::stream::decode_all(
            include_bytes!("../../../../fixtures/g2-4/m1/store-f-minus-one.sqlite.zst").as_slice(),
        )
        .unwrap(),
    )
    .unwrap();
    let connection = Connection::open(&catalogue).unwrap();
    connection
        .execute(
            "INSERT INTO hub_metadata VALUES ('installation_id',?1)",
            [store.installation_id().unwrap().to_string()],
        )
        .unwrap();
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .unwrap();
    drop(connection);
    reseal_backup_manifest(&backup, crate::db::MIN_STORE_VERSION - 1);
    let before = tree_snapshot(&backup);
    let destination = temporary.path().join("refused-restoration");
    let error = restore_data_backup(&backup, &destination)
        .labelled_err(test, "a bundle below the store floor is refused");
    assert!(
        matches!(&error,DataRecoveryError::InvalidBackup(message) if message.contains("outside the supported restore range")),
        "assertion failed: {test}: the restore floor refuses before staged migration"
    );
    let message = error.to_string();
    assert!(
        message.contains(&(crate::db::MIN_STORE_VERSION - 1).to_string())
            && message.contains(&crate::db::MIN_STORE_VERSION.to_string())
            && message.contains("re-import from the source"),
        "assertion failed: {test}: restore names both versions and the re-import path; {message}"
    );
    assert!(
        !destination.exists() && tree_snapshot(&backup) == before,
        "assertion failed: {test}: refusal preserves the bundle and creates no destination"
    );
}
