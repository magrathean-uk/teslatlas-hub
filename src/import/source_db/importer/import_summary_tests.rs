// SPDX-License-Identifier: AGPL-3.0-only

// The import's final report: the checked source range and the per-route
// counts (M08-T41, DEC-246).

use super::*;
use crate::import::source_db::seam::{SeamLink, SeamPackPlan, SeamPlan};

fn drive(id: i64, start: i64, end: i64) -> ProjectionDrive {
    serde_json::from_value(serde_json::json!({
        "id": id, "car_id": 1, "start_date_ms": start, "end_date_ms": end,
        "ascent": 1, "descent": 1,
    }))
    .unwrap()
}

fn charge(id: i64, start: i64, end: Option<i64>) -> ProjectionCharge {
    serde_json::from_value(serde_json::json!({
        "id": id, "car_id": 1, "start_date_ms": start, "end_date_ms": end,
    }))
    .unwrap()
}

fn plan() -> SeamPackPlan {
    let mut seam = SeamPlan::default();
    seam.drives.imported = vec![1, 2];
    seam.drives.hub_kept = vec![9];
    seam.drives.skipped = vec![SeamLink { loser_id: 3, winner_id: 9 }];
    seam.charges.imported = vec![5];
    seam.charges.skipped = vec![SeamLink { loser_id: 6, winner_id: 50 }];
    SeamPackPlan::for_plan(&Default::default(), &seam)
}

#[test]
fn import_summary_range_spans_drives_and_charges() {
    let basis = SourceDbImportBasis::of(
        &[drive(1, 2_000, 3_000), drive(2, 500, 800)],
        &[charge(1, 1_000, Some(9_000)), charge(2, 4_000, None)],
    );
    assert_eq!(basis.range_ms, Some((500, 9_000)));
    assert_eq!(basis.sessions, 4);
}

#[test]
fn import_summary_range_is_none_for_an_empty_source() {
    let basis = SourceDbImportBasis::of(&[], &[]);
    assert_eq!(basis, SourceDbImportBasis::default());
    let json = SourceDbImportSummary::new(basis, None, true).to_json();
    assert!(json["checkedFromMs"].is_null() && json["checkedToMs"].is_null());
}

#[test]
fn import_summary_counts_packed_kept_and_withheld_from_the_seam() {
    let basis = SourceDbImportBasis::of(&[drive(1, 1, 2)], &[]);
    let summary = SourceDbImportSummary::new(basis, Some(&plan()), true);
    let expected = SourceDbRouteCounts { packed: 3, kept: 1, withheld: 2 };
    assert_eq!(summary.legacy, expected);
    assert_eq!(summary.physical, expected);
}

#[test]
fn import_summary_without_a_seam_packs_every_session() {
    let basis = SourceDbImportBasis::of(&[drive(1, 1, 2)], &[charge(1, 3, Some(4))]);
    let summary = SourceDbImportSummary::new(basis, None, true);
    assert_eq!(summary.legacy, SourceDbRouteCounts { packed: 2, kept: 0, withheld: 0 });
}

#[test]
fn import_summary_withholds_a_physical_route_that_is_not_served() {
    let basis = SourceDbImportBasis::of(&[drive(1, 1, 2)], &[]);
    let summary = SourceDbImportSummary::new(basis, Some(&plan()), false);
    assert_eq!(summary.legacy.packed, 3);
    assert_eq!(summary.physical, SourceDbRouteCounts { packed: 0, kept: 0, withheld: 5 });
}

#[test]
fn import_summary_json_holds_only_a_range_and_counts() {
    let basis = SourceDbImportBasis::of(&[drive(1, 10, 20)], &[]);
    let json = SourceDbImportSummary::new(basis, Some(&plan()), true).to_json();
    assert_eq!(json["checkedFromMs"], 10);
    assert_eq!(json["checkedToMs"], 20);
    assert_eq!(json["routes"]["legacy"]["packed"], 3);
    assert_eq!(json["routes"]["legacy"]["kept"], 1);
    assert_eq!(json["routes"]["physical"]["withheld"], 2);
    let keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["checkedFromMs", "checkedToMs", "routes"]);
    assert_eq!(json["routes"].as_object().unwrap().len(), 2);
}
