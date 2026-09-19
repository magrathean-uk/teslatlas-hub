# Product versioning

The Python project, `uv.lock`, and Home Assistant integration manifest carry
the shared ecosystem product version `2026.36.2`. The Home Assistant manifest
is the runtime-visible integration version.

This calendar product number does not replace protocol identities or prove a
working Hub connection in general. `compatibility/hub.json` accepts
`hub-http-v1@1.0.0` only with embedded profile hash
`b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`, the exact
tested Hub source fingerprint and the named Debian 13 ARM64 Container receipts.
That source-built synthetic lane does not widen support to HA OS, HACS,
production, real data, replacement upgrades or an untested platform.
