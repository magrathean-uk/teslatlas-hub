# Licensing

This page explains the repository's licence and third-party notice; it does not alter either.

## Controlling licence

The repository's root [LICENSE](../../LICENSE) is the complete, unmodified GNU
Affero General Public License version 3 text. The Rust package metadata declares
`AGPL-3.0-only`. [NOTICE](../../NOTICE) names the copyright holder. This document
explains the repository material and does not alter the root licence or add terms.

Keep the root licence text intact when copying or distributing the project. Do
not replace it with this summary or use this summary to infer an `or-later`,
commercial, proprietary, dual-licence, contributor-assignment, trademark, or
ownership grant.

## Tesla Fleet Telemetry sidecar

The optional `teslatlas-fleet-telemetry` sidecar is built from Tesla Fleet
Telemetry v0.9.4 at revision
`d64c73ab65e7c5fb5fc12b35fe507e2c6054227b`. It retains its upstream Apache
License 2.0 terms. The repository includes the corresponding Apache licence at
[Apache-2.0-fleet-telemetry.txt](Apache-2.0-fleet-telemetry.txt) and documents
the Teslatlas overlay at
`packaging/fleet-telemetry-bridge/0001-teslatlas-http-dispatcher.patch`.

When distributing the sidecar, preserve its upstream and modified-file notices
and provide the pinned upstream source and overlay patch as required by the
existing third-party notice. The bridge lock records the pinned archive and
patch digests. The sidecar's Apache licence remains its own grant.

## Other dependencies and release material

`Cargo.lock` fixes the Rust dependency graph, but it is not a dependency
licence-notice inventory. Before distributing a binary or package, prepare and
verify notices for the exact locked dependency graph and delivered artifacts.
Keep those notices separate from the project licence and the sidecar notice.

The current repository evidence does not identify a separate commercial,
proprietary, or dual-licence grant. Any relicensing, additional terms,
ownership assertion, or contributor-rights policy requires an owner decision
and appropriate legal review. Earlier valid grants must not be represented as
withdrawn.

See [third-party notices](third-party-notices.md) for the existing receiver
attribution and distribution notice.
