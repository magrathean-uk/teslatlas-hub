# Public protocol binding

The production adapter is bound to `hub-http-v1@1.0.0`. Its checked profile
bundle is embedded in the integration and identified by SHA-256
`b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`. This is
the canonical current-Hub working-tree profile selected by Protocol and
contains the bounded 4096-byte claim request/error contract. The TypeScript SDK
vendored snapshot now matches this canonical profile and its current validator
bindings pass their source gate; the sibling status is recorded in the
[HA-to-Hub admission handoff](hub-admission-handoff-2026-09-08.json).

This profile supports public discovery, invitation claim, credential rotation,
vehicles, current state, and bounded drive queries. Home Assistant currently
uses discovery, claim, rotation, vehicles, and current state. It deliberately
does not invent SSE, command, charge, collector-health, cost, backup, or data
quality routes.

The integration remains unpublished. `compatibility/hub.json` is accepted only
for the exact product `2026.36.2`, profile hash, Hub source fingerprint and
Debian 13 ARM64 Container receipt named there. This bounded source-built
synthetic result does not establish HACS, HA OS, production, real-data,
replacement-upgrade or full-platform acceptance.
