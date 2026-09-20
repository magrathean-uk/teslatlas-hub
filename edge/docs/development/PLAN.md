# Edge — prove the complete native receiver-to-Hub path

Revision: 2026-09-20, after completion review. **DRAFT_NOT_SENT**.
Overall current-Mac state: **MACOS_REVIEW_REOPENED**.
Read [workspace authority](../../../WORKSPACE_AUTHORITY.md),
[review](../../../docs/development/POST_COMPLETION_REVIEW.md),
[master](../../../docs/development/MASTER_PLAN.md) and
[MR-0–MR-3 directive](../../../docs/development/NEXT_PHASE_PLAN.md).
The prior implementation task completed; this is a bounded repair from that state.

## Retained evidence and current source

The current native receiver bridge and Edge-to-Hub fault/restart tests passed separately. Their own receipt explicitly excludes a continuous receiver-to-Hub chain; the required combined runtime proof remains open.

Current main HEAD: `bd632c8f800dbf813cc7c1f69968ab4e61153b17` plus existing local changes. See [STATUS.json](STATUS.json)
for original accepted source identities, current review identity and receipt pointers.
Keep immutable receipts; a clean HEAD alone does not identify dirty/untracked code.
No source/runtime mutation or publication was performed by this review.

## Assigned repair

Milestones: MR-1, MR-3. Findings: MR-F3, MR-F6, MR-F7.

1. Coordinate one fresh pinned receiver → actual encrypted spool → app-managed Hub event path with the Hub owner.
2. Prove bounded quota/backpressure, credential rotation, status/doctor and affected recovery at runtime; reuse matching unchanged fault/ACK tests.
3. Record exact current native artifacts and the bounded MAC2 receipt separately from historical G5 identities.

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
