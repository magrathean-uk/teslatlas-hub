---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design the security and privacy model

Blocked by: [Build the representative database corpus](072-build-representative-database-corpus.md).

## Question

What threats cover credentials, vehicle location, local privilege, network
clients, backups, logs, packages, migration, plugins, and physical disk access?

## Starting recommendation

Document trust boundaries, least privilege, encryption limits, redaction,
rotation, revocation, and explicit non-goals with adversarial tests.

## Resolution

The security/privacy model defines protected assets, local/network/source and
physical-disk boundaries, least privilege, redaction, separate credential and
device custody, adversarial proof, and explicit limits. TeslaMate stays
read-only; raw telemetry needs operator-controlled disk protection.
