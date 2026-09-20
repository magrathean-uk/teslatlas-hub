# Protocol — retain contracts and verify affected integration

Revision: 2026-09-20, after completion review. **DRAFT_NOT_SENT**.
Overall current-Mac state: **MACOS_REVIEW_REOPENED**.
Read [workspace authority](../../../WORKSPACE_AUTHORITY.md),
[review](../../../docs/development/POST_COMPLETION_REVIEW.md),
[master](../../../docs/development/MASTER_PLAN.md) and
[MR-0–MR-3 directive](../../../docs/development/NEXT_PHASE_PLAN.md).
The prior implementation task completed; this is a bounded repair from that state.

## Retained evidence and current source

No new Protocol implementation defect was established. Current native reference and Hub checker results remain bounded evidence; aggregate product acceptance depends on the Hub/consumer repairs.

Current main HEAD: `c8a3ffab9183a428a61a4759c8d72bd405cbd2ca` plus existing local changes. See [STATUS.json](STATUS.json)
for original accepted source identities, current review identity and receipt pointers.
Keep immutable receipts; a clean HEAD alone does not identify dirty/untracked code.
No source/runtime mutation or publication was performed by this review.

## Assigned repair

Milestones: MR-2, MR-3. Findings: MR-F7.

1. Retain existing rich/current-Hub/Edge reference results; no protocol expansion is planned.
2. Check changed Hub behavior against the existing current-Hub and Edge profiles; do not invent rich Hub endpoints.
3. Supply exact source/profile identity to the final combined run and reconcile status/publication.

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
