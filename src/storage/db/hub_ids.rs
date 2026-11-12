// SPDX-License-Identifier: AGPL-3.0-only

//! The reserved range of row ids the Hub allocates itself (schema v80).
//!
//! Imported rows keep their source ids, which the source holds as 4-byte
//! integers. Rows the Hub collects take ids from `HUB_ID_BASE` through
//! `HUB_ID_MAX`, so every consumer, including the App's 4-byte readers, holds
//! the same id for the same row and an id alone says which side owns it. The
//! importer refuses a source id inside the range.

use thiserror::Error;

/// The first id the Hub allocates: 2^30.
pub const HUB_ID_BASE: i64 = 1 << 30;
/// The last id the Hub allocates: the largest 4-byte signed integer.
pub const HUB_ID_MAX: i64 = i32::MAX as i64;

const _: () = assert!(HUB_ID_BASE > 0 && HUB_ID_BASE < HUB_ID_MAX);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HubIdError {
    #[error("the Hub id range is exhausted: the next id {next} is past {HUB_ID_MAX}")]
    Exhausted { next: i64 },
}

/// Whether `id` lies in the Hub's own range, that is the Hub allocated it.
pub fn is_hub_id(id: i64) -> bool {
    (HUB_ID_BASE..=HUB_ID_MAX).contains(&id)
}

/// The id to allocate for a cursor that holds the next unallocated id, as the
/// lifecycle state keeps it. A cursor below the range (fresh, restored or
/// imported state) is raised to `HUB_ID_BASE`; a cursor past `HUB_ID_MAX`
/// refuses, so an id outside the range is never handed out.
pub fn next_hub_id(cursor: i64) -> Result<i64, HubIdError> {
    let next = cursor.max(HUB_ID_BASE);
    if next > HUB_ID_MAX {
        return Err(HubIdError::Exhausted { next });
    }
    Ok(next)
}
