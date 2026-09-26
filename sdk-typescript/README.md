# Teslatlas TypeScript SDK

The Teslatlas TypeScript SDK is a private, protocol-derived client for Node.js and browser applications. It exposes the released Teslatlas operations through typed methods and validates discovery responses, protocol data, errors, metadata, command jobs, and server-sent events before returning them to the caller.

The package is private and is not a registry release. The package manifest identifies version `2026.36.2`, Apache-2.0 licensing, and an evidence-only runtime policy. Local protocol fixtures and consumer checks demonstrate SDK behavior against checked-in inputs. They do not establish the behavior of a remote Hub or a production deployment.

## Entry points

Use a runtime-specific entry point to create a client:

```ts
import { createClient } from "@teslatlas/sdk/node";

const client = await createClient({
  baseUrl: "https://hub.example.invalid",
  authorization: () => undefined, // Supply a complete authorization value for protected reads.
});

const vehicles = await client.listVehicles();
if (vehicles.kind === "modified") {
  console.log(vehicles.value.items);
}
```

Import `createClient` from `@teslatlas/sdk/node` or `@teslatlas/sdk/browser`. The root `@teslatlas/sdk` entry point exports public types, errors, opaque value constructors, and caller-owned interfaces. It does not expose an arbitrary route executor or a runtime factory.

The example URL is a placeholder. The authorization provider is caller-owned; protected reads require a valid credential.

The public client has 20 named operations covering discovery, vehicles, current state, drives, positions, charges, samples, states, updates, events, data quality, commands, and metadata. The [API reference](docs/api.md) lists the entry points and methods. The [compatibility guide](docs/compatibility.md) describes result, error, ETag, pagination, command, and event replay behavior.

## Current Hub adapter

The browser and Node entry points also export `createHubClient` for the locked `hub-http-v1@1.0.0` profile. It uses a fixed HTTPS endpoint, an expected Hub UUID, and a caller-owned credential store. Its operations cover discovery, health, readiness, pairing, credential rotation, vehicles, current state, and drives.

The Node adapter keeps normal CA and hostname verification. During pairing it checks the invitation `tlsPin` against the SHA-256 digest of the connected leaf certificate DER bytes on the same new TLS connection before sending claim bytes. Browser Fetch cannot inspect the connected certificate, so browser pairing fails with `hub_tls_pin_unavailable` before discovery or claim network I/O unless the embedding supplies a pin-capable claim transport. Browser reads and rotation continue to use normal Web PKI. The SDK never disables certificate verification.

Credentials and event checkpoints remain caller-owned. Do not put authorization values or invitations in source files, command arguments, browser code, or published logs. See the [architecture guide](docs/architecture.md) for lifecycle and boundary details.

## Local development

Use the Node `26.7.0` and npm `11.19.0` toolchain named by `package.json`. The manifest declares no broader runtime floor because the current evidence policy does not accept one.

Follow `CONTRIBUTING.md` in the [source repository](https://github.com/magrathean-uk/teslatlas-sdk-typescript) and applicable workspace instructions before running package commands. A build replaces the checkout's `dist/` directory. With the pinned toolchain, the local verification sequence is:

```bash
npm ci
npm run build
npm run verify
```

Browser conformance needs the Playwright Chromium runtime to be available. Dependency installation alone does not establish that prerequisite.

The `verify` script runs formatting, linting, type checking, protocol checks, the build, unit and conformance suites, protocol cases, the Node example, package checks, and the clean-package test. Targeted commands are also available:

```bash
npm run test:unit
npm run test:conformance
npm run test:protocol
npm run typecheck
npm run protocol:check
npm run pack:check
```

The examples use local fixture responses:

```bash
npm run example:node
npm run example:browser
```

The current-Hub Node and browser examples require an operator-provisioned external fixture and caller-owned credentials. Their source files are under `examples/hub/`; the compatibility and package lifecycle guides describe the contracts without embedding credentials or private runtime paths.

## Further reading

- [API reference](docs/api.md)
- [Architecture](docs/architecture.md)
- [Compatibility](docs/compatibility.md)
- [Product versioning](docs/product-versioning.md)
- [Protocol dependency gate](docs/protocol-dependency-gate.md)
- [Docker package gate](docs/docker.md)
- [Package lifecycle](docs/package-lifecycle.md)
- [Apache-2.0 licence](LICENSE)

Repository-only guides are available in the [source repository](https://github.com/magrathean-uk/teslatlas-sdk-typescript): `CONTRIBUTING.md`, `SUPPORT.md`, `SECURITY.md`, and `docs/licensing.md`. They are not bundled in the npm archive.

## Licence

Apache-2.0. See [LICENSE](LICENSE) for the complete licence text.
