# Licensing

Teslatlas Edge is licensed under the GNU Affero General Public License version 3 only
(`AGPL-3.0-only`).

## Controlling licence

The repository's root [LICENSE](../../LICENSE) is the complete, unmodified GNU
Affero General Public License version 3 text. The Rust package metadata declares
`AGPL-3.0-only`. [NOTICE](../../NOTICE) names the copyright holder.

Keep the root licence text intact when copying or distributing the project. Do
not replace it with this summary or use this summary to infer an `or-later`,
commercial, proprietary, dual-licence, contributor-assignment, trade mark or
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

`Cargo.lock` fixes the Rust dependency graph. The
[third-party notices](third-party-notices.md) list each crate in the Edge's
normal dependency graph with its locked version and declared licence. Before
distributing a binary or package, check that list against the exact
`Cargo.lock` you build from and ship the licence and notice files of each crate
at that version.
Keep those notices separate from the project licence and the sidecar notice.

The current repository evidence does not identify a separate commercial,
proprietary, or dual-licence grant. Any relicensing, additional terms,
ownership assertion, or contributor-rights policy requires an owner decision
and appropriate legal review. Earlier valid grants must not be represented as
withdrawn.

See [third-party notices](third-party-notices.md) for the crate inventory and
the receiver attribution and distribution notice.
