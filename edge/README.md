# Teslatlas Edge

Optional user-operated Tesla Fleet Telemetry ingress for a Teslatlas home Hub.

The current release-cohort product version and its compatibility status are
described in [product versioning](docs/product-versioning.md).

> **Beta:** Wire version 1 remains compatible. The current on-disk v3 spool
> format is a guarded forward-only upgrade from v2. Wire version 2 adds stable
> retry identity, monotonic spool sequence, and durable loss notices. A v2
> spool with acknowledgement receipts cannot be migrated automatically because
> those receipts do not identify the original admission; Edge refuses it while
> preserving the spool for lineage reconciliation. A receipt-free v2 spool can
> migrate, but its old v1 receipt history is treated as unknown and v1 ACKs
> require the v2 endpoint after migration. G5 accepted one bounded product
> `2026.36.2` source-built synthetic Edge-to-Hub journey on Debian 13.6 ARM64:
> one guarded
> record survived Hub outage and Edge restart, retained stable retry identity,
> committed before occurrence-bound ACK, projected once, and remained durable
> through Hub restart. This is not installed/package/service-manager or live
> vehicle proof. Edge is not a hosted relay and stores no Tesla account or
> vehicle-command credentials.

V1 acknowledgement replay is bounded by the 1,024 retained receipt files. If
v1 receipt history is pruned, Edge fails subsequent v1 acknowledgements closed
and requires the v2 endpoint for sequence-bound receipt identity.

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
- Public contract: `edge-delivery-v2@2.0.0` under the sibling
  `teslatlas-protocol/profiles/edge-delivery-v2/2.0.0` repository path
- [Installed matrix coordinator and Edge contract](tools/interop/client_lanes/README.md)
- [Native installation](docs/operations/native-installation.md)
- [Docker installation](docs/operations/docker.md)
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
