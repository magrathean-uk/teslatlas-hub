# Licensing

## Controlling license

The repository's root [LICENSE](../LICENSE) is the complete, unmodified GNU
Affero General Public License version 3 text. The Rust package metadata declares
`AGPL-3.0-only`. This document explains the repository material and does not
alter the root license, add terms, or identify a copyright owner.

Keep the root license text intact when copying or distributing the project. Do
not replace it with this summary or use this summary to infer an `or-later`,
commercial, proprietary, dual-license, contributor-assignment, trademark, or
ownership grant.

## Tesla Fleet Telemetry sidecar

The optional `teslatlas-fleet-telemetry` sidecar is built from Tesla Fleet
Telemetry v0.9.4 at revision
`d64c73ab65e7c5fb5fc12b35fe507e2c6054227b`. It retains its upstream Apache
License 2.0 terms. The repository includes the corresponding Apache license at
[Apache-2.0-fleet-telemetry.txt](legal/Apache-2.0-fleet-telemetry.txt) and documents
the Teslatlas overlay at
`packaging/fleet-telemetry-bridge/0001-teslatlas-http-dispatcher.patch`.

When distributing the sidecar, preserve its upstream and modified-file notices
and provide the pinned upstream source and overlay patch as required by the
existing third-party notice. The bridge lock records the pinned archive and
patch digests. The sidecar's Apache license remains its own grant.

## Other dependencies and release material

`Cargo.lock` fixes the Rust dependency graph, but it is not a dependency
license-notice inventory. Before distributing a binary or package, prepare and
verify notices for the exact locked dependency graph and delivered artifacts.
Keep those notices separate from the project license and the sidecar notice.

The current repository evidence does not identify a separate commercial,
proprietary, or dual-license grant. Any relicensing, additional terms,
ownership assertion, or contributor-rights policy requires an owner decision
and appropriate legal review. Earlier valid grants must not be represented as
withdrawn.

See [third-party notices](legal/third-party-notices.md) for the existing
receiver attribution and distribution notice.
