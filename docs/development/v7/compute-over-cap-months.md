# Design: months that exceed the prepared-map caps (Compute V-24)

Status: design for plan group G2-0/G2-7; no implementation yet. Owner decision 2026-10-08: Hub failure memo, App per-block isolation, trim or split only under an `ALGORITHM_VERSION` bump, and measure the heaviest real month first.

## Problem

Protocol 1.3.0 (`prepared-pack-v1-contract.json`) caps a prepared month at 8,388,608 bytes counted as 8 + Σ(40 + segment bytes), 4,096 tiles per month and 200,000 segments per tile. Compute enforces a global 1,048,576 raw unique segments and a 16-byte-per-entry bound (`lib.rs:28-43, 538-560, 620-643`), which is looser than the contract's 40 bytes, so the Hub re-checks after generation (`prepared_map.rs:330-334`). Both the Hub (`compute_month`) and the App (`canonical_map.rs:62-83`) generate one block per UTC month.

When a month exceeds a cap the whole month fails:
- the Hub retries the previous complete month after every import and keeps no memo (`physical_publication.rs:95-105`);
- the App propagates the error with `?` from `run_canonical_cpu_work` (`canonical_map_bridge.rs:952`) and `rebuild_dirty_canonical_year` (`map_artifact_rebuild_bridge.rs:187`), so one bad month aborts the whole year rebuild.

Synthetic data reaches the cap at about 1,650 km of distinct road in one month. Real-world frequency is unknown.

## Step 0: measure before designing further

A read-only query on the household store (and the App mirror) estimates distinct road per month without running Compute: count distinct `(round(lat,4), round(lon,4))` cells per vehicle per UTC month over `materialised_positions`, and report the top three months with their raw position counts. Compare with the synthetic threshold. If no real month comes within 50% of the cap, only steps 1 and 2 ship in v7 and step 3 waits.

## Step 1: Hub failure memo

New table `prepared_map_failures(source_id, vehicle_id, month, data_revision, calculation_version, reason, measured_bytes, measured_tiles, failed_at_ms, PRIMARY KEY (source_id, vehicle_id, month, data_revision, calculation_version))`.

- `compute_month` writes a row when generation exceeds any cap and skips the month while `(data_revision, calculation_version)` are unchanged.
- A new import or correction that bumps the month's data revision clears the memo for that month by key mismatch; nothing else clears it.
- Read API and sync expose the month as `prepared: unavailable (exceeds cap)` in provenance, so clients fall back to local generation or raw positions instead of waiting.
- Doctor and status list memo rows.

Acceptance: a synthetic over-cap month produces one memo row, the next import does not recompute it (timed), a correction to that month does.

## Step 2: App per-block isolation (App writer)

`rebuild_dirty_canonical_year` treats a failed block as "unavailable" (persisted flag on the block row), continues the remaining months, and reports partial completion. The map shows the month from raw positions or hides it with a note; the year rebuild is never aborted by one block. Keyed on `ALGORITHM_VERSION` so a later Compute change retries once.

## Step 3: make the month fit (only if step 0 shows real months at risk)

Options, in order of preference:
1. Correct Compute's byte accounting to the contract's 40-byte entry so the Hub's post-check cannot disagree with Compute; this changes no output.
2. Trim inside Compute: escalate the simplification tolerance for the highest zoom levels until the month fits, recording the applied tolerance in block metadata. Changes output, so `ALGORITHM_VERSION` bumps and Hub packs republish for affected months.
3. Split months into smaller blocks. Rejected for v7: protocol 1.3.0 fixes one block per month, and the App and Hub both key on month.

## Out of scope here

V-04/V-05/V-06 (progress-callback storm, RDP run four times, capacity accounting) are byte-neutral performance fixes in G2-7 with guard tests and medians; V-13/V-19/V-22 are v7 App work.
