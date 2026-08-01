---
type: wayfinder:task
status: closed
parent: 000-map
---
# Freeze the core data model

Blocked by: [Define time, units, and numeric precision](013-define-time-unit-precision.md).

## Question

What canonical entities and relationships represent cars, states, drives,
positions, charging processes, charge samples, updates, addresses, and settings?

## Starting recommendation

Model TeslaMate domain relationships explicitly while separating immutable
observations from repairable projections.

## Resolution

The canonical model is Source -> Vehicle, with immutable Observation facts and
replaceable VehicleLifecycleState. TeslaMate capture models Car, StateInterval,
Drive -> Position, ChargingProcess -> ChargeSample, SoftwareUpdate, Address,
Geofence, and effective settings outcomes. Source relationships retain IDs;
the phone contract flattens address/geofence names only for completed projected
history. Open sessions are not fabricated as complete. TeslaMate settings
tables and control surfaces are not copied; their applicable outcomes are
separate versioned values.
