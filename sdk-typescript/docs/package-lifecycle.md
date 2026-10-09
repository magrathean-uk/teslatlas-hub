# Package lifecycle and final consumer gates

The package lifecycle gate and the final Hub consumer gate were retired on 2026-10-08
in plan group G2-2, together with the Docker lane, the package-provenance scripts, the
pinned records under `tools/`, and the examples. The package is private and not
published in v7, so there is no install, update, rollback or removal lifecycle to gate.

What remains is a local check of the packed archive: `npm run pack:check` verifies the
archive contents and `npm run test:package` builds clean and stale trees, compares the
two archives byte for byte, and installs, reinstalls and removes the archive in a fresh
consumer project.

Receipts that the retired gates produced stay under [`development/`](development/).
They record what an earlier source revision did and are not evidence for the current
one.
