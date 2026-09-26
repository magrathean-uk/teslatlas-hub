# Teslatlas viewer

Teslatlas viewer is a small open-source reference client for the public Teslatlas Hub SDK. It demonstrates discovery, pairing, Hub health, vehicles, current state, recent sessions, data quality, collector freshness, and paired-device management. It is a protocol proof, not Teslatlas feature parity or a replacement for the Hub, protocol, or SDK.

## What it demonstrates

The viewer has two modes:

- **Fixture mode** uses deterministic, redacted, viewer-owned data. It makes no Hub API request and needs no Tesla account, Hub service, or credential. Once the local server and assets are available, it needs no external network connection.
- **Live mode** uses the project-pinned `@teslatlas/sdk/browser` artifact and connects from the browser to a configured HTTPS Hub. The endpoint, expected Hub UUID, invitation TLS identity, and pairing invitation are separate inputs. Pairing material stays in memory and must be entered again after a reload.

The current live profile is `hub-http-v1@1.0.0`. It exposes discovery, pairing, health, readiness, vehicles, current state, and bounded drive pages. Charges, richer data quality, collector cost or backup age, and remote device management are shown as unsupported when the profile does not provide them. Clearing the local session only clears viewer-held state; it does not revoke a Hub device.

The reference views label loading, empty, absent, stale, inferred, degraded, offline, error, and complete states. The fixture invitation `482731` is public example data and grants no access.

## Development status

Viewer development is deferred by its owner. The commands below document the source workflow for future authorized work; this documentation refresh did not run them or reopen implementation, testing, or live pairing. Compatibility remains a candidate with no accepted Hub-version entries in [the manifest](compatibility/hub.json). Historical receipts do not establish acceptance of the current checkout.

## Requirements

For a source checkout, use Node.js 26.x, npm 11.x, and `tar`. The package manifest accepts Node `>=26.0.0` and npm `>=11.0.0`. Recreating the pinned SDK archive is stricter: `npm run sdk:prepare` requires Node `v26.7.0`, npm `11.19.0`, and the SDK source commit recorded in `artifacts/teslatlas-sdk.json`.

The repository expects the project-relative SDK archive at `artifacts/teslatlas-sdk-2026.36.2.tgz`. That generated archive is ignored. A clean checkout therefore needs an SDK source checkout and the exact Node/npm executables before `npm ci` can install the file dependency.

## Build from source

From the viewer checkout, prepare the SDK archive, install dependencies, then start Vite. Replace the paths below with the SDK checkout at the recorded commit and the exact Node executable; npm 11.19.0 must be alongside that executable:

```sh
TESLATLAS_SDK_SOURCE=../teslatlas-sdk-typescript \
TESLATLAS_SDK_NODE=/path/to/node-v26.7.0/bin/node \
npm run sdk:prepare
npm ci --include=dev
npm run dev -- --host 127.0.0.1
```

Open the address printed by Vite. The default route is the complete paired fixture. Useful deterministic routes are:

```text
/?paired=false
/?scenario=empty
/?scenario=stale
/?scenario=inferred
/?scenario=degraded
/?scenario=offline
/?scenario=error
/?scenario=loading
/?mode=live&paired=false
```

The home page links between the fixture demo and the live connection form. Live mode begins unpaired.

## Build and serve the package

`npm run build` creates the static `dist` assets. The packaged `teslatlas-viewer` command is a small Node HTTP server for those assets. It supports `--host`, `--port`, `--help`, and `--version`; port `0` asks the operating system for an available port.

```sh
npm run build
node bin/teslatlas-viewer.mjs --host 127.0.0.1 --port 4173
```

For local package inspection, `npm pack` runs the build again through `prepack` and creates an archive. It does not publish to a registry. The manifest is marked `private: true`; no registry installation or release availability is implied.

The server serves `GET` and `HEAD`, falls back to `index.html` for application routes, rejects path traversal, and serves only the packaged `dist` tree. It does not use Vite at runtime. HTTPS termination and the Hub CORS allow-list belong to the deployment around this static service.

## Docker and Compose

The Docker image builds the static assets in Node 26.8.1 Debian slim, then runs only `dist`, the static server, the health probe, and package metadata as the non-root `node` user. It has no Viewer data volume. Compose binds the service to `127.0.0.1:4173` and uses `unless-stopped`.

After preparing the ignored SDK archive, use the Compose command available on your host:

```sh
docker compose up --build -d
docker compose down
```

While the service is running, open `http://127.0.0.1:4173/` in a browser.

On hosts with the legacy executable, replace `docker compose` with `docker-compose`. A standalone image can be built with `docker build -t teslatlas-viewer:local .`. The container serves static assets only. The browser still connects directly to the Hub HTTPS origin.

## Verification

The package scripts expose type checking, unit tests, SDK artifact checks, the static CLI test, a Docker source-shape test, fixture browser tests, and Hub-coordinated live lanes:

```sh
npm run sdk:verify
npm run typecheck
npm test -- --run
npm run test:sdk-artifact
npm run test:cli
npm run test:container:source
npm run test:e2e -- e2e/viewer.spec.ts
```

The Playwright command above selects the fixture Viewer spec. Existing accessibility tests remain in the source, but current workspace scope excludes accessibility work. `npm run test:e2e` also discovers the installed recovery and data-state files, which require a Hub-owned handoff and are not standalone fixture checks. The live scripts require a disposable Hub, normal browser trust, private coordinator inputs, and evidence paths. They must not be run as an ad hoc environment-variable recipe. See [verification](docs/verification.md) for the evidence boundaries and [browser support](docs/browser-support.md) for current browser claims.

## Read next

- [Architecture](docs/architecture.md)
- [Protocol learning guide](docs/protocol-learning-guide.md)
- [Privacy](docs/privacy.md)
- [Browser support](docs/browser-support.md)
- [Product versioning](docs/product-versioning.md)
- [Verification](docs/verification.md)
- [Development status](docs/development/PLAN.md)
- [Contributing](CONTRIBUTING.md)
- [Support](SUPPORT.md)
- [Security](SECURITY.md)

## Non-goals

The viewer does not provide vehicle commands, advanced maps, proprietary analytics, embedded Grafana, an operator console, hosted deployment, or commercial Teslatlas styling. It does not implement the Hub or private Hub routes.

## License

This project is licensed under the [Apache License, Version 2.0](LICENSE). The complete license text is preserved. See [licensing](docs/legal/licensing.md) for attribution and dependency boundaries.
