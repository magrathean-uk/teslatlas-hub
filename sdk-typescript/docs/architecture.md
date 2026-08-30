# TypeScript SDK architecture

## Responsibility

Expose released Teslatlas protocol capabilities to browsers and Node.js without embedding Hub implementation or viewer product logic.

## Package boundaries

| Unit | Responsibility |
| --- | --- |
| Core | Protocol types, errors, capabilities, version checks |
| HTTP | Query resources, cursors, conditional requests |
| Events | SSE parsing, reconnect, Last-Event-ID persistence hooks |
| Auth | Explicit caller-owned paired-device credential interfaces |
| Node | Node-specific transport and stream adapters |
| Browser | Browser-safe transport with no embedded secrets |

## Security rules

- Consumers supply credential storage; the SDK never invents browser persistence policy.
- Public clients use paired-device scopes and public endpoints only.
- Errors preserve safe server diagnostics such as request IDs, never credentials.
- Command APIs remain explicit and do not retry non-idempotent actions automatically.

## Boundaries

The SDK is transport and contract code. It does not own access-control policy, operator configuration, dashboard UI, or Hub data projection.
