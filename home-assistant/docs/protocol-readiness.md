# Public protocol binding

The production adapter is bound to `hub-http-v1@1.0.0`. Its checked profile
bundle is embedded in the integration and identified by SHA-256
`b3914d35d28374f6423af789e9ed6a4a4c82196a068c041946e24d609db0b05b`.

This profile supports public discovery, invitation claim, credential rotation,
vehicles, current state, and bounded drive queries. Home Assistant currently
uses discovery, claim, rotation, vehicles, and current state. It deliberately
does not invent SSE, command, charge, collector-health, cost, backup, or data
quality routes.

The candidate remains unpublished. Local unit and integration tests do not
promote the compatibility manifest. The ecosystem acceptance matrix must bind
the exact integration artifact to the final macOS and Linux Hub targets before
release.
