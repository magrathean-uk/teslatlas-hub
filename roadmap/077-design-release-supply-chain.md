---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design release and supply-chain trust

Blocked by: [Design the platform and install matrix](076-design-platform-install-matrix.md).

## Question

How are dependencies, vendored SQLite, builds, provenance, signatures,
checksums, trust roots, release artifacts, and emergency revocation controlled?

## Starting recommendation

Produce reproducible or independently repeatable native artifacts, detached
signatures, pinned trust roots, and an offline-verifiable release manifest.

## Resolution

Release trust uses locked dependencies, verified vendored SQLite, recorded
native build provenance, repeatable artifacts, detached manifest signatures,
and independently pinned Minisign keys. Signed revocation and root rotation
fail closed; GitHub is storage only and no release action touches TeslaMate.
