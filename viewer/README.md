# Teslatlas viewer

Small open-source reference client for Teslatlas Hub.

The current release-cohort product version and its compatibility status are
described in [product versioning](docs/product-versioning.md).

The viewer proves the public client shape without duplicating the proprietary
Teslatlas product. It covers Hub discovery and pairing, health, vehicles,
current state, recent drives and charges, data quality, collector freshness,
and paired-device management.

## Current boundary

The viewer has two explicit modes:

- **Fixture mode:** deterministic, redacted, viewer-owned example data. It
  demonstrates client behaviour without a Tesla account, Hub service, secret,
  or network API call.
- **Live mode:** uses the verified, project-relative `@teslatlas/sdk`
  `2026.36.2` artifact through its public browser export. It accepts an HTTPS
  endpoint, expected Hub UUID, optional invitation TLS identity, and pairing
  invitation. Credentials stay in memory.

`ViewerDataSource` is an internal display seam, not a proposed public SDK API.
The live adapter maps public `hub-http-v1@1.0.0` results without copying the
SDK transport. Charges, quality, collector cost/backup age, and remote device
management remain visibly unsupported because that profile does not expose
them.

## Run locally

Requirements: Node.js 22.12 or later and npm.

```sh
npm ci
npm run dev
```

The default page opens the complete, already-paired fixture. Useful deterministic
states:

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

The fixture invitation code is `482731`. It is example data, not a credential.

## Run the installed static viewer

The package exposes `teslatlas-viewer`, a small Node HTTP server for the
packaged production assets. It does not use Vite's development or preview
server at runtime.

```sh
npm run build
npm pack
npm install --prefix ./viewer-install ./teslatlas-viewer-2026.36.2.tgz
./viewer-install/node_modules/.bin/teslatlas-viewer --host 127.0.0.1 --port 4173
```

Use port `0` to request an available port. The command prints the actual bound
HTTP address. `teslatlas-viewer --help` lists the complete bounded interface.

## Verify

```sh
npm run typecheck
npm test -- --run
npm run build
npm run test:cli
npx playwright install chromium
npm run test:e2e
# Requires the private actual-Hub/browser runtime described in the Task 5 report:
npm run test:e2e:hub
```

Browser verification covers the seven views, all data states, no fixture API
requests, pairing, confirmed paired-device removal, clearing the local session,
axe checks, keyboard entry, reduced motion, and 400% equivalent reflow. The
separate live lane exercises the built bundle, packed SDK, normal CA trust,
pairing, bounded drive pagination, conditional requests, identity failure,
session loss, and unsupported-resource boundaries.

## Screenshots

| Complete desktop | Complete mobile |
| --- | --- |
| ![Complete desktop fixture](output/playwright/screenshots/complete-desktop.png) | ![Complete mobile fixture](output/playwright/screenshots/complete-mobile.png) |

More captured states are under `output/playwright/screenshots/`.

## Read next

- [Architecture](docs/architecture.md)
- [Protocol learning guide](docs/protocol-learning-guide.md)
- [Public SDK integration roadmap](docs/public-sdk-integration-roadmap.md)
- [Privacy](docs/privacy.md)
- [Browser support](docs/browser-support.md)
- [Foundation plan](docs/plans/2026-08-30-foundation.md)
- [Reference-client implementation plan](docs/superpowers/plans/2026-08-30-reference-client.md)

## Non-goals

No vehicle commands, advanced maps, proprietary analytics, embedded Grafana,
operator console, hosted deployment, or commercial Teslatlas styling.

## Licence

Apache-2.0.
