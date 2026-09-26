# Teslatlas Edge

Teslatlas Edge is an optional, user-operated Fleet Telemetry ingress for a Teslatlas home Hub. A pinned Tesla receiver sidecar sends decoded envelopes to Edge over a loopback bearer-protected endpoint. Edge admits them to an encrypted, bounded spool. The home Hub connects outbound to Edge over mTLS, pulls batches, commits them with deduplication, and acknowledges them. Delivery is at least once.

Edge does not store Tesla account credentials, expose vehicle-command paths, or act as a hosted relay. The receiver, admission, Hub delivery, credential, health, metrics, and spool boundaries are described in [the architecture](docs/architecture.md).

## Current source status

The Cargo package version is `2026.36.2`. [Cargo.toml](Cargo.toml) declares
Rust 1.98 as the minimum compiler version and `AGPL-3.0-only` as the license.
Wire versions 1 and 2 are implemented; the spool format is 3. Read the
[upgrade guide](docs/operations/upgrade-backup-recovery.md) before opening an
older spool with this version.

Historical G5 evidence covers a bounded source-built synthetic journey on
Debian 13.6 ARM64. The 2026-09-22 cleanup record reports removed runtime
artifacts. Neither record establishes today's installation, listener state,
package, service-manager lifecycle, live vehicle or production acceptance.
See [product versioning](docs/product-versioning.md) for the evidence boundary.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for prerequisites, build commands and
focused checks. The bridge check downloads and builds a pinned receiver and
requires Go 1.27.0 exactly. In the maintained workspace, use its existing
execution wrapper and toolchain policy; independent clones need an external
Cargo target directory. Package recipes consume matching target binaries.

## Configuration and operation

The checked-in examples are [packaging/config.toml.example](packaging/config.toml.example), [packaging/docker/config.toml.example](packaging/docker/config.toml.example), and the matching Fleet Telemetry JSON examples. Native service installation, TLS boundaries, credential enrolment, rotation, and local health checks are in [native installation](docs/operations/native-installation.md). Compose mounts and lifecycle commands are in [Docker installation](docs/operations/docker.md). Preserve the spool key and complete spool during backups and upgrades; [upgrade, backup, and recovery](docs/operations/upgrade-backup-recovery.md) documents the format-3 migration and recovery rules.

The public delivery contract is `edge-delivery-v2@2.0.0` in the sibling
`teslatlas-protocol/profiles/edge-delivery-v2/2.0.0` tree. The request and acknowledgement rules are in [the Hub delivery contract](docs/hub-delivery-contract.md). The installed matrix coordinator and source-only handoff helpers are documented in [tools/interop/client_lanes](tools/interop/client_lanes/README.md); the coordinator can drive lifecycle operations through the shared runner,
while the handoff helper only prepares validated inputs. Their presence does
not demonstrate an installed matrix result.

## Scope and boundaries

Included: the Tesla receiver sidecar integration, loopback durable admission, encrypted bounded spool, Hub pull and acknowledgement API, credential lifecycle, health, readiness, and aggregate metrics.

Excluded: Tesla account tokens, vehicle commands, consumer APIs, a mandatory hosted relay, managed VPS deployment, and hosted GitHub automation. Keep receiver admission and health endpoints on loopback. Restrict the Hub mTLS listener to the intended private address or tunnel, and forward receiver traffic as raw TCP when a router is used.

## Contributing and support

- [Contributing](CONTRIBUTING.md): requirements, checks and review expectations.
- [Troubleshooting](SUPPORT.md): operational guides and safe report contents.
- [Security](SECURITY.md): trust boundaries and private reporting limitations.
- [Agent guidance](AGENTS.md): project-specific working rules.

GitHub is source storage for this project. No hosted CI, build/test automation
or release publishing is part of the development workflow.

## License

The main project declares [AGPL-3.0-only](LICENSE). The optional Tesla receiver
sidecar retains its upstream Apache-2.0 terms and modification notices. See
[licensing](docs/licensing.md), [third-party notices](docs/legal/third-party-notices.md)
and the bundled [Apache License 2.0](docs/legal/Apache-2.0-fleet-telemetry.txt).
