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

The Node current-Hub adapter keeps ordinary CA and hostname verification and,
on the same new TLS connection, verifies the invitation `tlsPin` against the
SHA-256 of the connected leaf certificate's DER bytes before claim bytes are
sent. Pairing expects the operator to obtain an invitation from Hub's
`pair --json` command and to learn the expected Hub UUID through trusted setup
context; the invitation itself does not contain that UUID.

Browser Fetch does not expose the peer certificate, so the browser adapter
fails `claimPairing` with `HubTlsPinUnavailableError` before discovery or claim
network I/O unless the caller supplies a pin-capable `claimTransport` (for
example, a trusted native bridge). Browser reads and rotation continue to use
normal Web PKI and caller-owned credentials; the SDK never disables TLS checks.

## Current-Hub consumer

Build a candidate tarball, then install that file into the source-distributed
minimal consumer outside this repository:

```bash
npm run build
mkdir -p /tmp/teslatlas-sdk /tmp/teslatlas-hub-consumer
npm pack --pack-destination /tmp/teslatlas-sdk
cp examples/hub/{package.json,node.mjs,index.html,app.js,serve.mjs} /tmp/teslatlas-hub-consumer/
npm --prefix /tmp/teslatlas-hub-consumer install --no-save --package-lock=false --ignore-scripts \
  /tmp/teslatlas-sdk/teslatlas-sdk-2026.36.2.tgz
```

Run the Node consumer with an operator-confirmed endpoint and Hub UUID. Keep
the invitation or credential envelope in a private file; no token is accepted
on the command line:

```bash
node /tmp/teslatlas-hub-consumer/node.mjs \
  --endpoint "$HUB_ENDPOINT" --hub-id "$HUB_ID" \
  --invitation-file "$INVITATION_FILE"
```

The browser consumer is a static example served from loopback. Provision its
endpoint- and Hub-bound credential envelope with the pin-capable Node path or a
trusted native bridge; do not paste an invitation into browser JavaScript:

```bash
npm --prefix /tmp/teslatlas-hub-consumer run browser
```

Hub must allow the exact page origin, and the browser must trust the endpoint
certificate. Browser Fetch can expose a generic CORS or TLS error when either
condition fails. The example verifies the credential envelope against the
entered endpoint and Hub UUID, clears the input after ingestion, keeps the
credential in memory, awaits logout before disposal, and requires a newly
provisioned credential after a 401. It does not poll or scan for Hub instances. See `examples/hub/`
for the five source files and the compatibility guide for the complete API
contract.

## Local verification

```bash
npm ci
npx playwright install chromium
npm run verify
```

The reproducibility toolchain is exactly Node.js `26.7.0` and npm `11.19.0`.
Current runtime evidence covers that Node version and Chrome 153 on macOS 27;
this package does not yet claim a broader accepted runtime or browser floor.

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
- [Docker setup](docs/docker.md)
- [Protocol dependency gate](docs/protocol-dependency-gate.md)

## Licence

Apache-2.0.
