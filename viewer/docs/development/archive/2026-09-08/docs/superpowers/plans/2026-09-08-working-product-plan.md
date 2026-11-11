# Teslatlas Viewer Working Product Development Plan

**Latest user model policy (2026-09-08):** This coordinator and delegation use `gpt-6-astra` with `thinking=high`. This product task, coding, goal execution and all development workers use `gpt-5.6-terra` with `thinking=high`. This supersedes every earlier model instruction in this plan and its historical goal snapshot. Preserve checkpoints at model transitions and verify the actual new turn model.

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` to execute this plan milestone by milestone. Keep one goal active across phase boundaries; a completed milestone is not whole-product completion.

**Goal:** Finish the Teslatlas Viewer as an ordinary read-only production browser product against a real installed current Hub, then complete its local Docker evidence, current documentation, review, and authorized source-only GitHub update.

**Architecture:** Retain the React/Vite application, `ViewerDataSource` boundary, public `@teslatlas/sdk/browser` transport, in-memory pairing, and dependency-free static CLI. Viewer owns its UI, SDK binding, local package, browser tests, Docker example, and product documentation. Hub owns its registry, installed targets, service lifecycle, CORS configuration, seed data, operator revocation, aggregate ledger, and host coordination.

**Tech stack:** React 19, TypeScript, Vite 8, npm 11, Vitest, Playwright, Node 26, the private local `@teslatlas/sdk` `2026.36.2` package, and `hub-http-v1@1.0.0`.

**Spec:** The user's 2026-09-08 working-product and goal-continuation instructions, workspace `WORKSPACE_AUTHORITY.md`, this repository's `docs/verification.md`, and the read-only Hub ledger at `../hub/docs/compatibility/execution-state.json`.

**Historical model policy:** The earlier Terra-max, Sol-high, and Luna-max assignments are superseded by the latest policy above. No token or time budget is assigned.

## Global constraints

- Work only in the existing independent `teslatlas-viewer` `main` checkout. Preserve unrelated changes and never create a replacement branch/worktree, reset, clean, or rewrite history.
- Teslatlas App and every sibling product are outside Viewer ownership. Read Hub state when needed; only the Hub task may change Hub source, ledger, installed hosts, brokers, services, or shared compatibility state.
- Product scope is read-only health/readiness, vehicles, current values, and drives. Pairing is memory-only. Never add vehicle commands, collection triggers, remote device administration, private Hub sockets, credential persistence, or invented protocol routes.
- Keep invitation secrets, bearer values, private descriptors, TLS keys, and raw cursors out of source, logs, screenshots, receipts, images, and browser storage. Record only bounded redacted evidence.
- GitHub is source storage only. Do not add CI, release/tag workflows, registry publication, downloadable build artifacts, or binary uploads. Local test packages and private local evidence are allowed.
- Continue through every READY milestone and every still-possible local step. Do not stop at a phase boundary, repeat unchanged green suites, or add tiny scaffolds as progress. Wait only when the exact next action requires an unavailable owner or resource.
- A milestone may be DONE while full product acceptance remains pending. The goal completes only after every acceptance criterion near the end of this plan is satisfied or explicitly narrowed by the user.

---

## Current checkpoint — 2026-09-08T19:02:00Z

Viewer is on `main` at `ba5424dcfeab59ac9fea0106f84fb5281672751a`, matching `origin/main`. The working tree contains the current Viewer-owned SDK rebind and compatibility package; preserve every listed path until review resolves it.

| Area | Current state | Evidence and limit |
| --- | --- | --- |
| Live setup and UX | **DONE** | Root demo/live choice, editable HTTPS/Hub identity, in-memory pairing, visible recovery, and access-ended flow are implemented at `ba5424d`. |
| Drive history | **DONE** | Explicit per-vehicle 25-row paging, 51-row `25/25/1` behavior, opaque cursor handling, cache revalidation, cancellation, retained partial failures, and terminal/auth-loss states are covered by source tests. |
| SDK/profile binding | **DONE locally** | `artifacts/teslatlas-sdk.json` binds the ignored, source-recreated 81-member archive `03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c`, installed manifest `4b1a44f7d2cd0ab6c772642125358703aa25c1efe32d164a119a6fc9d858ded6`, and profile `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`. Preparation verifies TypeScript SDK commit `b1cd548fef7ddd26be2637c14bb435480166cef7` with Node `v26.7.0` and npm `11.19.0`. |
| Local source evidence | **DONE for current code** | A clean Viewer copy recreated the SDK archive, then `npm ci`, SDK verification, and production build passed. The focused source-pack test, 2 SDK-artifact tests, current production build, and installed CLI server smoke passed. This is local evidence, not installed-Hub acceptance. |
| Normal CLI command | **DONE** | `npm run test:cli` now isolates npm's cache, requires a successful production build while preserving the Vite chunk advisory, then packs those verified assets without rerunning `prepack`. The loopback-capable integration run passed all package/install/server/containment/shutdown assertions. |
| Viewer installed lane | **PREPARATION COMPLETE; RUNTIME PENDING** | The Viewer coordinator now consumes Hub's reviewed private page/CORS/CA/browser/reservation wire and keeps browser TLS validation enabled. Hub still owns the source-pin update, runner launch, installed service/browser runtime inventory, per-case raw evidence, admission, and supplement. |
| Docker runtime | **WAITING FOR OWNER/RESOURCE** | Docker/Compose source exists and `docker-compose config` passed previously. Image build, non-root runtime, HTTP probes, restart, and clean stop have not run because the local daemon/Compose integration was unavailable. |
| Publication | **WAITING** | Current changes are uncommitted. The SDK tarball is generated locally and ignored; no commit, push, tag, release, CI, or artifact upload has occurred. |

Current Viewer-owned working paths include:

- SDK rebind: `.gitignore`, `artifacts/teslatlas-sdk.json`, `package-lock.json`, `scripts/prepare-sdk-artifact.mjs`, `scripts/prepare-sdk-artifact.test.mjs`, `scripts/verify-sdk-artifact.test.mjs`, `compatibility/hub.json`, and current SDK documentation.
- Compatibility package: `tools/matrix-contract.json`, `tools/matrix_contract.py`, `tools/matrix-live.mjs`, `tools/viewer-runtime-v1.schema.json`, `tools/viewer-sdk-raw-v1.schema.json`, `tools/viewer-ui-raw-v1.schema.json`, and their focused tests.
- Implemented product at `ba5424d`: `src/`, `e2e/`, static CLI, `Dockerfile`, `.dockerignore`, `compose.yaml`, README, AGENTS guidance, and current product docs.

Historical Task 5 and Task 10a evidence remains useful for provenance, but its 52-test, 11-E2E, 80-member SDK, hidden-live-entry, and eager paging descriptions are superseded by the checkpoint above. Do not rewrite those historical reports or reuse their old artifact identity as current evidence.

## Dependency table

| Dependency owner | Exact missing input | Work still possible locally | Event that unblocks dependent work |
| --- | --- | --- | --- |
| Hub task `01a07f89-45cb-7ea2-b96e-aa89de18fd14` | Reviewed `viewer` fixed-registry entry; staged Viewer manifest/validator/launcher/schema bindings; installed target descriptor; trusted Viewer and Hub origins; 51-drive and empty-vehicle seed; disposable pairing, outage, revoke, restore, and fresh-invitation operations | Complete all source, package, docs, Docker static review, and fixture checks without touching Hub | Hub records the reviewed Viewer entry and supplies a private installed-session handoff for the exact archive/profile |
| Viewer task `01a07f89-5148-7463-b374-7b00232e082c` | An available local Docker daemon and working Compose command for the current checkout | Finish the CLI fix, Dockerfile/Compose static review, docs, and installed-Hub preparation | `docker info` and the chosen `docker compose` or `docker-compose` command succeed without changing another project |

The TypeScript runtime/profile dependency is satisfied for local source-only work: `sdk:prepare` checks the recorded source commit and toolchain, writes only an ignored local archive, and a clean Viewer-copy recipe passed. Publication still waits the required installed-Hub and Docker runtime evidence.

The native goal is currently `blocked`, and the exposed agent tools cannot resume or replace that unfinished goal. Only the user's supported Resume goal control changes that native status. This does not block already authorized ordinary work in READY Milestones 2 and 3. Parent readback reports 30% weekly account usage and no rate limit reached; that account observation is not a goal token/time budget.

## Milestone 1 — Current live product and SDK rebind

**Status: DONE**

**Owned files:** `src/**`, `e2e/viewer.spec.ts`, `e2e/accessibility.spec.ts`, `artifacts/teslatlas-sdk.json`, `compatibility/hub.json`, `scripts/verify-sdk-artifact.mjs`, `scripts/verify-sdk-artifact.test.mjs`, and current docs.

**Interfaces:** `ViewerDataSource.loadMoreDrives(vehicleId, signal?)`, `DrivePagingState`, `useViewer.loadMoreDrives`, `SdkDataSource`, the `@teslatlas/sdk/browser` public export, and `hub-http-v1@1.0.0`.

**Evidence:** The current identities and green local gates are recorded in the checkpoint table and `docs/verification.md`. Do not rerun them until a later change affects their coverage.

**Acceptance boundary:** Fixture Chromium, source tests, a production build, and a local static-server smoke do not prove installed Hub, real CORS/TLS, operator revocation, Docker runtime, or publication.

## Milestone 2 — Repair the normal CLI pack/install gate

**Status: DONE**

**Files:** Modify `scripts/viewer-cli.test.mjs`. Modify `package.json` only if one narrowly named script is required. Modify `vite.config.ts` only if a measured code split fixes a real load problem; never use a blanket warning-limit increase as the gate fix. Update `docs/verification.md` with the final command and result.

**Interfaces:** Preserve `TESLATLAS_VIEWER_PACKAGE` as the externally supplied package path. Preserve the existing package/install checks: package metadata, `--help`, `--version`, local bind, GET/HEAD, hashed assets, `version.json`, extensionless fallback, missing asset, method rejection, traversal/symlink containment, bundle fingerprint, SIGTERM, and clean exit.

- [ ] Reproduce `npm run test:cli` once from a clean dependency state with a temporary npm cache. Capture the `npm pack` exit code, stdout JSON, stderr warning, and hidden lockfile state separately so a stale `node_modules/.package-lock.json` cannot masquerade as the Vite warning defect.
- [ ] Add a focused test path that explicitly runs the production build and asserts its exit code while retaining the measured chunk advisory in diagnostic output. The test must fail on a build error and must not require stderr to be empty.
- [ ] Pack the already-built assets without recursively rerunning the same prepack build, then execute the existing install/server/containment assertions against that exact archive. Keep normal package metadata and file whitelist behavior unchanged.
- [ ] If ordinary `npm pack` itself still exits nonzero from a clean tree, fix that package lifecycle path rather than teaching the test to accept code `1`. Do not suppress all Vite warnings or skip the build.
- [ ] Run `npm run test:cli`. Expected: 1 passed, exit 0, with the chunk advisory retained as an explicit M3 observation when emitted.
- [ ] Run `npm run build` once after the fix. Expected: exit 0 and a measured asset report. Run the 2 SDK artifact tests only if the CLI change touches artifact preparation.

**Evidence:** `npm run test:cli` passed with loopback networking enabled: one test, zero failures. The restricted sandbox reaches only `listen EPERM` after the build/package portion because it cannot bind `127.0.0.1`; that is not a product failure. The Vite advisory remains visible and is carried into M3 as a measured limitation.

**Exit:** The repository's normal CLI command is green from a clean local source state, and the existing package/server security checks still run. M3 remains either a measured advisory or a separately justified code-splitting change.

## Milestone 3 — Finish all local source, Docker-static, and documentation work

**Status: DONE**

Begin after Milestone 2 because both milestones may update `docs/verification.md` and package instructions.

**Files:** Review `Dockerfile`, `.dockerignore`, `compose.yaml`, `bin/teslatlas-viewer.mjs`, `README.md`, `docs/architecture.md`, `docs/browser-support.md`, `docs/privacy.md`, `docs/product-versioning.md`, `docs/protocol-learning-guide.md`, `docs/public-sdk-integration-roadmap.md`, `docs/verification.md`, all Viewer `AGENTS.md` files, and the compatibility package under `tools/`.

**Interfaces:** The runtime image contains only `dist`, `bin/teslatlas-viewer.mjs`, and `package.json`, runs as the non-root Node user, listens on container port 4173, and keeps no Viewer state. The browser receives Hub endpoint/UUID/invitation through the UI; container environment variables do not configure the static bundle.

- [x] Review the complete Viewer diff against the current checkpoint. Preserve the compatibility source files and local SDK metadata; no generated screenshots, caches, logs, test output, private evidence, or sibling changes were introduced.
- [x] Verify the Docker build stage includes the exact SDK verifier inputs and `tar`, and that `.dockerignore` excludes `.git`, `node_modules`, build output, screenshots, private evidence, secrets, and `.env` while retaining the local SDK inputs required for the build.
- [x] Run the available static Compose validation: `docker-compose config` passed. This does not establish image/runtime acceptance.
- [x] Reconcile current docs to the 81-member `03dddd...` SDK prepared from commit `b1cd548...`, `b80d940e...` profile, visible live/demo route, 25-row paging, manual refresh, memory-only pairing, exact CORS origin, browser trust, CLI advisory, Docker network boundary, and pending installed target.
- [x] Review Markdown links and documented commands. All local Markdown links passed; historical plans remain historical.
- [x] Define and verify the clean-source SDK preparation boundary: `sdk:prepare` recreates the exact archive from the pinned source commit/toolchain, and the archive remains ignored rather than committed.
- [x] Run focused checks: metadata JSON parsing, 9 contract tests with Python 3.13, 3 Node launcher/wire tests, a bound external Playwright configuration listing, static Compose validation, and local Markdown-link checks passed. The preinstalled Xcode Python 3.9 is unsupported by the contract test's slotted dataclasses.

**Evidence:** `docker-compose config` passed. The Docker daemon and Compose v2 command are unavailable, so the image/runtime lane remains untested. The source-only SDK recipe and isolated clean-copy install/verify/build passed; its generated archive remains untracked.

**Exit:** All locally owned implementation, review, static packaging work, and current documentation are finished. Remaining work requires only the Docker runtime resource, Hub installed handoff, and final publication gate.

## Milestone 4 — Accept the production Viewer against a real installed Hub

**Status: PREPARATION COMPLETE; WAITING FOR HUB RUNTIME — Hub task `01a07f89-45cb-7ea2-b96e-aa89de18fd14`**

**Viewer files if actual failures require changes:** `e2e/installed-hub.spec.ts`, `e2e/live-hub.spec.ts`, `e2e/live-evidence.ts`, `playwright.hub.config.ts`, `package.json`, relevant `src/**` tests, and `docs/verification.md`. Hub registry/ledger files are never Viewer-owned.

**Required owner input:** A reviewed fixed Viewer registry entry updated for the changed coordinator/configuration source identities; installed Hub version/source/target; a normally trusted browser-reachable HTTPS Hub; exact Viewer origin allowed by Hub CORS; private disposable invitation; known current values; one 51-drive vehicle and one empty vehicle; controlled outage/restore; actual test-device revoke; a fresh invitation; and cleanup correlation.

- [ ] Verify the production Viewer is already serving from the selected static CLI or container origin. `TESLATLAS_VIEWER_EXTERNAL_SERVER=1` must prevent Playwright from starting a dev server.
- [ ] Run `TESLATLAS_VIEWER_EXTERNAL_SERVER=1 npm run test:e2e:installed:hub` through the ordinary root UI route. Verify editable wrong-UUID recovery, successful pairing, known current values, `25/25/1` paging, empty and terminal history, conditional requests, unsupported resources with zero requests, refresh, and local logout.
- [ ] Keep normal certificate validation enabled. Record browser identity, Viewer origin, Hub origin, profile/archive identity, status/ETag presence, and bounded seed counts. Persist no secret, raw cursor, or full claim response.
- [ ] In the same disposable browser session, have Hub perform the controlled outage/restore and exact test-device revocation. Verify retained stale data, recovery, next authenticated 401, complete UI/session clearing, and successful pairing with a newly issued invitation.
- [ ] Run the managed `test:e2e:hub` lane only if changed code affects it. Keep its credential-rotation evidence supplemental; it does not substitute for actual operator revocation or installed-service proof.
- [ ] Give the redacted Viewer result to Hub for its existing platform matrix. Keep `compatibility/hub.json` at `candidate` until Hub accepts the required installed rows and authorizes broader compatibility status.

**Exit:** The exact production Viewer and SDK complete the ordinary user route against an identified installed Hub with real TLS/CORS, paging, outage recovery, operator revocation, and fresh re-pairing. Other browsers or Hub platforms remain unclaimed until separately exercised.

## Milestone 5 — Validate the local Docker runtime

**Status: WAITING FOR OWNER/RESOURCE — local Docker daemon/Compose**

**Files if runtime findings require changes:** `Dockerfile`, `.dockerignore`, `compose.yaml`, `bin/teslatlas-viewer.mjs`, `README.md`, and `docs/verification.md`.

- [ ] Run `docker compose config` with the supported local Compose command.
- [ ] Run `docker compose up --build -d` for this project only, after the source-only `sdk:prepare` command has generated its ignored local archive. Verify the image builds without secrets or private evidence in layers.
- [ ] Probe `/`, referenced JavaScript/CSS, `/version.json`, extensionless fallback, missing assets, and method rejection through `127.0.0.1:4173`.
- [ ] Verify the runtime process is non-root, has no `node_modules`, needs no volume, handles restart, and stops cleanly with `docker compose down` for this project.
- [ ] If this origin is used for Milestone 4, rerun only the installed smoke needed to establish container-served provenance and CORS origin behavior. Keep images local and record the actual architecture.

**Exit:** One local image and Compose service run the same production Viewer with documented networking, TLS ownership, lifecycle, and no persistence. A parsed Compose file alone does not satisfy this milestone.

## Milestone 6 — Final review and authorized source-only GitHub update

**Status: WAITING FOR OWNER/RESOURCE — Milestones 4–5**

**Files:** Only reviewed Viewer source and documentation. Exclude local package archives, generated bundles, screenshots, private receipts, credentials, caches, and test artifacts from the outgoing source-only commit.

- [ ] Compare the final diff with this plan and the acceptance criteria below. Record every passed, failed, blocked, and untested lane separately. Review approval cannot replace missing installed/browser/Docker evidence.
- [ ] Confirm a clean source checkout can obtain or reproduce the exact accepted SDK dependency under the source-only rule. If it cannot, keep publication pending; do not push metadata that references unavailable bytes.
- [ ] Immediately before Git writes, verify `main`, HEAD, remote URL, remote tip, staged paths, and the complete outgoing commit range. Any unrelated or forbidden outgoing content blocks the push without authorizing reset, history rewrite, or force push.
- [ ] Stage explicit Viewer source/docs paths only. Never use `git add .`. Inspect the staged binary list and ensure the replacement SDK tarball and private/local artifacts are absent.
- [ ] Commit the reviewed Viewer source/docs change set and push the verified `main` branch under the user's existing source-only authorization. Do not create CI, tags, releases, registry artifacts, downloadable binaries, or a force push.

**Exit:** The reviewed source/docs update is stored on the verified Viewer GitHub repository, and the local/installed/Docker evidence accurately supports every published claim.

## Full product acceptance criteria

Full product acceptance remains pending until all of these are true:

- [ ] `npm run test:cli` passes through its normal source/package path without accepting a nonzero pack result or hiding the measured Vite advisory.
- [x] The exact `03dddd...` SDK dependency and `b80d940e...` profile are reproducible from the approved source-only handoff and verified by Viewer.
- [ ] The production browser route works against a real installed current Hub with trusted TLS/CORS, editable identity, memory-only pairing, current values, explicit `25/25/1` paging, correct 304/cache behavior, outage recovery, actual operator revocation, and fresh-invitation re-pairing.
- [ ] The local Docker image/Compose service builds and runs as non-root, serves the verified assets, needs no persistent data, and stops cleanly.
- [ ] Documentation distinguishes fixture, local package, managed synthetic Hub, installed Hub, Docker runtime, and broader platform evidence; `compatibility/hub.json` remains candidate until Hub accepts promotion.
- [ ] The final source-only commit/push contains only reviewed Viewer source/docs and no CI, release, tag, binary artifact upload, private evidence, or unrelated work.

If Hub or Docker remains unavailable after Milestones 2 and 3 finish, keep the goal active and report the exact waiting dependency. Do not mark the whole goal complete or blocked merely because a later resource-bound milestone cannot start.
