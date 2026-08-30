// SPDX-License-Identifier: AGPL-3.0-only

//! Bounded, private projection-state capture for TeslaMate imports.
//!
//! A full source history can contain millions of facts.  This module retains
//! only a digest for every current projected row, and retains canonical JSON
//! only for rows which are already known to be new or changed.  That lets the
//! importer build a sparse typed successor without materialising a history or
//! duplicating every payload in the durable Hub catalogue.

use std::{
    collections::HashSet,
    error::Error,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, params, params_from_iter};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, open, openat, statat, unlinkat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{
    hub_pack::{
        ProjectionCar, ProjectionCharge, ProjectionChargeSample, ProjectionDrive,
        ProjectionPosition, ProjectionState, ProjectionTombstone, ProjectionUpdate,
    },
    protocol::Sha256Digest,
};

/// The hard cap used by the production reader unless a caller deliberately
/// supplies a narrower budget.
pub const DEFAULT_MAX_ROWS: u64 = 20_000_000;
pub const DEFAULT_MAX_STATE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const DEFAULT_MINIMUM_FREE_BYTES: u64 = 512 * 1024 * 1024;
/// A source row larger than this needs an explicit, narrower-or-wider caller
/// contract.  The production direct importer uses this exact value for both
/// durable retention and one decoded successor page.
pub const DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_PAGE_SIZE: u32 = 10_000;

const STAGING_DIRECTORY: &str = ".projection-state";
const IMPORT_GENERATION_NAMESPACE: &str = "v1";
const OWNER_FILE_NAME: &str = "owner.json";
const OWNER_SCHEMA: u8 = 1;
const OWNER_KIND: &str = "teslatlas-hub/teslamate-projection-state/v1";
const STATE_FILE_EXTENSION: &str = "sqlite";
const SQLITE_JOURNAL_SUFFIX: &str = ".sqlite-journal";
const SQLITE_WAL_SUFFIX: &str = ".sqlite-wal";
const SQLITE_SHM_SUFFIX: &str = ".sqlite-shm";
const MIN_STATE_BYTES: u64 = 64 * 1024;
const DIGEST_DOMAIN: &[u8] = b"teslatlas-hub/teslamate-projection-state/v1";
const TRANSFER_DIGEST_DOMAIN: &[u8] =
    b"teslatlas-hub/teslamate-projection-state/sealed-transfer/v1";

/// Fixed schema name used only while copying a sealed source-state spool into
/// the Hub catalogue. It is deliberately not caller-controlled.
pub(crate) const TESLAMATE_PROJECTION_STATE_ATTACHMENT_SCHEMA: &str =
    "teslamate_projection_state_spool";

// DELETE/FULL is deliberate: a sealed state file is the source of truth for a
// sparse successor and must survive a host crash.  Committing each source row
// under that durability policy is prohibitively expensive, however.  Keep one
// short, fixed-size transaction open instead.  This caps both recovery work
// and the amount of unwritten state without retaining an unbounded history in
// memory. Changed payloads have a byte cap too, so a dense changed-history
// pass does not turn one row-count batch into a multi-gigabyte commit.
const WRITE_BATCH_ROWS: u32 = 8_192;
const WRITE_BATCH_CHANGED_PAYLOAD_BYTES: u64 = DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES;
// A source batch can contain one 8 MiB changed payload plus its SQLite
// journal pages. Keep this small fixed margin above the durable free-space
// floor rather than reserving the configured whole-history cap.
const WRITE_BATCH_HEADROOM_BYTES: u64 = 32 * 1024 * 1024;
// Keep dynamic `VALUES` lookups well below SQLite's conservative 999-bind
// build-time limit.  Each requested changed row consumes two bind values.
const CHANGED_PAGE_PAYLOAD_LOOKUP_ROWS: usize = 250;
const MAX_OWNER_MARKER_BYTES: u64 = 1_024;

include!("projection_state/model.rs");
include!("projection_state/store.rs");
include!("projection_state/capture.rs");
include!("projection_state/spool.rs");
include!("projection_state/transfer_validation.rs");

#[cfg(test)]
#[path = "projection_state/tests.rs"]
mod tests;
