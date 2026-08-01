---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design backup and restore

Blocked by: [Design integrity checking and repair](022-design-integrity-repair.md).

## Question

What constitutes a complete, consistent, encrypted, versioned backup, and how
is restore proven on another host?

## Starting recommendation

Back up database, identities, encrypted credentials, and required metadata as
one declared generation, then verify restore through a clean-host drill.

## Resolution

One versioned generation contains a consistent SQLite catalogue image, every
referenced pack, sealed migration evidence, hashes, schema/protocol versions,
Hub revision, installation identity, time, and recovery floor. Live SQLite uses
backup/checkpoint-safe capture, never blind WAL copying. Systemd host-encrypted
blobs are nonportable: cross-host recovery needs separately escrowed,
recipient-encrypted Hub identity/key material or explicit reprovisioning;
TeslaMate credentials never transfer. Restore verifies into fresh private space,
checks integrity/schema, atomically selects the generation, then proves doctor,
manifest, and paired sync on a clean host. The drill remains a release gate.
