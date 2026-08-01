---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design dual-run safety

Blocked by: [Design the wake and live-data probe](062-design-wake-live-probe.md).

## Question

Can TeslaMate and Hub collect simultaneously during verification, and how are
rate limits, sleep behavior, token rotation, and data divergence controlled?

## Starting recommendation

Keep any overlap short and bounded to the one-minute proof. If continuous
dual-collection cannot be proven safe, report that operator cutover is required;
automation still must not pause, stop, or reconfigure TeslaMate.

## Resolution

Continuous dual collection is forbidden. The only overlap is the bounded,
one-minute owner-authorized probe: one lease, discovery, and at most one
online-only vehicle-data read, with durable rate, sleep, credential, and
divergence evidence. Any continuing Hub collection needs owner-controlled
cutover; Hub never mutates TeslaMate to obtain it.
