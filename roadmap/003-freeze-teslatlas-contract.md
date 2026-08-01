---
type: wayfinder:task
status: closed
parent: 000-map
---
# Freeze the current Teslatlas contract

Blocked by: [Define backend parity](002-define-backend-parity.md).

## Question

What exact cars, drives, positions, charges, charge samples, metadata, ordering,
identity, and refresh behavior does Teslatlas consume today?

## Starting recommendation

Extract the contract from production Hub and Teslatlas code into golden schemas
and fixtures before expanding the backend.

## Resolution

The current Teslatlas contract is `teslatlas-sync` protocol v1 with projection
schema v2.0 (`THP1` SQLite application ID), delivered as signed immutable
`sqlite-zstd` full snapshots only. Delta transfer, background refresh, and a
generic remote-SQLite interface are not current contract surface.

One manifest binds one installation, account, Hub vehicle, generation, snapshot,
and selected local car ID. It contains one or more ordinal contiguous chunks;
each chunk is signed as part of the exact manifest bytes and has matching
snapshot/schema/sequence bindings and byte and row totals. The receiver accepts
only a complete verified receipt set, stages it in a fresh local SQLite file,
then atomically replaces its previous mirror.

The typed SQLite payload has strict `cars`, `drives`, `positions`, `charges`,
and `charge_samples` tables. It preserves positive source integer identities:
one selected `cars` row; `drives.car_id` and `charges.car_id` reference it;
`positions.drive_id` references a completed drive; and
`charge_samples.charge_process_id` references a charge. Writer order is numeric
ID order, while consumer-visible ordering is by its explicit local query. All
timestamps are integer milliseconds. A complete drive and its attached
positions are included; an open drive and unattached positions are not. Charges
may be open and charge samples retain their parent relationship.

Teslatlas currently consumes the full fields present in those five typed tables:
car identity and metadata; drive interval, distance, efficiency, endpoint,
location, SOC, and range facts; position time, coordinates, movement, battery,
climate, and range facts; charge interval, energy, SOC, location, charger, and
range facts; and charge-sample time, battery, charger, thermal, fast-charger,
and cable facts. Refresh is an owner-initiated foreground full-snapshot
replacement. The strict protocol decoder, schema writer, and TLS import E2E
fixture are the golden contract checks until later compatibility and delta
tickets deliberately version them.
