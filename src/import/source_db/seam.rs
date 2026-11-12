// SPDX-License-Identifier: AGPL-3.0-only

//! The seam between imported history and the Hub's own collection.
//!
//! The cutoff lives in the store (schema v80 `collection_coverage`), written
//! beside the window ledger at window open and at handback, so it travels in
//! `hub.sqlite` through backup, restore and a new host. [`HubCoverage`] is the
//! cutoff one vehicle reads: the union of the recorded windows minus the gaps
//! whose vehicle is absent or is that vehicle.
//!
//! [`plan_seam`] is the pure plan (design M03-SEAM-R3, section 4): for one
//! vehicle it decides which side keeps each drive, charge, position and
//! charge sample, from the coverage (the cutoff), the Hub's rows (ownership by
//! the reserved id range) and the recorded seam links (earlier decisions). It
//! writes nothing; the store and the packs apply it.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;
use thiserror::Error;

use crate::db::hub_ids::is_hub_id;
use crate::db::{CoverageError, coverage_intervals_for_vehicle};

/// Why the coverage cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SeamError {
    /// A window is still open in the store: the cutoff is not recorded, so
    /// import must wait for the handback (or a restore's seal).
    #[error("a collection window has no handback yet")]
    WindowNotHandedBack,
    /// The coverage records could not be read.
    #[error("the collection coverage could not be read: {0}")]
    Store(String),
}

impl From<CoverageError> for SeamError {
    fn from(error: CoverageError) -> Self {
        match error {
            CoverageError::WindowOpen { .. } => Self::WindowNotHandedBack,
            other => Self::Store(other.to_string()),
        }
    }
}

/// The intervals in which the Hub collected one vehicle itself, in
/// milliseconds, half-open, sorted and disjoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubCoverage {
    intervals: Vec<(i64, i64)>,
}

impl HubCoverage {
    /// The coverage `vehicle` has in the store behind `connection`.
    pub fn for_store(connection: &Connection, vehicle: &str) -> Result<Self, SeamError> {
        Ok(Self {
            intervals: coverage_intervals_for_vehicle(connection, vehicle)?,
        })
    }

    /// Coverage made of the given half-open intervals in milliseconds; empty
    /// intervals are dropped and the rest are sorted and merged.
    pub fn from_intervals<I>(intervals: I) -> Self
    where
        I: IntoIterator<Item = (i64, i64)>,
    {
        let mut pieces: Vec<(i64, i64)> = intervals
            .into_iter()
            .filter(|(start, end)| end > start)
            .collect();
        pieces.sort_unstable();
        let mut merged: Vec<(i64, i64)> = Vec::with_capacity(pieces.len());
        for (start, end) in pieces {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        Self { intervals: merged }
    }

    /// The covered intervals in milliseconds.
    pub fn intervals(&self) -> &[(i64, i64)] {
        &self.intervals
    }

    /// The covered interval holding `ms`, if any.
    fn interval_at(&self, ms: i64) -> Option<(i64, i64)> {
        let index = self.intervals.partition_point(|(start, _)| *start <= ms);
        let candidate = *self.intervals.get(index.checked_sub(1)?)?;
        (ms < candidate.1).then_some(candidate)
    }

    /// Whether the Hub was collecting at `ms`.
    pub fn contains(&self, ms: i64) -> bool {
        self.interval_at(ms).is_some()
    }

    /// Whether `[start_ms, end_ms]` lies wholly inside one covered interval;
    /// otherwise it crosses a coverage edge or lies outside coverage.
    pub fn holds(&self, start_ms: i64, end_ms: i64) -> bool {
        self.interval_at(start_ms)
            .is_some_and(|(_, interval_end)| end_ms.max(start_ms) <= interval_end)
    }
}

/// Overlap two sessions need to belong to one group, and the margin within
/// which one side's extent covers the other's. Sessions that only abut at a
/// handover overlap by less and stay two groups.
pub const SEAM_SLACK_MS: i64 = 60_000;

/// One drive or charging session, on either side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionRow {
    pub id: i64,
    pub start_ms: i64,
    /// Absent while the session is unfinished; it then counts as far as its
    /// start only.
    pub end_ms: Option<i64>,
}

impl SessionRow {
    fn extent(&self) -> (i64, i64) {
        (
            self.start_ms,
            self.end_ms.unwrap_or(self.start_ms).max(self.start_ms),
        )
    }
}

/// One position or charge sample, on either side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChildRow {
    pub id: i64,
    pub at_ms: i64,
    /// The drive of a position or the charging session of a sample.
    pub parent: Option<i64>,
}

/// The store's side of one vehicle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubRows {
    /// Hub-range drives, and the inherited open drives the Hub extended (they
    /// keep their source ids), with the times the store holds.
    pub drives: Vec<SessionRow>,
    /// Hub-range charges and inherited open charges, as for drives.
    pub charges: Vec<SessionRow>,
    /// Hub-range positions.
    pub positions: Vec<ChildRow>,
    /// Hub-range charge samples.
    pub charge_samples: Vec<ChildRow>,
    /// Source drive ids the store holds from earlier imports.
    pub imported_drives: Vec<i64>,
    /// Source charge ids the store holds from earlier imports.
    pub imported_charges: Vec<i64>,
}

/// One source snapshot of one vehicle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceRows {
    pub drives: Vec<SessionRow>,
    pub charges: Vec<SessionRow>,
    pub positions: Vec<ChildRow>,
    pub charge_samples: Vec<ChildRow>,
}

/// The recorded seam decisions of one vehicle (`import_seam_links`), loser id
/// to winner id per session entity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeamLinks {
    pub drives: BTreeMap<i64, i64>,
    pub charges: BTreeMap<i64, i64>,
}

/// One decision: the losing row and the row that stands for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeamLink {
    pub loser_id: i64,
    pub winner_id: i64,
}

/// Which copy of an inherited open session the store keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InheritedCopy {
    /// The source copy covers the stored extent and replaces it.
    Source,
    /// The stored, Hub-extended row stays.
    Stored,
}

/// A kept session's bounds after widening for the children it gained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bounds {
    pub id: i64,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
}

/// The plan for one session entity (drives or charges).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPlan {
    /// Source ids written from the source copy, ascending (an inherited
    /// session whose source copy wins is among them).
    pub imported: Vec<i64>,
    /// Store rows kept as stored, ascending: Hub-range sessions and inherited
    /// sessions whose stored row stays.
    pub hub_kept: Vec<i64>,
    /// Inherited sessions (one id on both sides) and the copy kept.
    pub inherited: Vec<(i64, InheritedCopy)>,
    /// Source sessions skipped (seam-skipped in parity), each linked.
    pub skipped: Vec<SeamLink>,
    /// Hub sessions removed with a tombstone, each linked.
    pub superseded: Vec<SeamLink>,
    /// Source-id rows the store holds that go with a tombstone (stale rule).
    pub stale: Vec<i64>,
    /// Loser ids whose recorded link goes.
    pub links_dropped: Vec<i64>,
    /// Kept sessions whose start or end widens to hold moved or kept
    /// children; totals stay the session's own.
    pub widened: Vec<Bounds>,
}

/// A kept child given a new parent because its own lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChildMove {
    pub id: i64,
    pub from: i64,
    pub to: i64,
}

/// The plan for one child entity (positions or charge samples). Children go
/// by instant only: inside coverage the Hub's, outside the source's. Only the
/// exceptions are listed, so a long history does not make a long plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildPlan {
    /// Source children written (outside coverage).
    pub imported_count: u64,
    /// Source child ids not written (inside coverage), ascending.
    pub skipped: Vec<i64>,
    /// Hub children kept (inside coverage).
    pub hub_kept_count: u64,
    /// Hub child ids removed (outside coverage), ascending.
    pub hub_removed: Vec<i64>,
    /// Written source children whose parent changes, by id.
    pub moved_source: Vec<ChildMove>,
    /// Kept Hub children whose parent changes, by id.
    pub moved_hub: Vec<ChildMove>,
}

/// What each side keeps for one vehicle and one source snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeamPlan {
    pub drives: SessionPlan,
    pub charges: SessionPlan,
    pub positions: ChildPlan,
    pub charge_samples: ChildPlan,
}

/// Plans the seam for one vehicle. Pure: the same inputs in any order give
/// the same plan, and a plan applied and planned again repeats itself.
///
/// Sessions are decided in this order: recorded links (a linked loser stays
/// lost while its winner exists, whatever its times; the link goes when the
/// loser is gone from the source or its winner is gone), inherited open
/// sessions (one id on both sides), then overlap groups with the extent rule.
/// Children follow by instant; a kept child whose parent lost moves to a
/// winner.
pub fn plan_seam(
    coverage: &HubCoverage,
    hub: &HubRows,
    source: &SourceRows,
    links: &SeamLinks,
) -> SeamPlan {
    let drives = plan_sessions(
        coverage,
        &hub.drives,
        &source.drives,
        &links.drives,
        &hub.imported_drives,
        &hub.positions,
    );
    let charges = plan_sessions(
        coverage,
        &hub.charges,
        &source.charges,
        &links.charges,
        &hub.imported_charges,
        &hub.charge_samples,
    );
    let (drive_plan, positions) = drives.finish(coverage, &hub.positions, &source.positions);
    let (charge_plan, charge_samples) =
        charges.finish(coverage, &hub.charge_samples, &source.charge_samples);
    SeamPlan {
        drives: drive_plan,
        charges: charge_plan,
        positions,
        charge_samples,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Hub,
    Source,
}

#[derive(Debug, Clone)]
enum Fate {
    /// Kept with these bounds.
    Kept { start: i64, end: Option<i64> },
    /// Lost to `winner`; kept children move to one of `winners`.
    Lost { winner: i64, winners: Vec<i64> },
}

struct SessionDecisions {
    plan: SessionPlan,
    fates: BTreeMap<i64, Fate>,
}

fn by_id(rows: &[SessionRow]) -> BTreeMap<i64, SessionRow> {
    let mut map = BTreeMap::new();
    for row in rows {
        map.entry(row.id).or_insert(*row);
    }
    map
}

fn covers(a: (i64, i64), b: (i64, i64)) -> bool {
    a.0 <= b.0.saturating_add(SEAM_SLACK_MS) && a.1.saturating_add(SEAM_SLACK_MS) >= b.1
}

fn overlap(a: (i64, i64), b: (i64, i64)) -> i64 {
    a.1.min(b.1).saturating_sub(a.0.max(b.0))
}

fn hull(rows: &[SessionRow]) -> (i64, i64) {
    rows.iter().map(SessionRow::extent).fold(
        (i64::MAX, i64::MIN),
        |(start, end), (row_start, row_end)| (start.min(row_start), end.max(row_end)),
    )
}

fn union_length(rows: &[SessionRow]) -> i64 {
    let mut extents: Vec<(i64, i64)> = rows.iter().map(SessionRow::extent).collect();
    extents.sort_unstable();
    let mut total = 0_i64;
    let mut current: Option<(i64, i64)> = None;
    for (start, end) in extents {
        current = match current {
            Some((open, close)) if start <= close => Some((open, close.max(end))),
            Some((open, close)) => {
                total = total.saturating_add(close - open);
                Some((start, end))
            }
            None => Some((start, end)),
        };
    }
    if let Some((open, close)) = current {
        total = total.saturating_add(close - open);
    }
    total
}

/// The winner standing for `loser`: the most overlap (a gap counts as
/// negative overlap, so the nearest), then the smallest id.
fn best_winner(loser: &SessionRow, winners: &[SessionRow]) -> i64 {
    winners
        .iter()
        .max_by(|a, b| {
            overlap(loser.extent(), a.extent())
                .cmp(&overlap(loser.extent(), b.extent()))
                .then(b.id.cmp(&a.id))
        })
        .map_or(loser.id, |winner| winner.id)
}

fn plan_sessions(
    coverage: &HubCoverage,
    hub_rows: &[SessionRow],
    source_rows: &[SessionRow],
    links: &BTreeMap<i64, i64>,
    imported_before: &[i64],
    hub_children: &[ChildRow],
) -> SessionDecisions {
    let hub = by_id(hub_rows);
    let source = by_id(source_rows);
    let mut plan = SessionPlan::default();
    let mut fates: BTreeMap<i64, Fate> = BTreeMap::new();
    let mut decided: BTreeSet<i64> = BTreeSet::new();
    let exists = |id: i64| hub.contains_key(&id) || source.contains_key(&id);

    // Recorded links first.
    let mut linked: Vec<(i64, i64)> = Vec::new();
    for (&loser, &winner) in links {
        let side = if is_hub_id(loser) {
            Side::Hub
        } else {
            Side::Source
        };
        let present = match side {
            Side::Hub => hub.contains_key(&loser),
            Side::Source => source.contains_key(&loser),
        };
        if !present {
            // A skipped source id the source deleted: its link goes. A
            // superseded Hub id already removed stays as high-water memory.
            if side == Side::Source {
                plan.links_dropped.push(loser);
            }
            continue;
        }
        if winner != loser && exists(winner) {
            linked.push((loser, winner));
            decided.insert(loser);
        } else {
            // The winner is gone: the loser is planned afresh.
            plan.links_dropped.push(loser);
        }
    }

    // Inherited open sessions: one id on both sides.
    for (id, stored) in &hub {
        if is_hub_id(*id) || decided.contains(id) {
            continue;
        }
        let Some(copy) = source.get(id) else {
            continue;
        };
        let (kept, which) = if covers(copy.extent(), stored.extent()) {
            (copy, InheritedCopy::Source)
        } else {
            (stored, InheritedCopy::Stored)
        };
        plan.inherited.push((*id, which));
        match which {
            InheritedCopy::Source => plan.imported.push(*id),
            InheritedCopy::Stored => plan.hub_kept.push(*id),
        }
        fates.insert(
            *id,
            Fate::Kept {
                start: kept.start_ms,
                end: kept.end_ms,
            },
        );
        decided.insert(*id);
    }

    // Stale rule: a source-id row the store holds goes only when the source
    // no longer lists it and no Hub-range child links to it.
    let held_by_hub_child: BTreeSet<i64> = hub_children
        .iter()
        .filter(|child| is_hub_id(child.id))
        .filter_map(|child| child.parent)
        .collect();
    let stale: BTreeSet<i64> = imported_before
        .iter()
        .copied()
        .filter(|id| {
            !is_hub_id(*id)
                && !source.contains_key(id)
                && !held_by_hub_child.contains(id)
                && !decided.contains(id)
        })
        .collect();
    plan.stale = stale.iter().copied().collect();

    // Overlap groups over everything still undecided.
    let mut items: Vec<(Side, SessionRow)> = hub
        .values()
        .filter(|row| !decided.contains(&row.id) && !stale.contains(&row.id))
        .map(|row| (Side::Hub, *row))
        .chain(
            source
                .values()
                .filter(|row| !decided.contains(&row.id))
                .map(|row| (Side::Source, *row)),
        )
        .collect();
    items.sort_unstable_by_key(|(side, row)| (row.extent(), *side, row.id));
    let mut groups: Vec<Vec<(Side, SessionRow)>> = Vec::new();
    let mut group_end = i64::MIN;
    for (side, row) in items {
        let (start, end) = row.extent();
        match groups.last_mut() {
            Some(group) if group_end.min(end).saturating_sub(start) > SEAM_SLACK_MS => {
                group.push((side, row));
                group_end = group_end.max(end);
            }
            _ => {
                groups.push(vec![(side, row)]);
                group_end = end;
            }
        }
    }
    for group in groups {
        let hub_side: Vec<SessionRow> = group
            .iter()
            .filter(|(side, _)| *side == Side::Hub)
            .map(|(_, row)| *row)
            .collect();
        let source_side: Vec<SessionRow> = group
            .iter()
            .filter(|(side, _)| *side == Side::Source)
            .map(|(_, row)| *row)
            .collect();
        let winner_side = if hub_side.is_empty() {
            Side::Source
        } else if source_side.is_empty() {
            Side::Hub
        } else {
            let (hub_hull, source_hull) = (hull(&hub_side), hull(&source_side));
            match (covers(hub_hull, source_hull), covers(source_hull, hub_hull)) {
                (true, true) => {
                    let source_reaches_out = source_side.iter().any(|row| {
                        let (start, end) = row.extent();
                        !coverage.holds(start, end)
                    });
                    if source_reaches_out {
                        Side::Source
                    } else {
                        Side::Hub
                    }
                }
                (false, true) => Side::Source,
                (true, false) => Side::Hub,
                (false, false) => {
                    if union_length(&source_side) >= union_length(&hub_side) {
                        Side::Source
                    } else {
                        Side::Hub
                    }
                }
            }
        };
        let (winners, losers) = match winner_side {
            Side::Hub => (hub_side, source_side),
            Side::Source => (source_side, hub_side),
        };
        let winner_ids: Vec<i64> = winners.iter().map(|row| row.id).collect();
        for row in &winners {
            match winner_side {
                Side::Hub => plan.hub_kept.push(row.id),
                Side::Source => plan.imported.push(row.id),
            }
            fates.insert(
                row.id,
                Fate::Kept {
                    start: row.start_ms,
                    end: row.end_ms,
                },
            );
        }
        for row in &losers {
            fates.insert(
                row.id,
                Fate::Lost {
                    winner: best_winner(row, &winners),
                    winners: winner_ids.clone(),
                },
            );
        }
    }

    // Linked losers: follow the winner while it lost in this plan, so a
    // chain of decisions ends at a kept row.
    for (loser, winner) in &linked {
        let mut target = *winner;
        for _ in 0..=links.len() {
            match fates.get(&target) {
                Some(Fate::Lost { winner, .. }) => target = *winner,
                _ => break,
            }
        }
        fates.insert(
            *loser,
            Fate::Lost {
                winner: target,
                winners: vec![target],
            },
        );
    }

    // Losers and their links, in id order.
    for (id, fate) in &fates {
        let Fate::Lost { winner, .. } = fate else {
            continue;
        };
        let link = SeamLink {
            loser_id: *id,
            winner_id: *winner,
        };
        if hub.contains_key(id) && !source.contains_key(id) {
            plan.superseded.push(link);
        } else {
            plan.skipped.push(link);
        }
    }
    SessionDecisions { plan, fates }
}

impl SessionDecisions {
    /// Plans the children by instant and moves those whose parent lost.
    fn finish(
        mut self,
        coverage: &HubCoverage,
        hub_children: &[ChildRow],
        source_children: &[ChildRow],
    ) -> (SessionPlan, ChildPlan) {
        let original: BTreeMap<i64, (i64, Option<i64>)> = self
            .fates
            .iter()
            .filter_map(|(id, fate)| match fate {
                Fate::Kept { start, end } => Some((*id, (*start, *end))),
                Fate::Lost { .. } => None,
            })
            .collect();
        let mut bounds = original.clone();
        let mut children = ChildPlan::default();

        let mut hub_rows: Vec<ChildRow> = hub_children.to_vec();
        hub_rows.sort_unstable();
        hub_rows.dedup_by_key(|row| row.id);
        let mut source_rows: Vec<ChildRow> = source_children.to_vec();
        source_rows.sort_unstable();
        source_rows.dedup_by_key(|row| row.id);

        for (side, rows) in [(Side::Hub, &hub_rows), (Side::Source, &source_rows)] {
            for row in rows.iter() {
                let inside = coverage.contains(row.at_ms);
                let kept = match side {
                    Side::Hub => inside,
                    Side::Source => !inside,
                };
                if !kept {
                    match side {
                        Side::Hub => children.hub_removed.push(row.id),
                        Side::Source => children.skipped.push(row.id),
                    }
                    continue;
                }
                match side {
                    Side::Hub => children.hub_kept_count += 1,
                    Side::Source => children.imported_count += 1,
                }
                let Some(parent) = row.parent else {
                    continue;
                };
                let target = match self.fates.get(&parent) {
                    Some(Fate::Kept { .. }) => parent,
                    Some(Fate::Lost { winners, .. }) => {
                        let Some(to) = nearest(winners, row.at_ms, &original) else {
                            continue;
                        };
                        let moved = ChildMove {
                            id: row.id,
                            from: parent,
                            to,
                        };
                        match side {
                            Side::Hub => children.moved_hub.push(moved),
                            Side::Source => children.moved_source.push(moved),
                        }
                        to
                    }
                    None => continue,
                };
                if let Some((start, end)) = bounds.get_mut(&target) {
                    *start = (*start).min(row.at_ms);
                    if let Some(end) = end {
                        *end = (*end).max(row.at_ms);
                    }
                }
            }
        }

        for (id, now) in &bounds {
            if original.get(id) != Some(now) {
                self.plan.widened.push(Bounds {
                    id: *id,
                    start_ms: now.0,
                    end_ms: now.1,
                });
            }
        }
        let plan = &mut self.plan;
        for list in [
            &mut plan.imported,
            &mut plan.hub_kept,
            &mut plan.stale,
            &mut plan.links_dropped,
        ] {
            list.sort_unstable();
            list.dedup();
        }
        plan.inherited.sort_unstable();
        plan.skipped.sort_unstable();
        plan.superseded.sort_unstable();
        children.skipped.sort_unstable();
        children.hub_removed.sort_unstable();
        children.moved_hub.sort_unstable();
        children.moved_source.sort_unstable();
        (self.plan, children)
    }
}

/// The winner holding `at_ms`, else the nearest one, then the smallest id.
fn nearest(winners: &[i64], at_ms: i64, bounds: &BTreeMap<i64, (i64, Option<i64>)>) -> Option<i64> {
    winners
        .iter()
        .filter_map(|id| bounds.get(id).map(|bound| (*id, *bound)))
        .min_by_key(|(id, (start, end))| {
            let end = end.unwrap_or(i64::MAX);
            let distance = if at_ms < *start {
                start.saturating_sub(at_ms)
            } else if at_ms > end {
                at_ms.saturating_sub(end)
            } else {
                0
            };
            (distance, *id)
        })
        .map(|(id, _)| id)
}

#[cfg(test)]
mod tests;
