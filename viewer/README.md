# Teslatlas viewer

Small open-source reference client for Teslatlas Hub.

The viewer proves the public client shape without duplicating the proprietary
Teslatlas product. It covers Hub discovery and pairing, health, vehicles,
current state, recent drives and charges, data quality, collector freshness,
and paired-device management.

## Current boundary

The sibling `teslatlas-protocol` and `teslatlas-sdk-typescript` repositories
are foundation-only. They do not yet publish schemas, compatibility fixtures,
or a runtime SDK. This repository therefore has two explicit modes:

- **Fixture mode:** deterministic, redacted, viewer-owned example data. It
  demonstrates client behaviour without a Tesla account, Hub service, secret,
  or network API call.
- **Live mode:** intentionally unavailable until a released TypeScript SDK and
  protocol artifacts exist. It fails locally with a useful explanation and
  does not guess routes or payloads.

`ViewerDataSource` is an internal application seam, not a proposed public SDK
API. A future SDK adapter can replace the fixture source without turning the
viewer model into a competing protocol contract.

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

## Verify

```sh
npm run typecheck
npm test -- --run
npm run build
npx playwright install chromium
npm run test:e2e
```

Browser verification covers the seven views, all data states, no fixture API
requests, pairing, paired-device removal, axe checks, keyboard entry, reduced
motion, and 400% equivalent reflow.

## Screenshots

| Complete desktop | Complete mobile |
| --- | --- |
| ![Complete desktop fixture](output/playwright/screenshots/complete-desktop.png) | ![Complete mobile fixture](output/playwright/screenshots/complete-mobile.png) |

More captured states are under `output/playwright/screenshots/`.

## Read next

- [Architecture](docs/architecture.md)
- [Protocol learning guide](docs/protocol-learning-guide.md)
- [Privacy](docs/privacy.md)
- [Browser support](docs/browser-support.md)
- [Foundation plan](docs/plans/2026-08-30-foundation.md)
- [Reference-client implementation plan](docs/superpowers/plans/2026-08-30-reference-client.md)

## Non-goals

No vehicle commands, advanced maps, proprietary analytics, embedded Grafana,
operator console, hosted deployment, or commercial Teslatlas styling.

## Licence

Apache-2.0.
