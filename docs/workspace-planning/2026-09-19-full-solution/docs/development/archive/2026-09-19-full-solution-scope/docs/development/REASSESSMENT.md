# Programme reassessment — 2026-09-19

**The adoption milestone is complete within its accepted scope.** G0–G7 and secondary
Home Assistant Container lifecycle are accepted. The previous reassessment described
the 2026-09-18 reset, before execution advanced. Original root planning bytes are in
[the reconciliation archive](archive/2026-09-19-planning-reconciliation/manifest.json).

| Product | Accepted result | Remaining boundary |
| --- | --- | --- |
| Hub | Debian/Mac standalone, packed TS, durable Edge r5, external Swift r2, HA peer | Installed lifecycle, minimum Mac floor, named-source import/parity |
| TypeScript | Packed Node and Chrome 153 on macOS 27 arm64; G3 bindings | Package handoff deltas and broader supported-runtime assertions |
| Edge | Durable Debian synthetic projection before ACK, deduplication/outage/restart, drained state | Installed lifecycle and separately authorized non-synthetic evidence |
| Swift | External SwiftPM macOS 27 consumer; maintained example and adoption guide | Declared platform/compiler floors; no App/iOS runtime claim |
| Protocol | Exact G3 admission and canonical/vendored profile identity | Reproducible unpublished developer bundle; contract changes only for concrete deltas |
| Home Assistant | Container 2026.8.3 on Debian ARM64: config/polling/outage/reauth/reload/restart/redaction | Replacement-upgrade, HACS/HA OS/Supervisor and real-data scopes |

[MASTER_PLAN.md](MASTER_PLAN.md) links exact receipts; [APP_V7_READINESS.md](APP_V7_READINESS.md)
is the accepted handoff. No runtime journey was rerun during this reconciliation.
Historical cohorts and their credentials/private roots remain closed.

## Current source verification

All six active products were on `main` and their pre-upload HEADs matched GitHub.
Completed implementation, receipts and plans were still dirty/untracked. The root
workspace has no usable Git repository: Git falls through to malformed
`/Users/bolyki/.git`. That unrelated file is not repaired. Upload products independently;
record root-planning retention and actual outcomes in [UPLOAD_STATUS.json](UPLOAD_STATUS.json).

Read-only reconciliation checks passed:

- `python3 hub/scripts/sync-ecosystem-versions.py --workspace "$PWD" --check-g3`
- Pinned Node 26.7.0 `npm --prefix teslatlas-sdk-typescript run protocol:check`

These verify recorded receipt/profile/lock alignment, not a fresh runtime or equality
of every current file to old runtime content manifests. Original receipts retain
original HEAD/diff/content identities; publication commits are separate.

## Planning corrections

MASTER_PLAN, REASSESSMENT and per-task next-action/remaining fields lagged accepted
G3/G6/G7/HA status. Root records and all six active PLAN/STATUS records now describe
one accepted baseline and one unstarted next campaign. [HISTORY_REVIEW.md](HISTORY_REVIEW.md)
records current and archived conversation coverage. App/Viewer plans and transcripts
are excluded.

Next work is [L1 installed Debian lifecycle](NEXT_PHASE_PLAN.md), then Apple-silicon
lifecycle/floors and source/package handoff. D1 needs a fresh named input.
[GOAL_PROMPT.md](GOAL_PROMPT.md) is prepared for the owner to send. This planning task
creates no goal and starts no automation, implementation, build or runtime.
