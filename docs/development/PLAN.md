# Hub — repair data preservation and native Mac operation

Revision: 2026-09-20, after completion review. **DRAFT_NOT_SENT**.
Overall current-Mac state: **MACOS_REVIEW_REOPENED**.
Read [workspace authority](../../../WORKSPACE_AUTHORITY.md),
[review](../../../docs/development/POST_COMPLETION_REVIEW.md),
[master](../../../docs/development/MASTER_PLAN.md) and
[MR-0–MR-3 directive](../../../docs/development/NEXT_PHASE_PLAN.md).
The prior implementation task completed; this is a bounded repair from that state.

## Retained evidence and current source

Native UI fixture lifecycle, supported API/signed sync, import/isolated recovery and consumer observations are retained. New import/live-ID collision and the unhandled Legacy identity case remain open; fixture-only native control is insufficient for the combined product.

Current main HEAD: `b37064c46d7b4a72ad5540a27d7fdc5171b8ac80` plus existing local changes. See [STATUS.json](STATUS.json)
for original accepted source identities, current review identity and receipt pointers.
Keep immutable receipts; a clean HEAD alone does not identify dirty/untracked code.
No source/runtime mutation or publication was performed by this review.

## Assigned repair

Milestones: MR-0, MR-1, MR-3. Findings: MR-F1, MR-F2, MR-F3, MR-F6, MR-F7.

1. Prevent successor imports from overwriting non-import-owned live drive IDs; preserve transaction/provenance and affected publication paths.
2. Preserve internal vehicle identity and disabled/custom settings across Legacy same-VIN/provider-EID rotation in single, explicit and batch setup.
3. Make the native source-run control app manage fresh standalone/import-only and configured local Edge modes with strict trust and accurate startup failures.
4. Own the stable native runtime, disk/lock checks, receiver/Edge/HA coordination and final recovery/handoff.

## Pass and handoff

Close only assigned findings with focused negative/positive checks and the affected
final combined-product assertions. Return exact source/artifact/profile identity,
result and limits to the coordinator. Preserve unrelated state; the Hub owner alone
controls shared runtime starts/stops. No acceptance from cached values, partial
success or historical artifacts presented as current.

Use Sol/medium for routine fixes and Sol/high for integration/review, following root
AGENTS.md. Source publication follows the existing explicit authority after review.
No packaging, distribution, extra OS/floor work, new real-data access, Keychain/Touch
ID, signing or TLS bypass. App/Viewer and paused architectures remain excluded.
