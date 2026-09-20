# Swift — prove fresh post-restart data continuity

Revision: 2026-09-20, after completion review. **DRAFT_NOT_SENT**.
Overall current-Mac state: **MACOS_REVIEW_REOPENED**.
Read [workspace authority](../../../WORKSPACE_AUTHORITY.md),
[review](../../../docs/development/POST_COMPLETION_REVIEW.md),
[master](../../../docs/development/MASTER_PLAN.md) and
[MR-0–MR-3 directive](../../../docs/development/NEXT_PHASE_PLAN.md).
The prior implementation task completed; this is a bounded repair from that state.

## Retained evidence and current source

Native external consumers and the real-data semantic comparison are retained. The restart harness only checked vehicle count and reused pre-restart values, so data-continuity acceptance is reopened.

Current main HEAD: `8c254d78392b795b16c25142cfd6d2bf670a33a2` plus existing local changes. See [STATUS.json](STATUS.json)
for original accepted source identities, current review identity and receipt pointers.
Keep immutable receipts; a clean HEAD alone does not identify dirty/untracked code.
No source/runtime mutation or publication was performed by this review.

## Assigned repair

Milestones: MR-2, MR-3. Findings: MR-F4, MR-F7.

1. Re-fetch current and all bounded history pages after restart; compare fresh canonical semantics and write fresh evidence.
2. Add a negative test where vehicle count is unchanged but data is lost/altered, then run the affected external native consumer checks.
3. Preserve the four distinct public library contracts, explicit CA trust and caller-owned credentials; bind the final source/artifact identity.

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
