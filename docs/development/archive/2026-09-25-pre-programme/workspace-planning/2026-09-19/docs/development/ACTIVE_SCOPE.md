# Active scope — planning reconciliation and source upload

Revision: 2026-09-19. G0–G7 and bounded HA Container lifecycle are accepted.
This task reviews history, reconciles all six active plans and root records, uploads
authorized active branches, and writes an unsent next `/goal`. No new development
or runtime campaign starts here.

Repositories: Hub, TypeScript SDK, Edge, Swift SDK, Protocol and Home Assistant.
App and Viewer remain excluded, including their plans/tasks/transcripts. Targets:
Debian 13 ARM64 and Apple-silicon macOS. x86/amd64/Intel/Azure remain paused.

The draft [NEXT_PHASE_PLAN.md](NEXT_PHASE_PLAN.md) covers installed Debian lifecycle,
Mac lifecycle/floors, source/package handoff and a named-source parity input dependency.
HA remains secondary. Preserve accepted proof without relabelling it as installed,
minimum-floor, production or real-data evidence.

One-off GitHub upload is source storage only. Preserve local files outside the explicit
snapshot. No force push, tag, release, CI or deployment. Root is not a working Git
repository; see [UPLOAD_STATUS.json](UPLOAD_STATUS.json).
