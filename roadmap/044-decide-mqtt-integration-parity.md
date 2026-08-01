---
type: wayfinder:task
status: closed
parent: 000-map
---
# Decide MQTT and integration parity

Blocked by: [Decide external import parity](043-decide-external-import-parity.md).

## Question

Which TeslaMate MQTT topics, retain/QoS behavior, availability, Home Assistant
discovery, and event timing must Hub reproduce?

## Starting recommendation

Implement an optional compatibility publisher only after topic fixtures and
ordering tests define consumers’ actual contract.

## Resolution

MQTT and Home Assistant are excluded from this release. Hub has no broker
connection, MQTT credential, listener, discovery, or availability contract.
A future isolated publisher needs TeslaMate topic, encoding, retain/QoS,
ordering, restart, and failure fixtures before it can emit any compatibility
traffic.
