# Home Assistant — retain accepted lifecycle and reconnect final Hub

Revision: 2026-09-20, after completion review. **DRAFT_NOT_SENT**.
Overall current-Mac state: **MACOS_REVIEW_REOPENED**.
Read [workspace authority](../../../WORKSPACE_AUTHORITY.md),
[review](../../../docs/development/POST_COMPLETION_REVIEW.md),
[master](../../../docs/development/MASTER_PLAN.md) and
[MR-0–MR-3 directive](../../../docs/development/NEXT_PHASE_PLAN.md).
The prior implementation task completed; this is a bounded repair from that state.

## Retained evidence and current source

No concrete new HA functional defect was found. The current headed lifecycle receipt hash was checked; bounded registry/reauth/restart/removal and real-data projection results remain valid. Final aggregate integration and handoff still depend on the Hub repairs.

Current main HEAD: `b8747341d8d9384ee0925851de96388c5a30ea63` plus existing local changes. See [STATUS.json](STATUS.json)
for original accepted source identities, current review identity and receipt pointers.
Keep immutable receipts; a clean HEAD alone does not identify dirty/untracked code.
No source/runtime mutation or publication was performed by this review.

## Assigned repair

Milestones: MR-3. Findings: MR-F7.

1. Preserve the accepted headed MAC4/real-data subset and unrelated HA baseline; no broad rerun or new guest is needed.
2. With the Hub owner, verify only affected private TLS connectivity, polling/data semantics and restart against the final managed Hub.
3. Record actual final component/config/runtime identity and truthful bounded acceptance without obsolete no-real-data language.

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
