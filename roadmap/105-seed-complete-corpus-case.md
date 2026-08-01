---
type: wayfinder:task
status: closed
parent: 000-map
---
# Seed complete corpus case

Blocked by: [Restore validated corpus schema](104-restore-validated-corpus-schema.md).

## Question

What smallest source history exercises every direct-migration relation while
remaining semantically complete for a selected car?

## Starting recommendation

Add one deterministic selected-car case with a drive, positions, charge,
metadata, update, and state evidence.

## Resolution

`complete-selected-car.sql` supplies one finished selected-car drive, three
positions, one finished charge with a sample, linked addresses/geofence, a
state interval, and a completed software update. It leaves schema foreign-key
free so separate negative fixtures can still express source corruption.

Native PostgreSQL restore confirmed selected-car relation counts of
`1:1:3:1:1:2:1:1:1` in reviewed table order.
