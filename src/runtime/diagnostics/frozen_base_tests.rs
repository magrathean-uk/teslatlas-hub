// SPDX-License-Identifier: AGPL-3.0-only

// STORAGE-T3a: a damaged frozen physical base record is named by doctor, which
// still prints its integrity check and the rest of the report.

use super::{doctor_config, *};

#[test]
fn frozen_base_doctor_names_a_damaged_record_and_still_reports_integrity() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let config = doctor_config(temporary.path(), "interval_seconds = 0", "");
    let key = "physical_v3_frozen_base!00000000-0000-4000-8000-000000000001";
    store
        .open()
        .expect("catalogue")
        .execute("INSERT INTO hub_metadata(key, value) VALUES(?1, '{}')", [key])
        .expect("damaged record");

    let report = inspect_hub(&store, &config).expect("doctor still reports");
    let integrity = report
        .checks
        .iter()
        .find(|check| check.name == "catalogueIntegrity")
        .expect("the integrity check is printed");
    assert!(!integrity.passed);
    assert!(integrity.detail.contains(key), "{}", integrity.detail);
    assert!(integrity.detail.contains("restore"), "{}", integrity.detail);
    assert!(!report.is_ok());
    assert!(report.catalogue.schema_version > 0);
}
