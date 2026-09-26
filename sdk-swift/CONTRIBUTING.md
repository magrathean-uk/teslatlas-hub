# Contributing

Start by identifying the library and contract your change affects. The [architecture](docs/architecture.md) explains the three contract boundaries; the [development guide](docs/development.md) lists local checks.

## Keep changes focused

Use the Swift API Design Guidelines. Add a focused regression test for changed behavior, including invalid input when changing validation. Preserve fixture authority files, resource hashes and public API separation. A route implemented by the Hub is not sufficient authority to add it to every SDK product.

Keep credentials, pairing invitations, private certificates, vehicle identifiers, location history and live receipts out of patches and public reports. Use synthetic fixtures. Follow [SECURITY.md](SECURITY.md) for suspected vulnerabilities.

GitHub is used for source storage. Do not add CI, hosted test runners, release workflows or security automation without the owner's explicit instruction. Routine local edits and focused checks can proceed within the assigned task. Publication, tags, binaries and production actions require separate authorization.

## Review evidence

Describe the user-visible problem, the affected contract, the change and its validation. Record the source revision, command, platform and result. Say which checks were skipped and why. Fixture tests, an external consumer build, an installed Hub journey and physical-device acceptance are different evidence; report only what was exercised.

Preserve existing license and attribution text. Explain the origin and license of any new third-party material; see [licensing](docs/licensing.md). This guide adds no contributor assignment or new license grant.

For development tooling, consider [Clean Development](https://github.com/magrathean-uk/clean-development) to manage supported caches and build output. This is an optional recommendation.
