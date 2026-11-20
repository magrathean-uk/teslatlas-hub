// SPDX-License-Identifier: AGPL-3.0-only

//! Conservative planning of imported broken-drive repairs (M03-T26).
//!
//! An import captures a bounded candidate index from the source rows it
//! already reads: every unclosed source drive and the drive that immediately
//! follows it in start order, with their first and last samples and the
//! charge and state evidence of the gap between them. The index is bounded by
//! source drive rows, never by history samples, and a scheduled run plans from
//! it alone, without another source query.
//!
//! Planning is pure. It joins one maximal run of single-sample structural
//! fragments to the completed drive that follows it, only when every
//! continuity rule below holds and every member is import-owned and
//! unprotected. It invents no route, sample or endpoint and changes no served
//! row; publication is a separate, later step.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::source_db_projection::{
    SourceDbChargingProcess, SourceDbDrive, SourceDbPosition, SourceDbState,
};

/// The version of the rules below; part of every component identity.
pub const REPAIR_POLICY_VERSION: i64 = 1;
/// The largest sample-to-sample gap across a run and into its destination.
pub const MAX_SAMPLE_GAP_MS: i64 = 120_000;
/// The largest span from the first fragment sample to the destination.
pub const MAX_PREFIX_MS: i64 = 30 * 60_000;
/// How long the destination must have ended before it may be repaired.
pub const DESTINATION_STABILITY_MS: i64 = 48 * 3_600_000;
/// The fastest travel a gap may imply.
pub const MAX_GAP_SPEED_KMH: f64 = 250.0;
/// Rows read from the stored index per page.
pub const CANDIDATE_PAGE_ROWS: usize = 100;
/// The most rows one component may change: each fragment's suppressed drive
/// and reparented sample, and the destination.
pub const MAX_COMPONENT_CHANGED_ROWS: usize = 100;
const MAX_RUN_FRAGMENTS: usize = (MAX_COMPONENT_CHANGED_ROWS - 1) / 2;

/// One captured source sample: identity, time and the continuity evidence.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RepairSample {
    pub position_id: i64,
    pub date_ms: i64,
    pub odometer_km: Option<f64>,
    pub speed_kmh: Option<i64>,
}

/// What the source states say about a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateCoverage {
    /// One online state covers the whole gap.
    Online,
    /// An asleep or offline state overlaps the gap.
    Interrupted,
    /// No state evidence covers the whole gap.
    Unknown,
}

/// The source evidence between a row's last sample and the next drive's first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapEvidence {
    pub charge_between: bool,
    pub state: StateCoverage,
}

/// One indexed source drive row with its captured evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepairInput {
    pub drive_id: i64,
    pub car_id: i64,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub distance_km: Option<f64>,
    pub duration_min: Option<i64>,
    pub sample_count: u32,
    pub first_sample: Option<RepairSample>,
    pub last_sample: Option<RepairSample>,
    pub previous_drive_id: Option<i64>,
    /// The drive before this one in start order is itself unclosed.
    pub previous_unclosed: bool,
    pub next_drive_id: Option<i64>,
    /// Between this row's last sample and the next drive's first sample.
    pub gap_to_next: Option<GapEvidence>,
}

impl RepairInput {
    /// No end and no distance: what the import leaves out of journeys.
    pub fn is_unclosed(&self) -> bool {
        self.end_ms.is_none()
    }

    /// A structural fragment: no end, distance or duration and one sample.
    pub fn is_structural_fragment(&self) -> bool {
        self.end_ms.is_none()
            && self.distance_km.is_none()
            && self.duration_min.is_none()
            && self.sample_count == 1
            && self.first_sample.is_some()
            && self.first_sample == self.last_sample
    }

    /// The digest of the captured evidence; a change invalidates attestations
    /// and every record built on the row.
    pub fn input_digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"tesatlas-hub/drive-repair-input/v1\0");
        hasher.update(serde_json::to_vec(self).expect("repair input serialises"));
        hasher.finalize().into()
    }
}

/// The candidate index of one car, sorted by source start time then id.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BrokenDriveCandidateIndex {
    pub car_id: i64,
    pub rows: Vec<RepairInput>,
}

impl BrokenDriveCandidateIndex {
    /// The digest of the whole captured index.
    pub fn capture_digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"tesatlas-hub/drive-repair-capture/v1\0");
        hasher.update(self.car_id.to_be_bytes());
        for row in &self.rows {
            hasher.update(row.input_digest());
        }
        hasher.finalize().into()
    }
}

#[derive(Debug, Clone, Copy)]
struct CapturedDrive {
    start_ms: i64,
    end_ms: Option<i64>,
    distance_km: Option<f64>,
    duration_min: Option<i64>,
    sample_count: u32,
    first_sample: Option<RepairSample>,
    last_sample: Option<RepairSample>,
}

/// Builds the candidate index while an import streams its source rows. Holds
/// one entry per source drive of the car, never one per history sample.
#[derive(Debug)]
pub struct BrokenDriveCandidateCapture {
    car_id: i64,
    drives: HashMap<i64, CapturedDrive>,
    charges: Vec<(i64, Option<i64>)>,
    states: Vec<(bool, i64, Option<i64>)>,
}

impl BrokenDriveCandidateCapture {
    pub fn new(drives: &[SourceDbDrive], car_id: i64) -> Self {
        let drives = drives
            .iter()
            .filter(|drive| drive.car_id == car_id)
            .map(|drive| {
                (
                    drive.id,
                    CapturedDrive {
                        start_ms: drive.start_date_ms,
                        end_ms: drive.end_date_ms,
                        distance_km: drive.distance_km,
                        duration_min: drive.duration_min,
                        sample_count: 0,
                        first_sample: None,
                        last_sample: None,
                    },
                )
            })
            .collect();
        Self {
            car_id,
            drives,
            charges: Vec::new(),
            states: Vec::new(),
        }
    }

    pub fn observe_position(&mut self, position: &SourceDbPosition) {
        if position.car_id != self.car_id {
            return;
        }
        let Some(drive) = position.drive_id.and_then(|id| self.drives.get_mut(&id)) else {
            return;
        };
        let sample = RepairSample {
            position_id: position.id,
            date_ms: position.date_ms,
            odometer_km: position.odometer,
            speed_kmh: position.speed,
        };
        let key = |s: &RepairSample| (s.date_ms, s.position_id);
        drive.sample_count = drive.sample_count.saturating_add(1);
        if drive.first_sample.is_none_or(|first| key(&sample) < key(&first)) {
            drive.first_sample = Some(sample);
        }
        if drive.last_sample.is_none_or(|last| key(&sample) > key(&last)) {
            drive.last_sample = Some(sample);
        }
    }

    pub fn observe_charging_process(&mut self, process: &SourceDbChargingProcess) {
        if process.car_id == self.car_id {
            self.charges.push((process.start_date_ms, process.end_date_ms));
        }
    }

    pub fn observe_state(&mut self, state: &SourceDbState) {
        if state.car_id == self.car_id {
            self.states
                .push((state.state == "online", state.start_date_ms, state.end_date_ms));
        }
    }

    fn gap_evidence(&self, from_ms: i64, to_ms: i64) -> GapEvidence {
        let overlaps = |start: i64, end: Option<i64>| start < to_ms && end.is_none_or(|e| e > from_ms);
        let charge_between = self.charges.iter().any(|&(s, e)| overlaps(s, e));
        let state = if self
            .states
            .iter()
            .any(|&(online, s, e)| !online && overlaps(s, e))
        {
            StateCoverage::Interrupted
        } else if self
            .states
            .iter()
            .any(|&(online, s, e)| online && s <= from_ms && e.is_none_or(|e| e >= to_ms))
        {
            StateCoverage::Online
        } else {
            StateCoverage::Unknown
        };
        GapEvidence {
            charge_between,
            state,
        }
    }

    pub fn finish(self) -> BrokenDriveCandidateIndex {
        let mut order = self
            .drives
            .iter()
            .map(|(&id, drive)| (drive.start_ms, id))
            .collect::<Vec<_>>();
        order.sort_unstable();
        let mut selected = vec![false; order.len()];
        for (position, &(_, id)) in order.iter().enumerate() {
            if self.drives[&id].end_ms.is_none() {
                selected[position] = true;
                if let Some(next) = selected.get_mut(position + 1) {
                    *next = true;
                }
            }
        }
        let rows = order
            .iter()
            .enumerate()
            .filter(|(position, _)| selected[*position])
            .map(|(position, &(_, id))| {
                let drive = self.drives[&id];
                let previous = position.checked_sub(1).map(|p| order[p].1);
                let next = order.get(position + 1).map(|&(_, next)| next);
                let gap_to_next = match (drive.end_ms, drive.last_sample, next) {
                    (None, Some(last), Some(next)) => self.drives[&next]
                        .first_sample
                        .map(|first| self.gap_evidence(last.date_ms, first.date_ms)),
                    _ => None,
                };
                RepairInput {
                    drive_id: id,
                    car_id: self.car_id,
                    start_ms: drive.start_ms,
                    end_ms: drive.end_ms,
                    distance_km: drive.distance_km,
                    duration_min: drive.duration_min,
                    sample_count: drive.sample_count,
                    first_sample: drive.first_sample,
                    last_sample: drive.last_sample,
                    previous_drive_id: previous,
                    previous_unclosed: previous
                        .is_some_and(|previous| self.drives[&previous].end_ms.is_none()),
                    next_drive_id: next,
                    gap_to_next,
                }
            })
            .collect();
        BrokenDriveCandidateIndex {
            car_id: self.car_id,
            rows,
        }
    }
}

/// Why a candidate fragment is left unchanged. Codes are stable and carry no
/// sample values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RepairRefusal {
    Terminal,
    NotStructural,
    NoCompletedDestination,
    AmbiguousStart,
    AmbiguousOrder,
    DestinationIncomplete,
    DestinationZeroDistance,
    DestinationNotStable,
    MissingEvidence,
    LongGap,
    PrefixTooLong,
    Interrupted,
    UnknownState,
    OdometerReset,
    ImplausibleSpeed,
    StationaryBoundary,
    NotImportOwned,
    CollectionCoverage,
    Protected,
    UnknownEditHistory,
    Suppressed,
    Oversized,
}

impl RepairRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::NotStructural => "not_structural",
            Self::NoCompletedDestination => "no_completed_destination",
            Self::AmbiguousStart => "ambiguous_start",
            Self::AmbiguousOrder => "ambiguous_order",
            Self::DestinationIncomplete => "destination_incomplete",
            Self::DestinationZeroDistance => "destination_zero_distance",
            Self::DestinationNotStable => "destination_not_stable",
            Self::MissingEvidence => "missing_evidence",
            Self::LongGap => "long_gap",
            Self::PrefixTooLong => "prefix_too_long",
            Self::Interrupted => "interrupted",
            Self::UnknownState => "unknown_state",
            Self::OdometerReset => "odometer_reset",
            Self::ImplausibleSpeed => "implausible_speed",
            Self::StationaryBoundary => "stationary_boundary",
            Self::NotImportOwned => "not_import_owned",
            Self::CollectionCoverage => "collection_coverage",
            Self::Protected => "protected",
            Self::UnknownEditHistory => "unknown_edit_history",
            Self::Suppressed => "suppressed",
            Self::Oversized => "oversized",
        }
    }
}

/// A member's role in a candidate component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRole {
    Fragment,
    Destination,
}

/// Ownership, protection and suppression answers the store supplies.
pub trait RepairEligibility {
    type Error;
    /// `None` admits the member; a refusal leaves the whole component.
    fn member_refusal(
        &mut self,
        member: &RepairInput,
        role: MemberRole,
    ) -> Result<Option<RepairRefusal>, Self::Error>;
    /// An undone component stays suppressed until an explicit retry.
    fn suppressed(&mut self, component_key: &[u8; 32]) -> Result<bool, Self::Error>;
}

/// One planned repair: a maximal run of fragments joined to its destination.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedComponent {
    pub component_key: [u8; 32],
    pub input_digest: [u8; 32],
    pub destination_drive_id: i64,
    pub fragment_drive_ids: Vec<i64>,
    pub sample_position_ids: Vec<i64>,
    /// The destination's effective start: the first fragment sample.
    pub effective_start_ms: i64,
    pub changed_rows: usize,
    pub members: Vec<RepairInput>,
}

/// The planning outcome of one candidate component or fragment.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanOutcome {
    Planned(PlannedComponent),
    Refused { reason: RepairRefusal, fragments: u64 },
}

/// Planning counts. Every unclosed candidate is counted once, either planned
/// or under one reason; none of these are added to the import's own counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanCounts {
    pub candidates: u64,
    pub planned_components: u64,
    pub planned_fragments: u64,
    pub refused: BTreeMap<String, u64>,
}

impl PlanCounts {
    fn note(&mut self, outcome: &PlanOutcome) {
        match outcome {
            PlanOutcome::Planned(component) => {
                self.planned_components += 1;
                self.planned_fragments += component.fragment_drive_ids.len() as u64;
            }
            PlanOutcome::Refused { reason, fragments } => {
                *self.refused.entry(reason.code().to_owned()).or_default() += fragments;
            }
        }
    }

    pub fn refused_total(&self) -> u64 {
        self.refused.values().sum()
    }
}

/// Plans components from index rows pushed in source start order, one page at
/// a time; a run continues across page boundaries. Memory is bounded by the
/// largest admissible component.
#[derive(Debug)]
pub struct ComponentPlanner {
    vehicle_key: String,
    now_ms: i64,
    run: Vec<RepairInput>,
    run_fragments: u64,
    run_next: Option<i64>,
    ambiguous_start: bool,
    counts: PlanCounts,
}

impl ComponentPlanner {
    pub fn new(vehicle_key: impl Into<String>, now_ms: i64) -> Self {
        Self {
            vehicle_key: vehicle_key.into(),
            now_ms,
            run: Vec::new(),
            run_fragments: 0,
            run_next: None,
            ambiguous_start: false,
            counts: PlanCounts::default(),
        }
    }

    pub fn counts(&self) -> &PlanCounts {
        &self.counts
    }

    fn emit(&mut self, outcomes: &mut Vec<PlanOutcome>, outcome: PlanOutcome) {
        self.counts.note(&outcome);
        outcomes.push(outcome);
    }

    fn refuse_run(&mut self, outcomes: &mut Vec<PlanOutcome>, reason: RepairRefusal) {
        let fragments = std::mem::take(&mut self.run_fragments);
        self.run.clear();
        self.run_next = None;
        self.emit(outcomes, PlanOutcome::Refused { reason, fragments });
    }

    /// Push the next row in (start, id) order.
    pub fn push<E: RepairEligibility>(
        &mut self,
        row: RepairInput,
        eligibility: &mut E,
    ) -> Result<Vec<PlanOutcome>, E::Error> {
        let mut outcomes = Vec::new();
        if row.is_unclosed() {
            self.counts.candidates += 1;
        }
        if self.run_fragments > 0 {
            if self.run_next == Some(row.drive_id) {
                if !row.is_unclosed() {
                    let outcome = self.evaluate(&row, eligibility)?;
                    self.run.clear();
                    self.run_fragments = 0;
                    self.run_next = None;
                    self.emit(&mut outcomes, outcome);
                    return Ok(outcomes);
                }
                if row.is_structural_fragment() {
                    self.extend(row);
                    return Ok(outcomes);
                }
                self.refuse_run(&mut outcomes, RepairRefusal::NoCompletedDestination);
                self.emit(
                    &mut outcomes,
                    PlanOutcome::Refused {
                        reason: RepairRefusal::NotStructural,
                        fragments: 1,
                    },
                );
                return Ok(outcomes);
            }
            let reason = if self.run_next.is_none() {
                RepairRefusal::Terminal
            } else {
                RepairRefusal::MissingEvidence
            };
            self.refuse_run(&mut outcomes, reason);
        }
        if row.is_unclosed() {
            if row.is_structural_fragment() {
                self.ambiguous_start = row.previous_unclosed;
                self.extend(row);
            } else {
                self.emit(
                    &mut outcomes,
                    PlanOutcome::Refused {
                        reason: RepairRefusal::NotStructural,
                        fragments: 1,
                    },
                );
            }
        }
        Ok(outcomes)
    }

    fn extend(&mut self, row: RepairInput) {
        self.run_fragments += 1;
        self.run_next = row.next_drive_id;
        if self.run.len() < MAX_RUN_FRAGMENTS + 1 {
            self.run.push(row);
        }
    }

    /// End of the index: a pending run has no destination.
    pub fn finish(mut self) -> (PlanCounts, Vec<PlanOutcome>) {
        let mut outcomes = Vec::new();
        if self.run_fragments > 0 {
            let reason = if self.run_next.is_none() {
                RepairRefusal::Terminal
            } else {
                RepairRefusal::MissingEvidence
            };
            self.refuse_run(&mut outcomes, reason);
        }
        (self.counts, outcomes)
    }

    fn evaluate<E: RepairEligibility>(
        &self,
        destination: &RepairInput,
        eligibility: &mut E,
    ) -> Result<PlanOutcome, E::Error> {
        let fragments = self.run_fragments;
        let refused = |reason| Ok(PlanOutcome::Refused { reason, fragments });
        if self.ambiguous_start {
            return refused(RepairRefusal::AmbiguousStart);
        }
        if self.run.len() > MAX_RUN_FRAGMENTS {
            return refused(RepairRefusal::Oversized);
        }
        if let Some(reason) = self.destination_refusal(destination) {
            return refused(reason);
        }
        for member in &self.run {
            if let Some(reason) = eligibility.member_refusal(member, MemberRole::Fragment)? {
                return refused(reason);
            }
        }
        if let Some(reason) = eligibility.member_refusal(destination, MemberRole::Destination)? {
            return refused(reason);
        }
        if let Some(reason) = self.continuity_refusal(destination) {
            return refused(reason);
        }
        let (component_key, input_digest) = self.component_identity(destination);
        if eligibility.suppressed(&component_key)? {
            return refused(RepairRefusal::Suppressed);
        }
        let mut members = self.run.clone();
        members.push(destination.clone());
        Ok(PlanOutcome::Planned(PlannedComponent {
            component_key,
            input_digest,
            destination_drive_id: destination.drive_id,
            fragment_drive_ids: self.run.iter().map(|row| row.drive_id).collect(),
            sample_position_ids: self
                .run
                .iter()
                .filter_map(|row| row.first_sample.map(|s| s.position_id))
                .collect(),
            effective_start_ms: self.run[0].first_sample.map_or(self.run[0].start_ms, |s| s.date_ms),
            changed_rows: self.run.len() * 2 + 1,
            members,
        }))
    }

    fn destination_refusal(&self, destination: &RepairInput) -> Option<RepairRefusal> {
        let (Some(end_ms), Some(distance)) = (destination.end_ms, destination.distance_km) else {
            return Some(RepairRefusal::DestinationIncomplete);
        };
        if !distance.is_finite() || distance <= 0.0 {
            return Some(RepairRefusal::DestinationZeroDistance);
        }
        if destination.first_sample.is_none() || end_ms < destination.start_ms {
            return Some(RepairRefusal::MissingEvidence);
        }
        if end_ms.saturating_add(DESTINATION_STABILITY_MS) > self.now_ms {
            return Some(RepairRefusal::DestinationNotStable);
        }
        None
    }

    fn continuity_refusal(&self, destination: &RepairInput) -> Option<RepairRefusal> {
        let Some(target) = destination.first_sample else {
            return Some(RepairRefusal::MissingEvidence);
        };
        let mut samples = Vec::with_capacity(self.run.len() + 1);
        let mut previous_start = None;
        for (index, fragment) in self.run.iter().enumerate() {
            let sample = fragment.first_sample?;
            let next_start = self
                .run
                .get(index + 1)
                .map_or(destination.start_ms, |next| next.start_ms);
            if previous_start.is_some_and(|start| fragment.start_ms <= start)
                || sample.date_ms < fragment.start_ms
                || sample.date_ms > next_start
            {
                return Some(RepairRefusal::AmbiguousOrder);
            }
            previous_start = Some(fragment.start_ms);
            let Some(gap) = fragment.gap_to_next else {
                return Some(RepairRefusal::MissingEvidence);
            };
            if gap.charge_between || gap.state == StateCoverage::Interrupted {
                return Some(RepairRefusal::Interrupted);
            }
            if gap.state == StateCoverage::Unknown {
                return Some(RepairRefusal::UnknownState);
            }
            samples.push(sample);
        }
        if previous_start.is_some_and(|start| destination.start_ms <= start)
            || target.date_ms < destination.start_ms
        {
            return Some(RepairRefusal::AmbiguousOrder);
        }
        samples.push(target);
        if target.date_ms - samples[0].date_ms > MAX_PREFIX_MS {
            return Some(RepairRefusal::PrefixTooLong);
        }
        for pair in samples.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            let gap_ms = to.date_ms - from.date_ms;
            if gap_ms <= 0 {
                return Some(RepairRefusal::AmbiguousOrder);
            }
            if gap_ms > MAX_SAMPLE_GAP_MS {
                return Some(RepairRefusal::LongGap);
            }
            let (Some(a), Some(b)) = (from.odometer_km, to.odometer_km) else {
                return Some(RepairRefusal::MissingEvidence);
            };
            if !a.is_finite() || !b.is_finite() {
                return Some(RepairRefusal::MissingEvidence);
            }
            if b < a {
                return Some(RepairRefusal::OdometerReset);
            }
            let (Some(from_speed), Some(to_speed)) = (from.speed_kmh, to.speed_kmh) else {
                return Some(RepairRefusal::MissingEvidence);
            };
            if from_speed <= 0 || to_speed <= 0 {
                return Some(RepairRefusal::StationaryBoundary);
            }
            let hours = gap_ms as f64 / 3_600_000.0;
            if (b - a) / hours > MAX_GAP_SPEED_KMH {
                return Some(RepairRefusal::ImplausibleSpeed);
            }
        }
        None
    }

    fn component_identity(&self, destination: &RepairInput) -> ([u8; 32], [u8; 32]) {
        let mut inputs = Sha256::new();
        inputs.update(b"tesatlas-hub/drive-repair-members/v1\0");
        for member in self.run.iter().chain(std::iter::once(destination)) {
            inputs.update(member.input_digest());
        }
        let input_digest: [u8; 32] = inputs.finalize().into();
        let mut key = Sha256::new();
        key.update(b"tesatlas-hub/drive-repair-component/v1\0");
        key.update(REPAIR_POLICY_VERSION.to_be_bytes());
        key.update((self.vehicle_key.len() as u64).to_be_bytes());
        key.update(self.vehicle_key.as_bytes());
        key.update(destination.car_id.to_be_bytes());
        for member in self.run.iter().chain(std::iter::once(destination)) {
            key.update(member.drive_id.to_be_bytes());
        }
        key.update(input_digest);
        (key.finalize().into(), input_digest)
    }
}

#[cfg(test)]
#[path = "broken_drive_repair/tests.rs"]
mod tests;
