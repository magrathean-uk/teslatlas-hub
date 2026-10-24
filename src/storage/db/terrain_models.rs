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
