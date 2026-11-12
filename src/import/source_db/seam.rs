// SPDX-License-Identifier: AGPL-3.0-only

//! The seam between imported history and the Hub's own collection.
//!
//! The cutoff lives in the store (schema v80 `collection_coverage`), written
//! beside the window ledger at window open and at handback, so it travels in
//! `hub.sqlite` through backup, restore and a new host. [`HubCoverage`] is the
//! cutoff one vehicle reads: the union of the recorded windows minus the gaps
//! whose vehicle is absent or is that vehicle. The plan rules that use it
//! belong to M03-T14.

use rusqlite::Connection;
use thiserror::Error;

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

    /// The covered intervals in milliseconds.
    pub fn intervals(&self) -> &[(i64, i64)] {
        &self.intervals
    }

    /// Whether the Hub was collecting at `ms`.
    pub fn contains(&self, ms: i64) -> bool {
        let index = self.intervals.partition_point(|(start, _)| *start <= ms);
        index
            .checked_sub(1)
            .and_then(|index| self.intervals.get(index))
            .is_some_and(|(_, end)| ms < *end)
    }
}
