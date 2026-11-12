// SPDX-License-Identifier: AGPL-3.0-only

/// Schema v80: the collection windows this store covers, written beside the
/// window ledger so the import cutoff travels with `hub.sqlite` through backup,
/// restore and a new host. `gaps_json` holds the window's collection gaps at
/// handback; `vehicle_ends_json` and `sealed` record how a window left open in
/// a restored store was sealed per vehicle.
const COLLECTION_COVERAGE_SCHEMA: &str = "CREATE TABLE collection_coverage (
    seq INTEGER PRIMARY KEY,
    grant_digest TEXT NOT NULL,
    opened_at_ms INTEGER NOT NULL,
    closed_at_ms INTEGER CHECK(closed_at_ms IS NULL OR closed_at_ms >= opened_at_ms),
    gaps_json TEXT NOT NULL CHECK(json_valid(gaps_json)),
    vehicle_ends_json TEXT CHECK(vehicle_ends_json IS NULL OR json_valid(vehicle_ends_json)),
    sealed TEXT NOT NULL
) STRICT;";
