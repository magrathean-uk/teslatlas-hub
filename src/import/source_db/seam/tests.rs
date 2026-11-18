// SPDX-License-Identifier: AGPL-3.0-only

//! M03-T14: the pure seam plan (design M03-SEAM-R3, section 4). Times are
//! written in seconds and held in milliseconds. Every scenario checks that each
//! row is kept once or stands behind a kept winner, that no instant counts
//! twice, and that the plan applied and planned again repeats itself.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::db::hub_ids::HUB_ID_BASE;

const WINDOW: (i64, i64) = (1_000, 2_000);
const SECOND_WINDOW: (i64, i64) = (5_000, 6_000);

fn ms(seconds: i64) -> i64 {
    seconds * 1_000
}

fn hub_id(n: i64) -> i64 {
    HUB_ID_BASE + n
}

fn coverage(windows: &[(i64, i64)]) -> HubCoverage {
    HubCoverage::from_intervals(windows.iter().map(|(a, b)| (ms(*a), ms(*b))))
}

fn session(id: i64, start: i64, end: i64) -> SessionRow {
    SessionRow {
        id,
        start_ms: ms(start),
        end_ms: Some(ms(end)),
    }
}

/// One child every `step` seconds over `[from, to]`, ids counting up from
/// `first_id`.
fn children(first_id: i64, from: i64, to: i64, step: i64, parent: Option<i64>) -> Vec<ChildRow> {
    (0..)
        .map(|n| from + n * step)
        .take_while(|at| *at <= to)
        .enumerate()
        .map(|(n, at)| ChildRow {
            id: first_id + n as i64,
            at_ms: ms(at),
            parent,
        })
        .collect()
}

/// The store side as the planner reads it, with the recorded links.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Store {
    hub: HubRows,
    links: SeamLinks,
}

impl Store {
    fn normalised(mut self) -> Self {
        for rows in [&mut self.hub.drives, &mut self.hub.charges] {
            rows.sort_unstable();
        }
        for rows in [&mut self.hub.positions, &mut self.hub.charge_samples] {
            rows.sort_unstable();
        }
        for ids in [
            &mut self.hub.imported_drives,
            &mut self.hub.imported_charges,
        ] {
            ids.sort_unstable();
            ids.dedup();
        }
        self
    }
}

/// What the store would hold after applying `plan` (the M03-T15 contract in
/// miniature): superseded and stale rows go, inherited rows take the kept
/// copy, kept rows widen, links follow the decisions, Hub children outside
/// coverage go and moved Hub children take their new parent.
fn apply(store: &Store, source: &SourceRows, plan: &SeamPlan) -> Store {
    fn sessions(
        rows: &[SessionRow],
        imported: &[i64],
        source: &[SessionRow],
        plan: &SessionPlan,
        links: &BTreeMap<i64, i64>,
    ) -> (Vec<SessionRow>, Vec<i64>, BTreeMap<i64, i64>) {
        let gone: BTreeSet<i64> = plan
            .superseded
            .iter()
            .map(|link| link.loser_id)
            .chain(plan.stale.iter().copied())
            .collect();
        let mut kept: Vec<SessionRow> = rows
            .iter()
            .filter(|row| !gone.contains(&row.id))
            .map(|row| {
                let mut row = *row;
                if plan.inherited.contains(&(row.id, InheritedCopy::Source)) {
                    row = *source.iter().find(|copy| copy.id == row.id).unwrap();
                }
                if let Some(bounds) = plan.widened.iter().find(|bounds| bounds.id == row.id) {
                    row.start_ms = bounds.start_ms;
                    row.end_ms = bounds.end_ms;
                }
                row
            })
            .collect();
        kept.sort_unstable();
        let mut held: Vec<i64> = plan
            .imported
            .iter()
            .copied()
            .chain(plan.hub_kept.iter().copied().filter(|id| !is_hub_id(*id)))
            .collect();
        held.extend(imported.iter().copied().filter(|id| {
            !gone.contains(id) && !plan.skipped.iter().any(|link| link.loser_id == *id)
        }));
        held.sort_unstable();
        held.dedup();
        let mut links = links.clone();
        for loser in &plan.links_dropped {
            links.remove(loser);
        }
        for link in plan.skipped.iter().chain(&plan.superseded) {
            links.insert(link.loser_id, link.winner_id);
        }
        (kept, held, links)
    }
    fn hub_children(rows: &[ChildRow], plan: &ChildPlan) -> Vec<ChildRow> {
        rows.iter()
            .filter(|row| !plan.hub_removed.contains(&row.id))
            .map(|row| {
                let mut row = *row;
                if let Some(moved) = plan.moved_hub.iter().find(|moved| moved.id == row.id) {
                    row.parent = Some(moved.to);
                }
                row
            })
            .collect()
    }
    let (drives, imported_drives, drive_links) = sessions(
        &store.hub.drives,
        &store.hub.imported_drives,
        &source.drives,
        &plan.drives,
        &store.links.drives,
    );
    let (charges, imported_charges, charge_links) = sessions(
        &store.hub.charges,
        &store.hub.imported_charges,
        &source.charges,
        &plan.charges,
        &store.links.charges,
    );
    Store {
        hub: HubRows {
            drives,
            charges,
            positions: hub_children(&store.hub.positions, &plan.positions),
            charge_samples: hub_children(&store.hub.charge_samples, &plan.charge_samples),
            imported_drives,
            imported_charges,
        },
        links: SeamLinks {
            drives: drive_links,
            charges: charge_links,
        },
    }
    .normalised()
}

fn plan(coverage: &HubCoverage, store: &Store, source: &SourceRows) -> SeamPlan {
    plan_seam(coverage, &store.hub, source, &store.links)
}

/// Each session is kept once or stands behind a kept winner, kept sessions of
/// the two sides never overlap beyond the slack, each child is decided by its
/// instant and each kept child's final parent holds it.
fn assert_whole(
    name: &str,
    coverage: &HubCoverage,
    store: &Store,
    source: &SourceRows,
    plan: &SeamPlan,
) {
    fn sessions(
        name: &str,
        hub: &[SessionRow],
        source: &[SessionRow],
        plan: &SessionPlan,
    ) -> BTreeMap<i64, (i64, Option<i64>)> {
        let kept: BTreeSet<i64> = plan
            .imported
            .iter()
            .chain(&plan.hub_kept)
            .copied()
            .collect();
        let losers: BTreeSet<i64> = plan
            .skipped
            .iter()
            .chain(&plan.superseded)
            .map(|link| link.loser_id)
            .collect();
        for link in plan.skipped.iter().chain(&plan.superseded) {
            assert!(
                kept.contains(&link.winner_id),
                "{name}: winner of {link:?} is kept"
            );
        }
        for row in hub {
            assert!(
                kept.contains(&row.id) || losers.contains(&row.id) || plan.stale.contains(&row.id),
                "{name}: Hub session {} is decided",
                row.id
            );
        }
        for row in source {
            assert!(
                kept.contains(&row.id) || losers.contains(&row.id),
                "{name}: source session {} is decided",
                row.id
            );
            assert!(
                !(plan.imported.contains(&row.id) && losers.contains(&row.id)),
                "{name}: source session {} is kept once",
                row.id
            );
        }
        let hub_kept: Vec<&SessionRow> = hub
            .iter()
            .filter(|row| plan.hub_kept.contains(&row.id))
            .collect();
        for theirs in source.iter().filter(|row| plan.imported.contains(&row.id)) {
            for ours in hub_kept.iter().filter(|row| row.id != theirs.id) {
                assert!(
                    overlap(theirs.extent(), ours.extent()) <= SEAM_SLACK_MS,
                    "{name}: kept sessions {} and {} count an instant twice",
                    theirs.id,
                    ours.id
                );
            }
        }
        let mut bounds: BTreeMap<i64, (i64, Option<i64>)> = BTreeMap::new();
        for row in source.iter().filter(|row| plan.imported.contains(&row.id)) {
            bounds.insert(row.id, (row.start_ms, row.end_ms));
        }
        for row in hub.iter().filter(|row| plan.hub_kept.contains(&row.id)) {
            bounds.insert(row.id, (row.start_ms, row.end_ms));
        }
        for widened in &plan.widened {
            bounds.insert(widened.id, (widened.start_ms, widened.end_ms));
        }
        bounds
    }
    fn children(
        name: &str,
        coverage: &HubCoverage,
        hub: &[ChildRow],
        source: &[ChildRow],
        plan: &ChildPlan,
        bounds: &BTreeMap<i64, (i64, Option<i64>)>,
    ) {
        let check_parent = |row: &ChildRow, moves: &[ChildMove]| {
            let parent = moves
                .iter()
                .find(|moved| moved.id == row.id)
                .map(|moved| moved.to)
                .or(row.parent);
            if let Some((start, end)) = parent.and_then(|id| bounds.get(&id)) {
                assert!(
                    *start <= row.at_ms && end.is_none_or(|end| row.at_ms <= end),
                    "{name}: child {} lies inside its parent",
                    row.id
                );
            }
        };
        let mut imported = 0;
        for row in source {
            let inside = coverage.contains(row.at_ms);
            assert_eq!(
                plan.skipped.contains(&row.id),
                inside,
                "{name}: source child {}",
                row.id
            );
            if !inside {
                imported += 1;
                check_parent(row, &plan.moved_source);
            }
        }
        assert_eq!(plan.imported_count, imported, "{name}: imported children");
        let mut kept = 0;
        for row in hub {
            let inside = coverage.contains(row.at_ms);
            assert_eq!(
                plan.hub_removed.contains(&row.id),
                !inside,
                "{name}: Hub child {}",
                row.id
            );
            if inside {
                kept += 1;
                check_parent(row, &plan.moved_hub);
            }
        }
        assert_eq!(plan.hub_kept_count, kept, "{name}: kept Hub children");
    }
    let drives = sessions(name, &store.hub.drives, &source.drives, &plan.drives);
    let charges = sessions(name, &store.hub.charges, &source.charges, &plan.charges);
    children(
        name,
        coverage,
        &store.hub.positions,
        &source.positions,
        &plan.positions,
        &drives,
    );
    children(
        name,
        coverage,
        &store.hub.charge_samples,
        &source.charge_samples,
        &plan.charge_samples,
        &charges,
    );
}

/// The plan applied and planned again changes nothing more in the store and
/// keeps every decision.
fn assert_replays(name: &str, coverage: &HubCoverage, store: &Store, source: &SourceRows) -> Store {
    let first = plan(coverage, store, source);
    assert_whole(name, coverage, store, source, &first);
    let applied = apply(store, source, &first);
    let again = plan(coverage, &applied, source);
    assert_whole(name, coverage, &applied, source, &again);
    for (entity, before, after) in [
        ("drives", &first.drives, &again.drives),
        ("charges", &first.charges, &again.charges),
    ] {
        assert!(
            after.superseded.is_empty(),
            "{name}: {entity} replay supersedes nothing"
        );
        assert!(
            after.stale.is_empty(),
            "{name}: {entity} replay removes nothing"
        );
        assert!(
            after.links_dropped.is_empty(),
            "{name}: {entity} replay keeps the links"
        );
        assert!(
            after.widened.iter().all(|bounds| !is_hub_id(bounds.id)),
            "{name}: {entity} replay widens no stored Hub row"
        );
        assert_eq!(after.imported, before.imported, "{name}: {entity} imported");
        assert_eq!(after.skipped, before.skipped, "{name}: {entity} skipped");
    }
    for (entity, after) in [
        ("positions", &again.positions),
        ("charge samples", &again.charge_samples),
    ] {
        assert!(
            after.hub_removed.is_empty(),
            "{name}: {entity} replay removes nothing"
        );
        assert!(
            after.moved_hub.is_empty(),
            "{name}: {entity} replay moves no Hub child"
        );
    }
    assert_eq!(
        again.positions.skipped, first.positions.skipped,
        "{name}: positions"
    );
    assert_eq!(
        apply(&applied, source, &again),
        applied,
        "{name}: replay leaves the store as it is"
    );
    applied
}

fn store(hub: HubRows) -> Store {
    Store {
        hub,
        links: SeamLinks::default(),
    }
    .normalised()
}

fn link(loser_id: i64, winner_id: i64) -> SeamLink {
    SeamLink {
        loser_id,
        winner_id,
    }
}

#[test]
fn import_seam_plan_source_covering_hub_wins_across_an_edge() {
    // The source drove on into the window and its drive crosses the opening
    // edge; the Hub's drive lies inside it.
    let coverage = coverage(&[WINDOW]);
    let hub = store(HubRows {
        drives: vec![session(hub_id(1), 1_010, 1_300)],
        positions: children(hub_id(100), 1_010, 1_300, 10, Some(hub_id(1))),
        ..HubRows::default()
    });
    let source = SourceRows {
        drives: vec![session(10, 800, 1_310)],
        positions: children(1_000, 800, 1_310, 10, Some(10)),
        ..SourceRows::default()
    };
    let plan = plan(&coverage, &hub, &source);
    assert_eq!(plan.drives.imported, vec![10]);
    assert_eq!(plan.drives.superseded, vec![link(hub_id(1), 10)]);
    assert!(plan.drives.hub_kept.is_empty() && plan.drives.skipped.is_empty());
    // Children by instant: the Hub's inside the window, moved to the winner.
    assert_eq!(plan.positions.hub_kept_count, 30);
    assert_eq!(plan.positions.moved_hub.len(), 30);
    assert!(plan.positions.moved_hub.iter().all(|moved| moved.to == 10));
    assert_eq!(plan.positions.imported_count, 20);
    assert_eq!(plan.positions.skipped.len(), 32);
    assert!(plan.drives.widened.is_empty());
    assert_replays("across an edge", &coverage, &hub, &source);
}

#[test]
fn import_seam_plan_hub_wins_inside_coverage() {
    // Both extents cover each other within the slack and the source drive
    // lies wholly inside the window: the Hub's own row stands.
    let coverage = coverage(&[WINDOW]);
    let hub = store(HubRows {
        drives: vec![session(hub_id(1), 1_210, 1_490)],
        positions: children(hub_id(100), 1_210, 1_490, 10, Some(hub_id(1))),
        ..HubRows::default()
    });
    let source = SourceRows {
        drives: vec![session(11, 1_200, 1_500)],
        positions: children(1_000, 1_200, 1_500, 10, Some(11)),
        ..SourceRows::default()
    };
    let plan = plan(&coverage, &hub, &source);
    assert_eq!(plan.drives.hub_kept, vec![hub_id(1)]);
    assert_eq!(plan.drives.skipped, vec![link(11, hub_id(1))]);
    assert!(plan.drives.imported.is_empty() && plan.drives.superseded.is_empty());
    assert_eq!(plan.positions.imported_count, 0);
    assert_eq!(plan.positions.skipped.len(), 31);
    assert_eq!(plan.positions.hub_kept_count, 29);
    assert!(plan.positions.moved_hub.is_empty());
    // The same pair with the source crossing the edge goes to the source.
    let crossing = SourceRows {
        drives: vec![session(11, 970, 1_500)],
        ..SourceRows::default()
    };
    let hub_crossing = store(HubRows {
        drives: vec![session(hub_id(1), 1_000, 1_490)],
        ..HubRows::default()
    });
    let crossing_plan = super::plan_seam(
        &coverage,
        &hub_crossing.hub,
        &crossing,
        &SeamLinks::default(),
    );
    assert_eq!(crossing_plan.drives.imported, vec![11]);
    assert_replays("hub inside", &coverage, &hub, &source);
}

#[test]
fn short_source_drive_does_not_split_overlapping_drives() {
    let coverage = coverage(&[WINDOW]);
    let hub_drive = hub_id(1);
    let hub = store(HubRows {
        drives: vec![session(hub_drive, 1_210, 1_490)],
        ..HubRows::default()
    });
    let source = SourceRows {
        drives: vec![session(10, 1_220, 1_250), session(11, 1_260, 1_490)],
        ..SourceRows::default()
    };

    let planned = plan(&coverage, &hub, &source);
    assert_eq!(planned.drives.hub_kept, vec![hub_drive]);
    assert_eq!(planned.drives.imported, vec![10]);
    assert_eq!(planned.drives.skipped, vec![link(11, hub_drive)]);
    assert_replays("short drive between overlapping drives", &coverage, &hub, &source);
}

#[test]
fn import_seam_plan_neither_covers_longer_wins_children_move() {
    // The source saw the start, the Hub saw more of the drive: the Hub's row
    // is longer and wins; the source's positions outside the window stay and
    // move to it, widening its start only.
    let coverage = coverage(&[WINDOW]);
    let hub = store(HubRows {
        drives: vec![session(hub_id(1), 1_000, 1_600)],
        positions: children(hub_id(100), 1_000, 1_600, 20, Some(hub_id(1))),
        ..HubRows::default()
    });
    let source = SourceRows {
        drives: vec![session(12, 900, 1_200)],
        positions: children(2_000, 900, 1_200, 20, Some(12)),
        ..SourceRows::default()
    };
    let plan = plan(&coverage, &hub, &source);
    assert_eq!(plan.drives.hub_kept, vec![hub_id(1)]);
    assert_eq!(plan.drives.skipped, vec![link(12, hub_id(1))]);
    assert_eq!(plan.positions.imported_count, 5);
    assert_eq!(plan.positions.moved_source.len(), 5);
    assert!(
        plan.positions
            .moved_source
            .iter()
            .all(|moved| moved.from == 12 && moved.to == hub_id(1))
    );
    assert_eq!(
        plan.drives.widened,
        vec![Bounds {
            id: hub_id(1),
            start_ms: ms(900),
            end_ms: Some(ms(1_600)),
        }]
    );
    // The mirror case: the source is longer, so it wins and the Hub's
    // children inside the window move to it.
    let longer = SourceRows {
        drives: vec![session(12, 600, 1_200)],
        positions: children(2_000, 600, 1_200, 20, Some(12)),
        ..SourceRows::default()
    };
    let mirror = super::plan_seam(&coverage, &hub.hub, &longer, &SeamLinks::default());
    assert_eq!(mirror.drives.superseded, vec![link(hub_id(1), 12)]);
    assert_eq!(mirror.positions.moved_hub.len(), 31);
    assert_eq!(
        mirror.drives.widened,
        vec![Bounds {
            id: 12,
            start_ms: ms(600),
            end_ms: Some(ms(1_600)),
        }]
    );
    assert_replays("neither covers", &coverage, &hub, &source);
    assert_replays("neither covers, source longer", &coverage, &hub, &longer);
}

#[test]
fn import_seam_plan_abutting_handover_keeps_both() {
    // The source closed its drive at the window opening; the Hub started its
    // own drive there. They touch within the slack, so both stay.
    let coverage = coverage(&[WINDOW]);
    let hub = store(HubRows {
        drives: vec![session(hub_id(1), 990, 1_400)],
        positions: children(hub_id(100), 1_000, 1_400, 20, Some(hub_id(1))),
        ..HubRows::default()
    });
    let source = SourceRows {
        drives: vec![session(13, 500, 1_030)],
        positions: children(3_000, 500, 990, 10, Some(13)),
        ..SourceRows::default()
    };
    let plan = plan(&coverage, &hub, &source);
    assert_eq!(plan.drives.imported, vec![13]);
    assert_eq!(plan.drives.hub_kept, vec![hub_id(1)]);
    assert!(plan.drives.skipped.is_empty() && plan.drives.superseded.is_empty());
    assert!(plan.positions.moved_hub.is_empty() && plan.positions.moved_source.is_empty());
    assert_replays("abutting", &coverage, &hub, &source);
}

#[test]
fn import_seam_plan_inherited_row_kept_when_hub_saw_more() {
    // The window opened mid-drive: drive 40 went on in the Hub to 1,800 while
    // the source closed it at 1,000 when it stopped. Its stored, Hub-extended
    // row stays; the source's positions before the window are added.
    let coverage = coverage(&[WINDOW]);
    let hub = Store {
        hub: HubRows {
            drives: vec![session(40, 700, 1_800)],
            positions: children(hub_id(121), 1_000, 1_800, 10, Some(40)),
            imported_drives: vec![40],
            ..HubRows::default()
        },
        links: SeamLinks::default(),
    };
    let source = SourceRows {
        drives: vec![session(40, 700, 1_000)],
        positions: children(4_000, 700, 990, 10, Some(40)),
        ..SourceRows::default()
    };
    let plan = plan(&coverage, &hub, &source);
    assert_eq!(plan.drives.inherited, vec![(40, InheritedCopy::Stored)]);
    assert_eq!(plan.drives.hub_kept, vec![40]);
    assert!(plan.drives.imported.is_empty() && plan.drives.stale.is_empty());
    assert_eq!(plan.positions.hub_kept_count, 81);
    assert_eq!(plan.positions.imported_count, 30);
    assert!(plan.positions.moved_hub.is_empty() && plan.drives.widened.is_empty());
    // A later source copy that covers the stored extent replaces it; the
    // Hub's positions inside the window stand either way.
    let later = SourceRows {
        drives: vec![session(40, 700, 1_830)],
        ..source.clone()
    };
    let replaced = super::plan_seam(&coverage, &hub.hub, &later, &SeamLinks::default());
    assert_eq!(replaced.drives.inherited, vec![(40, InheritedCopy::Source)]);
    assert_eq!(replaced.drives.imported, vec![40]);
    assert_eq!(replaced.positions.hub_kept_count, 81);
    assert_replays("inherited, Hub saw more", &coverage, &hub, &source);
    assert_replays("inherited, source covers", &coverage, &hub, &later);
}

#[test]
fn import_seam_plan_link_survives_time_correction() {
    // Drive 14 was skipped for the Hub's drive. A later snapshot moves its
    // times an hour away (a time-zone correction): the link keeps it skipped.
    let coverage = coverage(&[WINDOW]);
    let mut hub = store(HubRows {
        drives: vec![session(hub_id(1), 1_200, 1_500)],
        positions: children(hub_id(100), 1_200, 1_500, 10, Some(hub_id(1))),
        ..HubRows::default()
    });
    hub.links.drives.insert(14, hub_id(1));
    let corrected = SourceRows {
        drives: vec![session(14, 4_800, 5_100)],
        positions: children(5_000, 4_800, 5_100, 50, Some(14)),
        ..SourceRows::default()
    };
    let plan = plan(&coverage, &hub, &corrected);
    assert_eq!(plan.drives.skipped, vec![link(14, hub_id(1))]);
    assert!(plan.drives.imported.is_empty() && plan.drives.links_dropped.is_empty());
    assert_eq!(plan.drives.hub_kept, vec![hub_id(1)]);
    // Its positions now lie outside coverage, so they are written under the
    // winner, which widens to hold them.
    assert_eq!(plan.positions.moved_source.len(), 7);
    assert_eq!(plan.drives.widened[0].end_ms, Some(ms(5_100)));
    assert_replays("time correction", &coverage, &hub, &corrected);

    // When the winner goes, the link goes and the row is planned afresh.
    let mut orphaned = hub.clone();
    orphaned.hub.drives.clear();
    orphaned.hub.positions.clear();
    let afresh = super::plan_seam(&coverage, &orphaned.hub, &corrected, &orphaned.links);
    assert_eq!(afresh.drives.links_dropped, vec![14]);
    assert_eq!(afresh.drives.imported, vec![14]);
}

#[test]
fn import_seam_plan_source_deletes_a_skipped_id() {
    // Drive 15 was skipped and linked; the source deletes it. Drive 16 was
    // imported and is deleted too; drive 17 is deleted but a Hub child
    // refers to it, so it stays.
    let coverage = coverage(&[WINDOW]);
    let mut hub = store(HubRows {
        drives: vec![session(hub_id(1), 1_200, 1_500)],
        positions: [
            children(hub_id(100), 1_200, 1_500, 10, Some(hub_id(1))),
            children(hub_id(200), 1_010, 1_020, 10, Some(17)),
        ]
        .concat(),
        imported_drives: vec![16, 17, 18],
        ..HubRows::default()
    });
    hub.links.drives.insert(15, hub_id(1));
    let source = SourceRows {
        drives: vec![session(18, 100, 300)],
        ..SourceRows::default()
    };
    let plan = plan(&coverage, &hub, &source);
    assert_eq!(plan.drives.links_dropped, vec![15]);
    assert_eq!(plan.drives.stale, vec![16]);
    assert_eq!(plan.drives.imported, vec![18]);
    assert_eq!(plan.drives.hub_kept, vec![hub_id(1)]);
    assert!(plan.drives.skipped.is_empty() && plan.drives.superseded.is_empty());
    // A superseded Hub id whose row is already gone stays as memory.
    let mut memory = hub.clone();
    memory.links.drives.insert(hub_id(9), 18);
    let kept = super::plan_seam(&coverage, &memory.hub, &source, &memory.links);
    assert_eq!(kept.drives.links_dropped, vec![15]);
    assert_replays("source deletes", &coverage, &hub, &source);
}

/// A mixed vehicle: windows, an edge crossing, an inherited row, charges.
fn mixed() -> (HubCoverage, Store, SourceRows) {
    let coverage = coverage(&[WINDOW, SECOND_WINDOW]);
    let mut hub = store(HubRows {
        drives: vec![
            session(40, 700, 1_300),
            session(hub_id(1), 1_400, 1_700),
            session(hub_id(2), 5_100, 5_400),
            session(hub_id(3), 5_900, 5_990),
        ],
        charges: vec![
            session(hub_id(1), 1_750, 1_950),
            session(hub_id(2), 5_500, 5_800),
        ],
        positions: [
            children(hub_id(100), 1_000, 1_300, 10, Some(40)),
            children(hub_id(200), 1_400, 1_700, 10, Some(hub_id(1))),
            children(hub_id(300), 5_100, 5_400, 10, Some(hub_id(2))),
            children(hub_id(400), 5_900, 5_990, 10, Some(hub_id(3))),
        ]
        .concat(),
        charge_samples: [
            children(hub_id(500), 1_750, 1_950, 25, Some(hub_id(1))),
            children(hub_id(600), 5_500, 5_800, 25, Some(hub_id(2))),
        ]
        .concat(),
        imported_drives: vec![40, 41],
        imported_charges: vec![70],
    });
    hub.links.drives.insert(43, hub_id(1));
    let source = SourceRows {
        drives: vec![
            session(40, 700, 1_000),
            session(41, 300, 600),
            session(42, 5_850, 6_200),
            session(43, 1_420, 1_690),
            session(44, 2_100, 2_400),
        ],
        charges: vec![session(70, 200, 280), session(71, 5_450, 6_100)],
        positions: [
            children(10_000, 300, 600, 30, Some(41)),
            children(11_000, 700, 990, 10, Some(40)),
            children(12_000, 5_850, 6_200, 25, Some(42)),
            children(13_000, 2_100, 2_400, 50, Some(44)),
            children(14_000, 3_000, 3_100, 50, None),
        ]
        .concat(),
        charge_samples: [
            children(20_000, 200, 280, 20, Some(70)),
            children(21_000, 5_450, 6_100, 50, Some(71)),
        ]
        .concat(),
    };
    (coverage, hub, source)
}

#[test]
fn import_seam_plan_is_deterministic() {
    let (coverage, hub, source) = mixed();
    let expected = plan(&coverage, &hub, &source);
    let mut shuffled_hub = hub.hub.clone();
    let mut shuffled_source = source.clone();
    shuffled_hub.drives.reverse();
    shuffled_hub.charges.reverse();
    shuffled_hub.positions.rotate_left(17);
    shuffled_hub.charge_samples.reverse();
    shuffled_hub.imported_drives.reverse();
    shuffled_source.drives.rotate_left(2);
    shuffled_source.charges.reverse();
    shuffled_source.positions.reverse();
    shuffled_source.charge_samples.rotate_left(3);
    for _ in 0..3 {
        assert_eq!(
            super::plan_seam(&coverage, &shuffled_hub, &shuffled_source, &hub.links),
            expected
        );
    }
    // Duplicated input rows change nothing either.
    shuffled_source.drives.extend(source.drives.clone());
    assert_eq!(
        super::plan_seam(&coverage, &shuffled_hub, &shuffled_source, &hub.links),
        expected
    );
    assert_eq!(expected.drives.skipped, vec![link(43, hub_id(1))]);
    assert_eq!(expected.drives.superseded, vec![link(hub_id(3), 42)]);
    assert_eq!(expected.charges.superseded, vec![link(hub_id(2), 71)]);
    assert_replays("mixed", &coverage, &hub, &source);
}

struct Scenario {
    name: &'static str,
    build: fn() -> (HubCoverage, Store, SourceRows),
    check: fn(&SeamPlan),
}

fn replay_case() -> (HubCoverage, Store, SourceRows) {
    let (coverage, hub, source) = mixed();
    // The store as a first import left it, planned again unchanged.
    let applied = apply(&hub, &source, &plan(&coverage, &hub, &source));
    (coverage, applied, source)
}

fn edge_case() -> (HubCoverage, Store, SourceRows) {
    let coverage = coverage(&[WINDOW]);
    let hub = store(HubRows {
        drives: vec![session(hub_id(1), 1_600, 2_000)],
        charges: vec![session(hub_id(1), 1_900, 2_000)],
        positions: children(hub_id(100), 1_600, 1_990, 10, Some(hub_id(1))),
        charge_samples: children(hub_id(200), 1_900, 1_990, 30, Some(hub_id(1))),
        ..HubRows::default()
    });
    // The source resumed at the handback and recorded the drive and the
    // charge across the closing edge.
    let source = SourceRows {
        drives: vec![session(20, 1_590, 2_300)],
        charges: vec![session(80, 1_890, 2_400)],
        positions: children(1_000, 2_000, 2_300, 10, Some(20)),
        charge_samples: children(2_000, 2_000, 2_400, 40, Some(80)),
    };
    (coverage, hub, source)
}

fn inherited_case() -> (HubCoverage, Store, SourceRows) {
    let coverage = coverage(&[WINDOW]);
    let hub = store(HubRows {
        charges: vec![session(90, 800, 1_500)],
        charge_samples: children(hub_id(100), 1_000, 1_500, 25, Some(90)),
        imported_charges: vec![90],
        ..HubRows::default()
    });
    let source = SourceRows {
        charges: vec![session(90, 800, 1_000)],
        charge_samples: children(5_000, 800, 1_000, 25, Some(90)),
        ..SourceRows::default()
    };
    (coverage, hub, source)
}

fn two_windows_case() -> (HubCoverage, Store, SourceRows) {
    let (coverage, hub, source) = mixed();
    (coverage, hub, source)
}

fn publication_between_windows_case() -> (HubCoverage, Store, SourceRows) {
    // First import after the first window, published; then the second window
    // collects and a later snapshot is planned against both.
    let first_coverage = coverage(&[WINDOW]);
    let first_store = store(HubRows {
        drives: vec![session(hub_id(1), 1_100, 1_400)],
        positions: children(hub_id(100), 1_100, 1_400, 10, Some(hub_id(1))),
        ..HubRows::default()
    });
    let first_source = SourceRows {
        drives: vec![session(30, 200, 500), session(31, 1_120, 1_390)],
        positions: children(1_000, 200, 500, 20, Some(30)),
        ..SourceRows::default()
    };
    let mut published = apply(
        &first_store,
        &first_source,
        &plan(&first_coverage, &first_store, &first_source),
    );
    published.hub.drives.push(session(hub_id(2), 5_200, 5_600));
    published
        .hub
        .positions
        .extend(children(hub_id(300), 5_200, 5_600, 20, Some(hub_id(2))));
    let later = SourceRows {
        drives: [
            first_source.drives.clone(),
            vec![session(32, 3_000, 3_300), session(33, 4_900, 5_610)],
        ]
        .concat(),
        positions: [
            first_source.positions.clone(),
            children(2_000, 3_000, 3_300, 30, Some(32)),
        ]
        .concat(),
        ..SourceRows::default()
    };
    (
        coverage(&[WINDOW, SECOND_WINDOW]),
        published.normalised(),
        later,
    )
}

fn rewrite_after_link_case() -> (HubCoverage, Store, SourceRows) {
    let (coverage, hub, mut source) = mixed();
    // Drive 43 is linked to the Hub's drive; the source rewrites it longer,
    // reaching outside the window.
    for row in &mut source.drives {
        if row.id == 43 {
            *row = session(43, 900, 1_800);
        }
    }
    (coverage, hub, source)
}

fn deletion_after_link_case() -> (HubCoverage, Store, SourceRows) {
    let (coverage, hub, mut source) = mixed();
    source.drives.retain(|row| row.id != 43);
    (coverage, hub, source)
}

fn restore_with_open_window_case() -> (HubCoverage, Store, SourceRows) {
    // A backup taken inside a window was restored: the window is sealed for
    // this vehicle at its last Hub-range row (1,450), so the cutoff ends there.
    let coverage = coverage(&[(WINDOW.0, 1_450)]);
    let hub = store(HubRows {
        drives: vec![session(hub_id(1), 1_100, 1_440)],
        positions: children(hub_id(100), 1_100, 1_440, 10, Some(hub_id(1))),
        ..HubRows::default()
    });
    // The source restarted after the restore and drove on.
    let source = SourceRows {
        drives: vec![session(50, 1_600, 1_900)],
        positions: children(1_000, 1_600, 1_900, 10, Some(50)),
        ..SourceRows::default()
    };
    (coverage, hub, source)
}

#[test]
fn import_seam_plan_scenarios_hold_every_row_once_and_replay() {
    let scenarios = [
        Scenario {
            name: "replay",
            build: replay_case,
            check: |plan| {
                assert!(plan.drives.superseded.is_empty());
                assert!(plan.positions.moved_hub.is_empty());
            },
        },
        Scenario {
            name: "a row across a window edge",
            build: edge_case,
            check: |plan| {
                assert_eq!(plan.drives.superseded, vec![link(hub_id(1), 20)]);
                assert_eq!(plan.charges.superseded, vec![link(hub_id(1), 80)]);
                assert_eq!(plan.positions.moved_hub.len(), 40);
                assert_eq!(plan.charge_samples.moved_hub.len(), 4);
                assert!(plan.positions.hub_removed.is_empty());
            },
        },
        Scenario {
            name: "an inherited open session",
            build: inherited_case,
            check: |plan| {
                assert_eq!(plan.charges.inherited, vec![(90, InheritedCopy::Stored)]);
                assert_eq!(plan.charge_samples.hub_kept_count, 21);
                assert_eq!(plan.charge_samples.imported_count, 8);
            },
        },
        Scenario {
            name: "two windows",
            build: two_windows_case,
            check: |plan| {
                assert_eq!(plan.drives.inherited, vec![(40, InheritedCopy::Stored)]);
                assert_eq!(plan.drives.imported, vec![41, 42, 44]);
                assert_eq!(plan.drives.hub_kept, vec![40, hub_id(1), hub_id(2)]);
                assert_eq!(plan.charges.imported, vec![70, 71]);
                assert_eq!(plan.charges.hub_kept, vec![hub_id(1)]);
                assert_eq!(plan.positions.imported_count, 11 + 30 + 9 + 7 + 3);
            },
        },
        Scenario {
            name: "publication between windows",
            build: publication_between_windows_case,
            check: |plan| {
                assert_eq!(plan.drives.imported, vec![30, 32, 33]);
                assert_eq!(plan.drives.skipped, vec![link(31, hub_id(1))]);
                assert_eq!(plan.drives.superseded, vec![link(hub_id(2), 33)]);
                assert_eq!(plan.positions.moved_hub.len(), 21);
            },
        },
        Scenario {
            name: "a source rewrite after a link exists",
            build: rewrite_after_link_case,
            check: |plan| {
                assert!(plan.drives.skipped.contains(&link(43, hub_id(1))));
                assert!(!plan.drives.imported.contains(&43));
            },
        },
        Scenario {
            name: "a deletion after a link exists",
            build: deletion_after_link_case,
            check: |plan| {
                assert_eq!(plan.drives.links_dropped, vec![43]);
                assert!(plan.drives.stale.is_empty());
                assert!(plan.drives.hub_kept.contains(&hub_id(1)));
            },
        },
        Scenario {
            name: "a restore with a window open",
            build: restore_with_open_window_case,
            check: |plan| {
                assert_eq!(plan.drives.hub_kept, vec![hub_id(1)]);
                assert_eq!(plan.drives.imported, vec![50]);
                assert_eq!(plan.positions.hub_kept_count, 35);
                assert_eq!(plan.positions.imported_count, 31);
                assert!(plan.positions.hub_removed.is_empty());
            },
        },
    ];
    for scenario in scenarios {
        let (coverage, hub, source) = (scenario.build)();
        let first = plan(&coverage, &hub, &source);
        (scenario.check)(&first);
        assert_replays(scenario.name, &coverage, &hub, &source);
    }
}

/// Review finding #39: a restart gap from the 5 s watermark tombstones a Hub
/// position collected after the last tick; one from the first whole second
/// after the Hub's newest row keeps it.
#[test]
fn m08_t39_restart_gap_after_the_newest_hub_row_keeps_it() {
    let row = ChildRow {
        id: hub_id(1),
        at_ms: ms(1_103) + 400,
        parent: None,
    };
    let hub = store(HubRows {
        positions: vec![row],
        ..HubRows::default()
    });
    let source = SourceRows::default();
    // The window ran 1_000 to 2_000; the service stopped after 1_103.4 and
    // started again at 1_700; the last persisted tick was 1_100.
    let from_watermark = coverage(&[(1_000, 1_100), (1_700, 2_000)]);
    assert_eq!(plan(&from_watermark, &hub, &source).positions.hub_removed, vec![hub_id(1)]);
    let from_newest_row = coverage(&[(1_000, 1_104), (1_700, 2_000)]);
    let kept = plan(&from_newest_row, &hub, &source);
    assert!(kept.positions.hub_removed.is_empty());
    assert_eq!(kept.positions.hub_kept_count, 1);
    assert_replays("restart gap after the newest row", &from_newest_row, &hub, &source);
}
