# TypeScript — make sync acceptance decisive

Revision: 2026-09-20, after completion review. **DRAFT_NOT_SENT**.
Overall current-Mac state: **MACOS_REVIEW_REOPENED**.
Read [workspace authority](../../../WORKSPACE_AUTHORITY.md),
[review](../../../docs/development/POST_COMPLETION_REVIEW.md),
[master](../../../docs/development/MASTER_PLAN.md) and
[MR-0–MR-3 directive](../../../docs/development/NEXT_PHASE_PLAN.md).
The prior implementation task completed; this is a bounded repair from that state.

## Retained evidence and current source

Packed Node and strict-NSS Firefox data/signature observations are retained. The no-op verification command has a false-positive path and needs repair before overall acceptance.

Current main HEAD: `983e8c4bbdca6158204a21d936cb4324be8621b3` plus existing local changes. See [STATUS.json](STATUS.json)
for original accepted source identities, current review identity and receipt pointers.
Keep immutable receipts; a clean HEAD alone does not identify dirty/untracked code.
No source/runtime mutation or publication was performed by this review.

## Assigned repair

Milestones: MR-2, MR-3. Findings: MR-F5, MR-F7.

1. Make the required sync/no-op gate reject 406/503, invalid signatures and malformed responses; keep partial diagnostics explicitly separate.
2. Add focused negative tests; rerun the affected signed sync gate and final Node/strict-NSS Firefox comparison against the managed Hub.
3. Retain the existing leaf-pin/browser trust boundary and capture exact changed-source/package identity; set pinned Node PATH.

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
