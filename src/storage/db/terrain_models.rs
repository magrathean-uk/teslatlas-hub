// SPDX-License-Identifier: AGPL-3.0-only

/// C9 R6: a position is imported only when both maxima cover it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportBoundary {
    pub max_position_id: i64,
    pub max_date_ms: i64,
}

/// C9 R12: a vehicle and its cursor and import boundaries for terrain selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainVehicle {
    pub vehicle_id: Uuid,
    pub cursor_position_id: i64,
    pub stored_boundary: ImportBoundary,
    pub watermark_boundary: ImportBoundary,
}

/// C9 R8: the bounded forward window of one vehicle.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainForwardWindow {
    pub candidates: Vec<TerrainCandidate>,
    pub examined_through: i64,
}

/// C9 R10: a due retry and its candidate, absent when the row must be retired.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainRetry {
    pub vehicle_id: Uuid,
    pub position_id: i64,
    pub attempts: i64,
    pub retry_after_ms: i64,
    pub candidate: Option<TerrainCandidate>,
}

/// C9 R24: totals of the capped per-vehicle terrain backlog counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerrainBacklog {
    pub rows_unexamined: u64,
    pub retries_due: u64,
    pub retries_waiting: u64,
}

/// C9 R58: tells a caller whether a boundary raise interrupted completed tidy work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainBoundaryRaise {
    Unchanged,
    Raised,
    RaisedAndRearmed,
}

/// C9 R58: progress and completion of the current terrain tidy run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainTidyProgress {
    pub run: i64,
    pub examined: i64,
    pub cleared: i64,
    pub requeued: i64,
    pub removed: i64,
    pub kept: i64,
    pub drives_changed: i64,
    pub armed_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
}

/// C9 R58: the durable identity under which a pass prepared its chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerrainTidyRun {
    pub run: i64,
    pub armed_at_ms: Option<i64>,
}

impl TerrainTidyProgress {
    /// C9 R58: retain this identity from the pass's marker read until commit.
    pub fn identity(&self) -> TerrainTidyRun {
        TerrainTidyRun {
            run: self.run,
            armed_at_ms: self.armed_at_ms,
        }
    }
}

/// C9 R55, C9 R56: one bounded chunk, or a re-arm that invalidated its work.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerrainTidyApplied {
    pub examined: u32,
    pub cleared: u32,
    pub requeued: u32,
    pub removed: u32,
    pub kept: u32,
    pub drives_changed: u32,
    pub complete: bool,
    pub rearmed: bool,
}

/// C9 R45, C9 R53: the acquisition identity of the bytes sampled for an outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileProvenance {
    pub tile_name: String,
    pub tile_hash: String,
    pub dataset_source: String,
    pub dataset_version: String,
}

/// C9 R17: a capped exponential delay, measured from the outcome's commit time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrySchedule {
    pub base_ms: i64,
    pub cap_ms: i64,
}

/// C9 R17, section 5: prevent shift overflow without changing delays below a cap.
pub(crate) const K_RETRY_SHIFT_MAX: i64 = 32;

impl RetrySchedule {
    /// C9 R17: clamp the exponent before multiplying, including very old retries.
    pub fn delay_ms(self, attempts_after: i64) -> i64 {
        let shift = (attempts_after.max(1) - 1).min(K_RETRY_SHIFT_MAX) as u32;
        self.base_ms.saturating_mul(1_i64 << shift).min(self.cap_ms)
    }
}

/// C9 R45: one sampled value, terminal gap, or scheduled acquisition failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerrainOutcomeKind {
    Filled {
        elevation_m: i16,
        tile: TileProvenance,
    },
    Void {
        tile: TileProvenance,
        error_code: Option<&'static str>,
    },
    Failed {
        error_code: &'static str,
        schedule: RetrySchedule,
        tile: Option<TileProvenance>,
    },
}

/// C9 R16: the selected position and queue against which a result is checked.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainOutcome {
    pub candidate: TerrainCandidate,
    pub forward: bool,
    pub kind: TerrainOutcomeKind,
}

/// C9 R9, C9 R15: one vehicle's durable outcomes and proposed forward frontier.
pub struct TerrainBatch<'a> {
    pub vehicle_id: Uuid,
    pub outcomes: &'a [TerrainOutcome],
    pub retire: &'a [i64],
    pub cursor_to: Option<i64>,
    pub committed_at_ms: i64,
}

/// C9 R16: committed counts, undecided stale positions and the cursor actually raised.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerrainBatchApplied {
    /// C9 R58: abandon remaining prepared work and read the tidy marker again.
    pub rearmed: bool,
    pub filled: u32,
    pub void: u32,
    pub failed: u32,
    pub stale_dropped: u32,
    pub stale_held: Vec<i64>,
    pub retired: u32,
    pub drives_changed: u32,
    pub cursor_written: Option<i64>,
}

/// C9 R8: maximum forward rows examined in a vehicle window.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "bounds terrain forward selection")
)]
pub(crate) const K_SCAN_ROWS: u32 = 5_000;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "bounds terrain pass candidates")
)]
pub(crate) const K_PASS_CANDIDATES: u32 = 1_000;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "bounds terrain retry selection")
)]
pub(crate) const K_RETRY_SLICE: u32 = 250;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "caps each vehicle's terrain backlog counts")
)]
pub(crate) const K_STATUS_COUNT_CAP: u32 = 10_000;

/// C9 R54: the immediate provenance effect of a materialised-position rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainRewrite {
    Unchanged,
    Requeued,
    Removed,
}

/// C9 R54: corrections and the old/current drives to finalise after all positions.
#[must_use = "finalise every returned drive after the transaction's position mutations"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainRewriteApplied {
    pub outcome: TerrainRewrite,
    pub recompute_drive_ids: Vec<i64>,
}
