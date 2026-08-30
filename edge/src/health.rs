use serde::Serialize;

use crate::spool::SpoolSnapshot;

#[derive(Debug, Serialize)]
pub(crate) struct HealthResponse {
    status: &'static str,
    queue: QueueHealth,
    corrupt_records: u64,
    expired_records: u64,
}

#[derive(Debug, Serialize)]
struct QueueHealth {
    records: usize,
    bytes: u64,
    oldest_age_seconds: u64,
}

impl HealthResponse {
    pub(crate) fn from_snapshot(snapshot: &SpoolSnapshot) -> Self {
        Self {
            status: if snapshot.degraded { "degraded" } else { "ok" },
            queue: QueueHealth {
                records: snapshot.pending_records,
                bytes: snapshot.pending_bytes,
                oldest_age_seconds: snapshot.oldest_age_seconds,
            },
            corrupt_records: snapshot.corrupt_records,
            expired_records: snapshot.expired_records,
        }
    }
}
