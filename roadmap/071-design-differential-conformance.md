---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design differential conformance

Blocked by: [Design fake Tesla and fault injection](070-design-fake-tesla-fault-injection.md).

## Question

How will identical source scenarios be run through TeslaMate and Hub and
compared across lifecycle, database, calculations, MQTT, and recovery behavior?

## Starting recommendation

Build a reference harness that normalizes allowed implementation differences
and fails every unexplained semantic difference.

## Resolution

Run identical virtual-clock fixtures through Hub and a disposable pinned
TeslaMate reference runner, then compare normalized Teslatlas-visible fact,
calculation, request, and recovery graphs. Only named contract conversions or
approved deviations may differ; MQTT and web integrations are negative scope
assertions for Hub.
