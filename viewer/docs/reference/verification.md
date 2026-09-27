# Viewer verification

These commands are documented from the source. They were not executed during the documentation refresh. Viewer development remains deferred; follow [AGENTS.md](../../AGENTS.md) before running checks.

## Source checks

Prepare the pinned SDK archive and install development dependencies using the [README](../../README.md#build-from-source). Run only checks relevant to an authorized change.

| Command | What it checks |
| --- | --- |
| `npm run sdk:verify` | Recorded SDK archive and installed package identity |
| `npm run typecheck` | TypeScript project compilation checks |
| `npm test -- --run` | Vitest unit and component tests, including adapter lifecycle |
| `npm run test:sdk-artifact` | SDK artifact verifier cases |
| `npm run test:sdk-source` | Source artifact preparation cases |
| `npm run build` | SDK verification, TypeScript compilation, and Vite output |
| `npm run test:cli` | Build, local package/install, static server, containment, and shutdown |
| `npm run test:container:source` | Docker and Compose source shape, not container runtime |
| `python3 -m unittest discover -s tools -p 'test_matrix_contract.py'` | Compatibility admission contract, using Python 3.10 or later |
| `node --test tools/matrix-live.test.mjs` | Compatibility launcher and SessionInput wiring |

`pretypecheck`, `pretest`, and `prebuild` run the SDK verifier. TypeScript checks and builds can write output; the CLI check also builds, packs, installs, and starts a local server. These are not read-only documentation checks.

## Fixture browser checks

Select the intended specs explicitly:

```sh
npm run test:e2e -- e2e/viewer.spec.ts
```

This uses the configured Chromium project and starts Vite on loopback port 4173. The screenshot recipe, `npm run capture:screenshots`, writes fixture images under `output/playwright/screenshots/`. Review image changes before including them in a patch.

The unqualified `npm run test:e2e` is not a fixture-only command in this checkout. Its configuration excludes `live-hub.spec.ts` and `installed-hub.spec.ts`, but also discovers the installed R1 recovery and data-state specs. Those require an external Viewer and private Hub coordination. Do not run the unqualified command as a local smoke check.

An existing accessibility spec remains in the tree. The current workspace scope excludes accessibility work; its presence does not authorize a new accessibility task.

## Live and installed Hub checks

`npm run test:e2e:hub` first builds the Viewer, then starts its static CLI through `playwright.hub.config.ts`. It needs a fresh Hub-owned descriptor and normal browser trust.

The installed scripts are `test:e2e:installed:hub`, `test:e2e:installed:recovery`, and `test:e2e:installed:data-state`. They set external-server mode. Configuration requires a bound page origin, browser executable, and raw-evidence directory, while the specs need the matching private handoff. These scripts are not standalone setup recipes.

The Hub coordinator supplies disposable synthetic records, distinct Viewer and Hub origins, CORS configuration, trusted TLS, invitations, lifecycle control, and cleanup ownership. The Viewer does not revoke devices or stop Hub processes. Never bypass certificate validation, reuse expired invitations, or copy raw credentials into a receipt. Operator revocation and a browser's simulated authentication failure are different evidence.

## Historical evidence

The preserved receipts describe specific past runs:

| Receipt | Scope recorded |
| --- | --- |
| [B1 installed browser](../development/receipts/2026-09-08-b1-installed-browser-r3.json) | Pairing, five drives, empty vehicle, refresh, and local session clearing on Debian ARM64 |
| [R1 recovery](../development/receipts/2026-09-09-r1-installed-recovery7.json) | 51-drive paging, outage recovery, operator revocation, and re-pairing |
| [R1 data state](../development/receipts/2026-09-09-r1-installed-data-state.json) | Conditional revalidation, unsupported resources, partial failure, and cancellation |
| [D1 container](../development/receipts/2026-09-09-d1-container-source.json) | One Debian ARM64 container's build, liveness, HTTP, restart, and cleanup |

These records do not verify today's dirty checkout, a different package, a different browser, or a complete compatibility matrix. The [compatibility manifest](../../compatibility/hub.json) still says `candidate` and has no accepted Hub-version or receipt entries. Native package and macOS lifecycle acceptance are not established here.

## Container boundary

[Dockerfile](../../Dockerfile) builds with `node:26.8.1-bookworm-slim`. The SDK archive must exist before its build. The runtime serves static assets as the non-root `node` user. [Compose](../../compose.yaml) maps port 4173 to host loopback, uses `unless-stopped`, and declares no volume.

For an authorized container task, `docker compose config` inspects configuration, `docker compose up --build -d` builds and starts the service, and `docker compose down` stops it. The health probe checks HTTP 200 at the static root only. It does not test Hub readiness, pairing, browser trust, or CORS. A running container is not an installed user-path acceptance result.
