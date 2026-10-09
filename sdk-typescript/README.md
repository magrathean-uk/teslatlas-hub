<p align="center">
  <img src="https://raw.githubusercontent.com/magrathean-uk/magrathean-uk/main/assets/icons/teslatlas.png" width="96" height="96" alt="">
</p>

<h1 align="center">Teslatlas TypeScript SDK</h1>

<p align="center">A private, protocol-derived Hub client for Node.js.</p>

<p align="center">
  <a href="docs/architecture.md">Documentation</a> ·
  <a href="LICENSE">Licence</a>
</p>

## Overview

The Teslatlas TypeScript SDK is a private, protocol-derived client for the Teslatlas Hub read API on Node.js. It talks to one fixed Hub endpoint through typed methods and validates every JSON discovery, resource, pairing, and error response against validators generated from the in-repository `hub-http-v1@1.2.0` contract before returning it to the caller. There is no browser client.

The package is private and is not a registry release. The package manifest identifies version `2026.36.2`, Apache-2.0 licensing, and an evidence-only runtime policy. Local fixtures and unit checks demonstrate SDK behavior against checked-in inputs. They do not establish the behavior of a live Hub or a production deployment.

## Entry points

Create a client from the Node entry point:

```ts
import { createHubClient } from "@teslatlas/sdk/node";

const client = createHubClient({
  endpoint: "https://hub.example.invalid",
  expectedHubId: "11111111-1111-4111-8111-111111111111", // From trusted setup context.
  credentials, // A caller-owned HubCredentialStore.
});

const discovery = await client.discover();
const vehicles = await client.vehicles();
```

The root `@teslatlas/sdk` entry point exports public types, errors, opaque value constructors, and caller-owned interfaces. It has no factory: only `@teslatlas/sdk/node` carries a claim transport that can enforce the invitation pin.

The example endpoint and Hub UUID are placeholders, and the credential store is caller-owned. The [API reference](docs/api.md) lists the methods. The [compatibility guide](docs/compatibility.md) describes the profile, result, error, ETag, and pagination behavior.

## Hub client

`createHubClient` targets `hub-http-v1@1.2.0`. It uses a fixed HTTPS endpoint, an expected Hub UUID, and a caller-owned credential store. Its operations cover discovery, health, readiness, pairing, credential rotation, vehicles, current state, and drives. The client requires `profile` in the discovery document, so it refuses a Hub that predates that profile.

The Node entry point keeps normal CA and hostname verification. During pairing it checks the invitation `tlsPin` against the SHA-256 digest of the connected leaf certificate DER bytes on the same new TLS connection before sending claim bytes. Without a claim transport that can do this, pairing fails with `hub_tls_pin_unavailable` before discovery or claim network I/O. The SDK never disables certificate verification.

Credentials remain caller-owned. Do not put authorization values or invitations in source files, command arguments, or published logs. See the [architecture guide](docs/architecture.md) for lifecycle and boundary details.

## Local development

Use the Node `26.10.0` and npm `12.1.0` toolchain named by `package.json`. The manifest declares no broader runtime floor because the current evidence policy does not accept one. The accepted runtime evidence still names Node `26.7.0` and npm `11.19.0`; it covers the new versions only after a focused run records them.

Follow `.github/CONTRIBUTING.md` in the [source repository](https://github.com/magrathean-uk/teslatlas-sdk-typescript) and applicable workspace instructions before running package commands. A build replaces the checkout's `dist/` directory. With the pinned toolchain, the local verification sequence is:

```bash
npm ci
npm run build
npm run verify
```

The `verify` script runs formatting, linting, type checking, the protocol check, the build, the unit and script tests, the package check, and the clean-package test. Targeted commands are also available:

```bash
npm run build
npm run typecheck
npm run test:unit
npm run protocol:check
npm run pack:check
npm run test:package
```

Types and validators are generated from `../protocol/profiles/hub-http-v1/1.2.0`, which the generator reads in place. `npm run protocol:generate` regenerates them, and `npm run protocol:check` verifies the profile's `SHA256SUMS` and that the committed output matches a fresh generation.

## Further reading

- [API reference](docs/api.md)
- [Architecture](docs/architecture.md)
- [Compatibility](docs/compatibility.md)
- [Product versioning](docs/product-versioning.md)
- [Apache-2.0 licence](LICENSE)

Repository-only guides are available in the [source repository](https://github.com/magrathean-uk/teslatlas-sdk-typescript): `.github/CONTRIBUTING.md`, `.github/SUPPORT.md`, `.github/SECURITY.md`, and `docs/legal/licensing.md`. They are not bundled in the npm archive.

## Licence

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE) for the complete licence text and copyright notice.

<sub>© 2026 MAGRATHEAN UK LTD · [Legal](https://github.com/magrathean-uk/.github/blob/main/LEGAL.md)</sub>
