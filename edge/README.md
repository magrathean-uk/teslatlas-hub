# Teslatlas Edge

Optional user-operated Tesla Fleet Telemetry ingress for a Teslatlas home Hub.

> **Beta:** Wire version 1 is implemented, but Edge and Hub must be upgraded
> together for any future incompatible contract change. Edge is not a hosted
> Magrathean relay and stores no Tesla account or vehicle-command credentials.

## Build and inspect

```bash
cargo build --locked --release
cargo run -- --help
scripts/test-fleet-telemetry-bridge.sh
```

The service accepts decoded receiver envelopes on loopback, durably writes an
encrypted bounded spool, and exposes batches to a home Hub over mTLS plus a
scoped bearer. Delivery is at-least-once. The Hub must deduplicate and commit
before acknowledging records.

## Documentation

- [Architecture](docs/architecture.md)
- [Hub delivery contract](docs/hub-delivery-contract.md)
- [Native installation](docs/operations/native-installation.md)
- [Upgrade, backup, and recovery](docs/operations/upgrade-backup-recovery.md)
- [Third-party notices](docs/legal/third-party-notices.md)

## Scope

Included: Tesla receiver sidecar, loopback durable admission, encrypted spool,
Hub pull/ack API, credential lifecycle, health, and aggregate metrics.

Excluded: Tesla account tokens, vehicle commands, consumer APIs, mandatory
hosted relays, managed VPS deployment, and hosted GitHub automation.

## Licence

AGPL-3.0-only. The optional Tesla receiver sidecar retains its upstream
Apache-2.0 terms and modification notices.
