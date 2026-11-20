// SPDX-License-Identifier: AGPL-3.0-only

use std::convert::Infallible;

use serde_json::json;

use super::*;

const T0: i64 = 1_700_000_000_000;
const DEST_END: i64 = T0 + 3_600_000;
const NOW: i64 = DEST_END + 72 * 3_600_000;
const ONLINE: GapEvidence = GapEvidence {
    charge_between: false,
    state: StateCoverage::Online,
};

fn sample(id: i64, at: i64, odometer: f64) -> RepairSample {
    RepairSample {
        position_id: id,
        date_ms: at,
        odometer_km: Some(odometer),
        speed_kmh: Some(30),
    }
}

fn fragment(id: i64, at: i64, odometer: f64) -> RepairInput {
    let s = sample(id * 100, at, odometer);
    RepairInput {
        drive_id: id,
        car_id: 1,
        start_ms: at,
        end_ms: None,
        distance_km: None,
        duration_min: None,
        sample_count: 1,
        first_sample: Some(s),
        last_sample: Some(s),
        previous_drive_id: Some(id - 1),
        previous_unclosed: false,
        next_drive_id: Some(id + 1),
        gap_to_next: Some(ONLINE),
    }
}

fn destination(id: i64, at: i64, odometer: f64) -> RepairInput {
    RepairInput {
        drive_id: id,
        car_id: 1,
        start_ms: at,
        end_ms: Some(DEST_END),
        distance_km: Some(12.5),
        duration_min: Some(57),
        sample_count: 40,
        first_sample: Some(sample(id * 100, at, odometer)),
        last_sample: Some(sample(id * 100 + 39, DEST_END, odometer + 12.5)),
        previous_drive_id: Some(id - 1),
        previous_unclosed: true,
        next_drive_id: Some(id + 1),
        gap_to_next: None,
    }
}

/// Three structural fragments one minute apart, then their destination.
fn three_fragment_run() -> Vec<RepairInput> {
    let mut rows = vec![
        fragment(11, T0, 100.0),
        fragment(12, T0 + 60_000, 100.5),
        fragment(13, T0 + 120_000, 101.0),
        destination(14, T0 + 180_000, 101.5),
    ];
    rows[1].previous_unclosed = true;
    rows[2].previous_unclosed = true;
    rows
}

struct Answers {
    refuse: Option<(i64, RepairRefusal)>,
    suppressed: bool,
}

const ADMIT: Answers = Answers {
    refuse: None,
    suppressed: false,
};

impl RepairEligibility for Answers {
    type Error = Infallible;

    fn member_refusal(
        &mut self,
        member: &RepairInput,
        _role: MemberRole,
    ) -> Result<Option<RepairRefusal>, Infallible> {
        Ok(self
            .refuse
            .filter(|(id, _)| *id == member.drive_id)
            .map(|(_, reason)| reason))
    }

    fn suppressed(&mut self, _component_key: &[u8; 32]) -> Result<bool, Infallible> {
        Ok(self.suppressed)
    }
}

fn plan(rows: Vec<RepairInput>, mut answers: Answers, now_ms: i64) -> (PlanCounts, Vec<PlanOutcome>) {
    let mut planner = ComponentPlanner::new("vehicle", now_ms);
    let mut outcomes = Vec::new();
    for row in rows {
        outcomes.extend(planner.push(row, &mut answers).unwrap());
    }
    let (counts, tail) = planner.finish();
    outcomes.extend(tail);
    assert_eq!(
        counts.candidates,
        counts.planned_fragments + counts.refused_total(),
        "every candidate is counted exactly once"
    );
    (counts, outcomes)
}

fn planned(outcomes: &[PlanOutcome]) -> Vec<&PlannedComponent> {
    outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            PlanOutcome::Planned(component) => Some(component),
            PlanOutcome::Refused { .. } => None,
        })
        .collect()
}

#[test]
fn broken_drive_repair_plan_three_fragments_yield_one_plan() {
    let (counts, outcomes) = plan(three_fragment_run(), ADMIT, NOW);
    let components = planned(&outcomes);
    assert_eq!(components.len(), 1);
    let component = components[0];
    assert_eq!(component.destination_drive_id, 14);
    assert_eq!(component.fragment_drive_ids, vec![11, 12, 13]);
    assert_eq!(component.sample_position_ids, vec![1100, 1200, 1300]);
    assert_eq!(component.effective_start_ms, T0);
    assert_eq!(component.changed_rows, 7);
    assert_eq!(counts.candidates, 3);
    assert_eq!(counts.planned_components, 1);
    assert_eq!(counts.planned_fragments, 3);
    assert!(counts.refused.is_empty());
    let (_, again) = plan(three_fragment_run(), ADMIT, NOW + 1);
    assert_eq!(planned(&again)[0].component_key, component.component_key, "identity is deterministic");
}

fn refused_with(
    mutate: impl FnOnce(&mut Vec<RepairInput>),
    answers: Answers,
    now_ms: i64,
    expected: RepairRefusal,
) {
    let mut rows = three_fragment_run();
    mutate(&mut rows);
    let (counts, outcomes) = plan(rows, answers, now_ms);
    assert!(planned(&outcomes).is_empty(), "{expected:?}: no plan");
    assert!(
        counts.refused.contains_key(expected.code()),
        "{expected:?} expected in {:?}",
        counts.refused
    );
}

#[test]
fn broken_drive_repair_plan_refuses_every_unsafe_shape() {
    use RepairRefusal::*;
    let cases: Vec<(Box<dyn FnOnce(&mut Vec<RepairInput>)>, RepairRefusal)> = vec![
        (Box::new(|rows| {
            rows.truncate(3);
            rows[2].next_drive_id = None;
            rows[2].gap_to_next = None;
        }), Terminal),
        (Box::new(|rows| rows[1].duration_min = Some(0)), NotStructural),
        (Box::new(|rows| rows[1].sample_count = 2), NotStructural),
        (Box::new(|rows| rows[3].distance_km = Some(0.0)), DestinationZeroDistance),
        (Box::new(|rows| rows[3].end_ms = None), NotStructural),
        (Box::new(|rows| {
            let mut s = rows[0].first_sample.unwrap();
            s.odometer_km = None;
            rows[0].first_sample = Some(s);
            rows[0].last_sample = Some(s);
        }), MissingEvidence),
        (Box::new(|rows| rows[1].gap_to_next = None), MissingEvidence),
        (Box::new(|rows| {
            rows[3].start_ms += 200_000;
            let mut s = rows[3].first_sample.unwrap();
            s.date_ms += 200_000;
            rows[3].first_sample = Some(s);
        }), LongGap),
        (Box::new(|rows| {
            let mut s = rows[3].first_sample.unwrap();
            s.date_ms = rows[2].first_sample.unwrap().date_ms;
            rows[3].first_sample = Some(s);
            rows[3].start_ms = s.date_ms;
        }), AmbiguousOrder),
        (Box::new(|rows| rows[0].previous_unclosed = true), AmbiguousStart),
        (Box::new(|rows| rows[1].gap_to_next = Some(GapEvidence { charge_between: true, state: StateCoverage::Online })), Interrupted),
        (Box::new(|rows| rows[1].gap_to_next = Some(GapEvidence { charge_between: false, state: StateCoverage::Interrupted })), Interrupted),
        (Box::new(|rows| rows[1].gap_to_next = Some(GapEvidence { charge_between: false, state: StateCoverage::Unknown })), UnknownState),
        (Box::new(|rows| {
            let mut s = rows[2].first_sample.unwrap();
            s.odometer_km = Some(99.0);
            rows[2].first_sample = Some(s);
            rows[2].last_sample = Some(s);
        }), OdometerReset),
        (Box::new(|rows| {
            let mut s = rows[3].first_sample.unwrap();
            s.odometer_km = Some(140.0);
            rows[3].first_sample = Some(s);
        }), ImplausibleSpeed),
        (Box::new(|rows| {
            let mut s = rows[1].first_sample.unwrap();
            s.speed_kmh = Some(0);
            rows[1].first_sample = Some(s);
            rows[1].last_sample = Some(s);
        }), StationaryBoundary),
    ];
    for (mutate, expected) in cases {
        refused_with(mutate, ADMIT, NOW, expected);
    }
    refused_with(|_| {}, ADMIT, DEST_END + 3_600_000, DestinationNotStable);
    for (member, reason) in [
        (11, NotImportOwned),
        (14, NotImportOwned),
        (12, CollectionCoverage),
        (14, Protected),
        (13, UnknownEditHistory),
    ] {
        refused_with(
            |_| {},
            Answers {
                refuse: Some((member, reason)),
                suppressed: false,
            },
            NOW,
            reason,
        );
    }
    refused_with(
        |_| {},
        Answers {
            refuse: None,
            suppressed: true,
        },
        NOW,
        Suppressed,
    );
}

#[test]
fn broken_drive_repair_plan_ignores_completed_rows_and_bounds_components() {
    // A zero-distance completed drive is never a candidate.
    let mut lone = destination(14, T0, 100.0);
    lone.distance_km = Some(0.0);
    let (counts, outcomes) = plan(vec![lone], ADMIT, NOW);
    assert_eq!(counts.candidates, 0);
    assert!(outcomes.is_empty());

    // A run beyond the changed-row bound is skipped whole.
    let mut rows = (0..60)
        .map(|i| {
            let mut row = fragment(1_000 + i, T0 + i * 10_000, 100.0 + i as f64 * 0.05);
            row.previous_unclosed = i > 0;
            row
        })
        .collect::<Vec<_>>();
    rows.push(destination(1_060, T0 + 600_000, 103.1));
    let (counts, outcomes) = plan(rows, ADMIT, NOW);
    assert!(planned(&outcomes).is_empty());
    assert_eq!(counts.refused.get("oversized"), Some(&60));
}

fn drive(id: i64, start: i64, end: Option<i64>, distance: Option<f64>) -> SourceDbDrive {
    serde_json::from_value(json!({
        "id": id, "car_id": 1, "start_date_ms": start, "end_date_ms": end,
        "distance_km": distance, "duration_min": end.map(|_| 1),
    }))
    .unwrap()
}

fn position(id: i64, drive_id: Option<i64>, at: i64, odometer: f64) -> SourceDbPosition {
    serde_json::from_value(json!({
        "id": id, "car_id": 1, "drive_id": drive_id, "date_ms": at,
        "latitude": 0.0, "longitude": 0.0, "speed": 30, "odometer": odometer,
    }))
    .unwrap()
}

#[test]
fn broken_drive_repair_plan_capture_indexes_only_candidates_with_gap_evidence() {
    let drives = vec![
        drive(10, T0 - 600_000, Some(T0 - 300_000), Some(1.0)),
        drive(11, T0, None, None),
        drive(12, T0 + 60_000, None, None),
        drive(13, T0 + 120_000, Some(DEST_END), Some(12.0)),
        drive(14, DEST_END + 600_000, Some(DEST_END + 900_000), Some(3.0)),
        drive(15, DEST_END + 1_200_000, None, None),
    ];
    let mut capture = BrokenDriveCandidateCapture::new(&drives, 1);
    capture.observe_state(
        &serde_json::from_value(json!({
            "id": 1, "car_id": 1, "state": "online", "start_date_ms": T0 - 1_000_000,
            "end_date_ms": DEST_END + 1_000_000,
        }))
        .unwrap(),
    );
    capture.observe_charging_process(
        &serde_json::from_value(json!({
            "id": 1, "car_id": 1, "start_date_ms": DEST_END + 2_000_000, "end_date_ms": null,
        }))
        .unwrap(),
    );
    for p in [
        position(1, Some(11), T0, 100.0),
        position(2, Some(12), T0 + 60_000, 100.5),
        position(4, Some(13), T0 + 125_000, 101.2),
        position(3, Some(13), T0 + 120_000, 101.0),
        position(5, Some(15), DEST_END + 1_200_000, 120.0),
        position(6, None, T0, 100.0),
    ] {
        capture.observe_position(&p);
    }
    let index = capture.finish();
    let ids = index.rows.iter().map(|row| row.drive_id).collect::<Vec<_>>();
    assert_eq!(ids, vec![11, 12, 13, 15], "unclosed rows and their successors only");
    assert!(index.rows[0].is_structural_fragment());
    assert_eq!(index.rows[0].gap_to_next, Some(ONLINE));
    assert!(index.rows[1].previous_unclosed);
    assert_eq!(index.rows[2].first_sample.unwrap().position_id, 3, "earliest sample");
    assert_eq!(index.rows[2].sample_count, 2);
    assert_eq!(index.rows[3].next_drive_id, None, "terminal");
    assert_ne!(index.capture_digest(), BrokenDriveCandidateIndex::default().capture_digest());

    let (counts, outcomes) = plan(index.rows.clone(), ADMIT, NOW);
    assert_eq!(planned(&outcomes).len(), 1);
    assert_eq!(counts.refused.get("terminal"), Some(&1));
}
