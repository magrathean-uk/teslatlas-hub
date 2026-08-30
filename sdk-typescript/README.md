# Teslatlas TypeScript SDK

Private development foundation for the planned public Teslatlas browser and Node.js SDK.

## Status

Transport foundation under development. It provides locked tooling, protocol-rule
primitives, browser and Node.js fetch adapters, conditional HTTP helpers, caller-owned
credential interfaces, and raw SSE replay/reconnect mechanics.

The pinned protocol authority revision does not publish OpenAPI, JSON Schema, event,
authentication, error, command-job, or conformance artifacts. This package therefore has
no resource methods or generated payload types and is deliberately private. See the
[protocol dependency gate](docs/protocol-dependency-gate.md).

## Run the SDK locally

```bash
npm ci
npx playwright install chromium
npm run verify
```

The repository pins Node.js `26.7.0`, npm `11.19.0`, and every development dependency.

## Try the Node.js example

```bash
npm run example:node
```

Expected output:

```text
Teslatlas SDK transport example: 304 "fixture-2"
```

The example uses a deterministic injected fetch response. It does not rely on an
unreleased Hub route.

## Try the browser example

```bash
npm run example:browser
```

Open `http://127.0.0.1:4173`. This also uses a deterministic local response.

## Use the current foundation

```typescript
import { asEntityTag, createNodeTransport } from "@teslatlas/sdk/node";

const transport = createNodeTransport({
  baseUrl: "https://hub.example",
  authorization: async () => credentialStore.load(),
});

const response = await transport.request(protocolGeneratedPath, {
  ifNoneMatch: asEntityTag(previousEntityTag),
});
```

`protocolGeneratedPath` and `credentialStore` must come from the caller. The SDK does not
ship candidate routes or choose credential persistence.

## Read next

- [Architecture](docs/architecture.md)
- [API reference](docs/api.md)
- [Protocol dependency gate](docs/protocol-dependency-gate.md)

## Verification

```bash
npm run format:check
npm run lint
npm run typecheck
npm run build
npm run test:unit
npm run test:browser
npm run test:conformance
npm run pack:check
```

The shared conformance fixtures cover this SDK transport foundation in Node.js and real
Chromium. Unit verification also loads the built browser example and runtime-imports every
package entry point. These are not Teslatlas protocol conformance fixtures.

## Licence

Apache-2.0.
