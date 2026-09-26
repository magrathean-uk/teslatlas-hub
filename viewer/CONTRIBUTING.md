# Contributing to Teslatlas Viewer

Viewer is a small reference client for the public Hub protocol. Keep changes focused on health, vehicles, current state, recent sessions, data quality, collector freshness, and local pairing lifecycle. Use the public SDK; do not add private Hub routes, vehicle commands, proprietary UI, or private analytics.

## Current development status

Viewer development is deferred by its owner. This documentation refresh does not restart implementation or runtime testing. Read [AGENTS.md](AGENTS.md) and the [development note](docs/development/PLAN.md) before taking on work. An explicit owner scope change can authorize future development; routine steps within that authorized scope do not need repeated confirmation.

## Prepare a change

Explain the user-visible problem and keep the patch small. Preserve unrelated edits. In the owner workspace, use the existing checkout and follow the parent plan; do not create branches, worktrees, or stashes there. Hub services, SDK contracts, and shared compatibility decisions belong to their respective projects.

For future authorized work, the [README](README.md#build-from-source) explains the pinned SDK prerequisite. Choose checks from [verification](docs/verification.md) that exercise the changed behavior. Include the exact source state, command and result, plus any untested path. Fixture and unit results do not establish installed Hub or browser compatibility.

Never attach invitations, authorization headers, credentials, precise locations, private endpoints, raw cursors, or unredacted live screenshots. Use synthetic reproductions. Follow [SECURITY.md](SECURITY.md) for suspected vulnerabilities.

## Review and publication

Describe what changed, why, and what you verified. Distinguish a source review from an executed check. Keep unsupported resources and null values explicit in both code and examples.

GitHub is used for source storage. Do not add CI, security automation, release workflows, tags, binary uploads, or deployment as part of a routine change. Pushes and publication require an explicit owner instruction. This repository has no automated check badge to rely on.

## Licensing

Preserve existing copyright, attribution, and license notices. Do not copy Hub's AGPL terms or contributor-assignment process into Viewer. See [LICENSE](LICENSE) and [licensing](docs/legal/licensing.md) for the existing Apache License 2.0 grant and dependency boundaries.
