# Teslatlas Edge

Optional user-operated Tesla Fleet Telemetry ingress for a Teslatlas home Hub.

> **Beta:** Wire version 1 remains compatible. The on-disk v2 spool format is a
> forward-only upgrade. Wire version 2 adds stable retry identity, monotonic
> spool sequence, and durable loss notices. Hub-side v2 integration and live
> vehicle proof are separate work. Edge is not a hosted relay and stores no
> Tesla account or vehicle-command credentials.

## Build and inspect

```bash
cargo build --locked --release
cargo run -- --help
scripts/test-fleet-telemetry-bridge.sh
```

The service accepts decoded receiver envelopes on loopback, durably writes an
encrypted bounded spool, and exposes batches to a home Hub over mTLS plus a
rotating bearer. Delivery is at-least-once. The Hub must deduplicate and commit
records and gap evidence before acknowledging them.

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
