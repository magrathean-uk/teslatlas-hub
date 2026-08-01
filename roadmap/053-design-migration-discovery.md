---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design migration discovery

Blocked by: [Design source provenance and switching](052-design-source-provenance-switching.md).

## Question

How does one migration command discover TeslaMate deployment shape, service
manager, database endpoint, version, vehicles, credentials, and writable target?

## Starting recommendation

Use only read-only database queries, file reads, process inspection, service
inspection, and container inspection. Print exact detected scope. Never use
`docker exec`, create helper containers, alter configuration, or perform any
TeslaMate service action.

## Resolution

Discovery is a bounded, read-only preflight with a redacted machine-readable
scope report. It proves source shape, schema, candidate vehicles, credential
availability, and Hub target readiness, then stops; ambiguity or incompatibility
fails closed without touching TeslaMate.
