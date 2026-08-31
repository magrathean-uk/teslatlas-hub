# Security model

## Protected assets

- provider access and refresh tokens;
- Fleet virtual-key and command-proxy material;
- device pairing bearers and cursor-signing keys;
- exact vehicle identity, location, and travel history;
- backup and recovery keys.

## Trust boundaries

| Boundary | Control |
|---|---|
| Local plaintext HTTP | Loopback only; local processes are trusted. |
| Remote sync | TLS plus paired-device bearer authentication. |
| Pairing | Short-lived, single-use invitation; claim is the only unauthenticated mutation. |
| Fleet Telemetry ingress | Private bearer on supervised loopback route. |
| Provider credentials | Resident collector is the sole refresh owner. |
| TeslaMate migration | Read-only PostgreSQL transaction and exact schema admission. |
| Vehicle command | Explicit action, confirmation, selected vehicle, bounded proxy. |
| Source release | Immutable annotated tag, exact version, clean tree, provenance classification, and licence material. |

## Defensive defaults

- configuration rejects unknown fields and unsafe paths;
- non-loopback bind requires TLS;
- ordinary request bodies, concurrency, handlers, pack streams, logs, caches,
  retries, and shutdown are bounded;
- credential-like provider fields are stripped before retention;
- secrets enter through protected files or bounded standard input, not argv;
- log display, copy, and save redact credentials and identifying telemetry;
- service units use dedicated accounts and operating-system sandboxing;
- fake/development sources are unavailable in release operation;
- migration and schema admission fail closed.

## Out of scope

This security model does not authorize testing Tesla, TeslaMate, Apple, GitHub,
another vehicle, or another person's system.

Report product vulnerabilities privately under the [security policy](../../.github/SECURITY.md).
