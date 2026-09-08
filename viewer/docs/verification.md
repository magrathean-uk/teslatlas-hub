# Viewer verification

This page records the commands and evidence for the Viewer. A green local
check proves the current checkout only. Installed Hub acceptance also needs a
Hub-owned disposable target and its redacted descriptor; it is not inferred
from fixture data or from a build.

## Local source gates

Verified in the 2026-09-08 execution on macOS with Node `v26.8.1`, npm
`11.19.0`, and `/usr/bin/tar`: `npm ci --include=dev`, `npm run sdk:verify`,
`npm run typecheck`, `npm test -- --run` (104 tests),
`npm run test:sdk-artifact` (2 tests), `npm run build`, `npm run test:cli` (1
test), and `npm run test:e2e` (14 Chromium tests) passed. The production build
still emits Vite's advisory about the roughly 1,064 kB JavaScript chunk; it is
recorded as a measured M3 limitation rather than suppressed or treated as a
failure.

The earlier connection findings are covered by source and browser checks:
M1 is closed by the editable connection form and in-place correction path; M2
is closed by retaining the invitation TLS identity in the live adapter and
mapped snapshot. M3 remains an advisory bundle-size limitation pending a
measured code-splitting decision.

Run from this repository after the accepted SDK archive and metadata are present:

```sh
npm ci --include=dev
npm run sdk:verify
npm run typecheck
npm test -- --run
npm run test:sdk-artifact
npm run build
npm run test:cli
npm run test:e2e
```

The locked development toolchain requires Node 26.x (the current recorded
machine uses Node 26.8.1), npm 11.x, and `tar`. The SDK verifier checks the
project-relative `@teslatlas/sdk` archive before typecheck, test and build. The
current metadata records SDK version `2026.36.2`, 80 installed members,
tarball SHA-256
`d1ab6ba0ede3a24ae12ed4151db0c90bf957fa19f5640cc4323bd368e565e8bb`, and
profile `hub-http-v1@1.0.0`.

The default Playwright suite is fixture-only and makes no Hub requests. The
CLI test packages and installs the static server, checks GET/HEAD, fallback,
version metadata, containment and shutdown, and does not upload the package.

## Managed synthetic-Hub lane

`npm run test:e2e:hub` builds the production Viewer and starts the packaged
static CLI through `playwright.hub.config.ts`. The existing private descriptor
and browser trust witness supply the endpoint, Hub UUID, disposable invitation,
normal CA trust and synthetic data. Required private variables are selected by
the Hub handoff and are never copied into this document or a receipt.

The managed lane supplements, but does not replace, ordinary installed-Hub
acceptance. Its credential-rotation `auth-loss` scenario proves a bounded 401
recovery path; only a Hub operator's confirmed revocation of the exact test
device proves revocation.

The managed lane was not run in this execution because the Hub-owned descriptor,
browser trust witness, and forwarding handoff were not available.

## Installed production Viewer lane

First serve a production build with the packaged CLI or the local Docker image.
Then run the read/claim-only smoke against that already serving origin:

```sh
TESLATLAS_VIEWER_EXTERNAL_SERVER=1 \
TESLATLAS_VIEWER_PAGE_ORIGIN=https://viewer.example.test \
npm run test:e2e:installed:hub
```

The Hub owner supplies the remaining private descriptor variables and keeps
invitations out of logs. The smoke starts at `/`, follows the visible live
connection link, corrects a wrong Hub UUID without reload, pairs through the
ordinary form, reads current data, loads a seeded 51-drive vehicle as
25/25/1, observes terminal history and an empty vehicle, refreshes, and clears
the local session. Normal browser certificate validation remains enabled.

The coordinated installed acceptance requires a separate Hub-owned browser
session to observe a temporary outage and recovery, then operator revocation
of the exact disposable device, a 401 on the next authenticated read, and a
successful fresh-invitation re-pair. The Viewer never performs the revoke or
changes Hub service state. Record the browser, Viewer origin, Hub version and
source, profile/artifact identity, request/status/ETag observations, seed
counts, operator revoke result and cleanup in a private owner-only receipt.
Persist no invitation, authorization value or raw cursor; compare cursors only
in memory and record presence or a digest when needed.

As of the current plan, this installed target and operator receipt are pending
Hub admission. Therefore `compatibility/hub.json` remains `candidate` with
empty tested-Hub and receipt arrays. Chromium is the only browser with current
automated evidence; Safari, Firefox, Edge and other Hub platforms remain
untested unless their own runs are recorded.

## Container check

The Docker build runs the same SDK verifier and production build in a pinned
Node 26.8.1 Debian slim image. Validate locally when Docker is available:

```sh
docker compose config
docker compose up --build -d
curl -fsS http://127.0.0.1:4173/
docker compose down
```

The runtime contains only `dist`, the static CLI and package metadata, runs as
the non-root `node` user, and has no Viewer persistence. Public HTTPS and Hub
CORS belong to the existing TLS proxy and Hub configuration.

`docker-compose config` passed on this host. Image build and runtime checks were
blocked because the Docker daemon socket was unavailable, and the installed
`docker` binary does not provide the Compose subcommand; no container or
multi-architecture claim is made.
