---
type: wayfinder:task
status: closed
parent: 000-map
---
# Run metadata COPY lanes in parallel

Blocked by: [Set bounded parallel lane contract](100-set-bounded-parallel-lane-contract.md).

## Question

Can independent address, geofence, and update streams run concurrently against
one exported source snapshot while preserving the global row ceiling?

## Starting recommendation

Open one attached read-only lane per independent table, finish every lane
before using its values, then add their counts before the main pack lane runs.

## Resolution

Address, geofence, and update COPY streams now run concurrently on their own
attached read-only lanes when the configured bound permits three lanes. Lower
bounds run those same lanes serially. Their combined rows are checked before
the main pack lane begins.
