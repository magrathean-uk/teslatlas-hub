# Security model

## Protected assets

- Hub's own Tesla access and refresh tokens;
- the window issuer key and its ledger;
- Fleet virtual-key and command-proxy material;
- device pairing bearers and cursor-signing keys;
- exact vehicle identity, location, and travel history; and
- backup and recovery keys.

## Trust boundaries

| Boundary | Control |
|---|---|
| Local plaintext HTTP | Loopback only. Protected routes require the private local bearer and one matching `Host` header. Health, readiness, discovery, and Fleet ingress have explicit route handling; Fleet ingress has its own bearer check. |
| Remote sync | TLS plus paired-device bearer authentication. |
| Pairing | Short-lived, single-use invitation; claim is the only unauthenticated mutation. |
| Fleet Telemetry ingress | Private bearer on supervised loopback route. |
| Tesla credentials | Hub uses only the login it created itself through its own authorisation-code exchange. It never reads, stores, loads or refreshes a token held by another application. Only the resident service, inside an open collection window, sends Tesla traffic and refreshes that login; one refresh runs at a time. |
| Collection window | A grant signed by the Mac's window issuer, naming this host, approved by the owner and ending at a hard expiry. Outside a window Hub sends nothing to Tesla and keeps history current by read-only import. See [Collection windows](../operations/collection-windows.md). |
| history import | Read-only PostgreSQL transaction and exact schema admission. |
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
- fake/development sources are unavailable in release operation; and
- migration and schema admission fail closed.

## Out of scope

This security model does not authorise testing Tesla, third-party source deployments, Apple, GitHub,
another vehicle, or another person's system.

Report product vulnerabilities privately under the [security policy](../../.github/SECURITY.md).
