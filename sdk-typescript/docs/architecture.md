# TypeScript SDK architecture

## Responsibility

Provide the contract-neutral browser and Node.js foundation for future released Teslatlas
protocol clients without embedding Hub implementation or viewer product logic.

## Package boundaries

| Unit | Responsibility |
| --- | --- |
| Core | Safe errors, opaque capabilities, version checks |
| HTTP | Fetch transport, opaque query values, conditional headers |
| Events | SSE parsing, reconnect, Last-Event-ID persistence hooks |
| Auth | Generic caller-owned credential interfaces |
| Node | Node-named factory over the shared fetch transport |
| Browser | Browser-named factory over the shared fetch transport |

The current implementation provides the contract-neutral part of these units. Resource
clients and payload decoders remain generated-layer work because the protocol authority
has not released its schemas.

## Runtime flow

1. A caller creates the browser or Node.js adapter with a base URL and optional
   authorization provider.
2. A future generated protocol layer supplies approved relative paths, query names, and
   payload decoders.
3. The shared fetch transport resolves only root-relative targets, loads authorization for
   that request, applies conditional headers, and makes one fetch attempt.
4. Ordinary responses remain native `Response` objects for the generated decoder.
5. SSE subscriptions parse raw standard events, checkpoint only after consumer progress,
   and reconnect only when the caller policy authorizes it.

Browser and Node.js entry points export the same core. Their only difference is the named
adapter factory, and both default to their runtime's `globalThis.fetch`.

## Security rules

- Consumers supply credential storage; the SDK never invents browser persistence policy.
- The low-level transport accepts a caller-supplied authorization value but defines no
  credential format, scope, or endpoint policy.
- Errors preserve only validated safe request IDs; arbitrary underlying causes are not
  retained.
- The command safety guard requires an idempotency key and retry mode `never`; submission
  and polling wait for the released command-job contract.

## Boundaries

The SDK is transport and contract code. It does not own access-control policy, operator configuration, dashboard UI, or Hub data projection.

The low-level transport is infrastructure for released protocol clients. It does not
declare candidate product-brief routes public and must not be used to institutionalise a
Hub-private route.

## Dependency boundary

`teslatlas-protocol` is the only authority for discovery, routes, resource fields, event
payloads, authentication, error codes, and command jobs. See
[Protocol dependency gate](protocol-dependency-gate.md) for the precise blocked surface.
