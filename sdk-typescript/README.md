# Teslatlas TypeScript SDK

Private, protocol-derived Teslatlas client for browser and Node.js development.

## Status

The SDK implements the 20 named operations in the pinned Teslatlas protocol
snapshot. It validates discovery, responses, errors, metadata, command jobs,
and typed event streams before returning protocol data.

The package is private. Local protocol-case checks exercise SDK behavior against
vendored fixtures; they do not establish the behavior of a remote deployment.

## Use a runtime factory

The root entry point exports public types, errors, and opaque constructors. Use
a runtime-specific factory to create a client:

```ts
import { createClient } from "@teslatlas/sdk/node";

const client = await createClient({
  baseUrl: "https://hub.example.invalid",
  authorization: async () => loadCallerCredential(),
});

const result = await client.listVehicles();
if (result.kind === "modified") {
  console.log(result.value.items);
}
```

Callers provide complete authorization values and own any credential or event
checkpoint persistence. See the [API reference](docs/api.md) and
[compatibility guide](docs/compatibility.md) for the result, error, ETag, and
replay contracts.

For the frozen current Hub API, import `createHubClient` from the same browser
or Node subpath. Supply the exact Hub endpoint, expected Hub UUID, and a
caller-owned credential store. This dedicated adapter preserves the richer
`createClient` API and exposes discovery, health/readiness, pairing, credential
rotation, vehicles, current state, and drives.

## Local verification

```bash
npm ci
npx playwright install chromium
npm run verify
```

The locked toolchain uses Node.js `26.7.0` and npm `11.19.0`.

## Examples

```bash
npm run example:node
```

Expected output:

```text
Teslatlas SDK Node client: 1 vehicle, protocol 1.2.0
```

```bash
npm run example:browser
```

The browser example runs only local fixture responses and renders:

```text
Teslatlas SDK browser client: 1 vehicle, protocol 1.2.0
```

## Read next

- [Product versioning](docs/product-versioning.md)
- [Architecture](docs/architecture.md)
- [API reference](docs/api.md)
- [Compatibility](docs/compatibility.md)
- [Protocol dependency gate](docs/protocol-dependency-gate.md)

## Licence

Apache-2.0.
