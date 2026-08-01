---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design resource-pressure controls

Blocked by: [Design the adaptive runtime profile](068-design-adaptive-runtime-profile.md).

## Question

How does Hub react to low disk, memory pressure, CPU saturation, slow storage,
database contention, oversized history, and network backlog?

## Starting recommendation

Apply bounded backpressure, preserve collection durability first, pause optional
projection work, and fail migrations before recovery space is consumed.

## Resolution

Profile-bounded detectors move work through constrained and critical states.
They backpressure or stop new work, preserve durable collection facts, defer
optional work, and fail/discard incomplete Hub migration stages before reserve
loss. Recovery requires fresh measurement and validation; it never resumes an
old source snapshot or weakens safety.
