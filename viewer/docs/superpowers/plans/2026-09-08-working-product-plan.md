# Teslatlas Viewer Working Product Development Plan

> **Status: Execution in progress (2026-09-08).** Local source, fixture, managed-lane, documentation, and packaging work has been implemented and verified. Installed-Hub acceptance remains blocked pending the Hub-owned disposable target and redacted handoff; see `docs/verification.md`.

**Goal:** Deliver a usable read-only browser Viewer connected through the built public TypeScript SDK to a real current Hub, including pairing, trusted connection setup, current data, usable drive history, recovery and revoked access.

**Architecture:** Keep React/Vite, `ViewerDataSource`, `SdkDataSource`, existing views, and the static-serving CLI. Make small corrections at those seams; all Hub transport remains in `@teslatlas/sdk/browser`. Fixture mode remains an explicitly labelled demonstration.

**Tech stack:** React 19, TypeScript, Vite, npm lockfile, Vitest, Playwright, public `hub-http-v1@1.0.0` SDK. The current product and SDK package version is `2026.36.2`; product identity is separate from wire/profile revision. Recorded successful tooling was Node `26.7.0` / npm `11.19.0`; this is a reproducible starting point, not a new minimum-version claim. The locked `jsdom@30.0.1` requires `^22.22.2 || ^24.15.0 || >=26.0.0`, so README's `22.12 or later` prerequisite is insufficient for this locked development toolchain.

**Specification and authority:** User's 2026-09-08 working-product instructions; workspace `WORKSPACE_AUTHORITY.md`; [existing ecosystem plan](../../../../docs/superpowers/plans/2026-09-05-hub-ecosystem-compatibility.md), especially Tasks 4, 5, 10 and 10a; `hub/docs/compatibility/execution-state.json`. Paths below are Viewer-relative unless prefixed with a sibling name.

## Constraints

- Work in the existing independent `main` checkout. Preserve unrelated changes and staged work. No branch/worktree/reset/clean/stash.
- This task owns Viewer files only. Hub coordinates shared compatibility state, installed-host operations and ecosystem documentation. Teslatlas App is excluded from development and all AGENTS changes.
- No builds, tests, installs, Docker/VM/service operations, implementation agents, commits or pushes during planning. All unchecked steps below are future work.
- Keep read-only product scope: health/readiness, vehicles, current state and drives. Pairing is the required access setup; local logout does not revoke a Hub device. No vehicle commands, private control socket, remote device-management implementation, proprietary analytics or richer-profile invention.
- Keep credentials and invitations in memory; never put secrets in URLs, static assets, image layers, logs or browser storage. Browser CA trust, Hub UUID and invitation TLS identity are distinct.
- Reuse meaningful existing tests and acceptance tooling. Fix environment setup directly; do not build a replacement generic validation framework. No CI, releases, tags, binaries or artifact uploads. Source/docs Git update is the final execution step only.
- Dependency and metadata checks are local; do not require all sibling products to finish before implementing Viewer. Protocol/SDK handoff and a usable installed Hub are the relevant dependencies. Swift, Home Assistant and Edge are not Viewer runtime dependencies.

## Current evidence and remaining gaps

Inspected on 2026-09-08 without running verification. Viewer is on `main`, HEAD `a078f77` (`Connect the viewer to live Hub via the TypeScript SDK`), and `git status --porcelain=v1` returned no changes before this plan. The supplied dirty-worktree description is historical for this checkout; recheck at execution and preserve any new edits.

| Evidence level | Existing work | Limit or remaining gap |
| --- | --- | --- |
| Source inspected | `src/data/sdk-data-source.ts` uses public browser SDK for discovery, pairing, health/readiness, vehicles/current and drives; handles null/zero, partial failures, aborts, session clearing and unsupported resources. | Do not reimplement it. Ordinary connection entry and visible paging still need work. |
| Source inspected | `src/main.tsx` defaults to a paired fixture. Live mode requires `?mode=live`; `initialPaired` still defaults true unless `paired=false`. | A normal user cannot enter live setup from the default page; live URL without the second flag starts incorrectly paired. |
| Source inspected | `readDrivePages` uses limit 2 and at most 3 pages per vehicle; `RecentSessionsView` shows a flat list with no continuation control. | More than six drives can be silently inaccessible. Expose bounded paging and end-of-history rather than implying completeness. |
| Recorded local gates | Execution state reports 52 unit tests, 2 artifact tests, 11 fixture E2E tests for Task 5. Later reconciliation records install/typecheck/build/artifact/CLI gates. | Historical reports, not tests run during this planning session; do not infer current passing status. |
| Recorded real browser/process proof | Task 5 fix2 review approves built Viewer against native synthetic Hub with normal CA trust, mixed-resource recovery, three drive pages, per-page 304s, logout and authenticated 401. | Synthetic data served by an actual Hub is stronger than fixture E2E, but does not establish the final installed cross-platform matrix. |
| Recorded dependency reconciliation | Task 10a reconciliation review approves the 80-member SDK archive; current metadata pins SHA-256 `d1ab6ba0ede3a24ae12ed4151db0c90bf957fa19f5640cc4323bd368e565e8bb`. | Review explicitly says no new live browser acceptance for that reconciled digest. Its runtime equality is bounded historical evidence. |
| Installed acceptance | CLI package HTTP/asset/containment/shutdown work is recorded as approved. Compatibility metadata remains `candidate`, with empty tested-Hub and receipt arrays. | Shared Task 10 matrix remains pending; Task 10 Viewer adapter brief is prepared, not dispatched. No installed compatibility promotion is justified yet. |

Relevant report directory: `hub/.superpowers/sdd/2026-09-05-hub-ecosystem-compatibility/`. Execution should read `task-5-report.md`, `task-5-review.md`, `task-5-fix-2-review.md`, `task-10a-viewer-fix-1-review.md`, `task-10a-viewer-sdk-reconcile-report.md`, `task-10a-viewer-sdk-reconcile-review.md`, and the current Hub-owned installed acceptance handoff. Do not revive superseded tarball pins from older reports.

Open findings carried forward from Task 5 review:

- **M1:** Connection inputs disappear after inspection; a mistaken endpoint/UUID needs reload to correct.
- **M2:** Accepted invitation TLS identity is not retained for later display when the optional input was blank; `completePairing` ignores its result.
- **M3:** Large generated JS chunk (roughly 1 MB in recorded runs) and optional installation notices need explicit triage, not a claim of warning-free output.
- Task 5 fix2 closes the mixed-result Important finding; its review explicitly leaves M1–M3 for final triage. These are not newly proven runtime failures in this session.

Documentation drift already visible: SDK roadmap says 78 installed members while metadata says 80; browser documentation says two pages where the final correction review records three. README describes capabilities more broadly than the live profile supplies, and its live command depends on private runtime setup. No Docker solution was present in the inspected file inventory.

### Detailed second review, 2026-09-08

The review checked the actual component, hook, data-source, SDK, CORS, package and browser-test code. It did not execute any of them. Only this plan is changed. These corrections make the first draft executable without expanding the product architecture:

| Plan issue | Source evidence | Correction in this revision |
| --- | --- | --- |
| Auth loss could be mistaken for actual revocation acceptance. | `e2e/live-hub.spec.ts` uses `rotator.rotateDevice()` for its `auth-loss` scenario. Its logout lane deliberately reuses a spent invitation and expects rejection. | Keep those assertions, but separately require real operator revocation and successful re-pairing with a newly issued invitation. |
| First draft retained the test-sized two-row page and underspecified state merging. | `DRIVE_PAGE_LIMIT = 2`; `useViewer` currently has one snapshot effect and no paging operation state. | Use 25 rows per production request, define paging state and serialize refresh/page operations so old full snapshots cannot overwrite newer state. |
| `hasMore: false` alone could falsely mean end-of-history when the first read failed. | Resources have aggregate availability; `RecentSessionsView` does not have an unsupported-drives branch and can show “No recent drives” when `query.drives` is absent. | Per-vehicle paging availability and `hasMore: null` represent unknown state; render unsupported, unavailable, empty and terminal separately. |
| Late operations could outlive an edit or logout. | Pair submit passes no abort signal; `readDrivePages` writes its cache before the parent snapshot's final generation check. | Guard pairing completion, credential callbacks and every cache commit; add late-response regressions without redesigning the SDK. |
| Recovery actions were specified only for the successful dashboard. | `Clear local session` is in the ready-state sidebar; loading/error screens have no equivalent exit. | Keep a local disconnect/change-connection action available during loading and errors, with pending work cancelled. |
| Clean setup and container build details were too loose. | All build libraries are devDependencies; postinstall needs `bin/verify-source-install.mjs`, `scripts/verify-sdk-artifact.mjs` and artifact metadata; that verifier invokes `tar`. | State the Node floor mismatch, include development dependencies and verification inputs in the build stage, and keep the runtime static and dependency-free. |
| Acceptance setup was described as if current variables were optional. | The existing live test requires CDP witnesses, a fixture descriptor and SSH-forward variables, kills its forward and starts a replacement. | Separate the existing managed test environment from an ordinary-user browser route; remove those requirements from the normal product path only through a bounded test configuration change. |
| Single-target product proof risked implying ecosystem compatibility promotion. | `docs/product-versioning.md` defers the tested-Hub arrays until the ecosystem matrix is accepted. | Record a working-product result separately; leave cohort compatibility metadata pending until Hub closes the existing matrix policy. |

### Dependencies and explicit blockers

| Owner / input | Needed for | Current status and fallback |
| --- | --- | --- |
| Viewer: exact npm toolchain and verified existing SDK artifact | Baseline, all source changes and Docker build | Archive and lock exist; installation was not checked this run. Use the recorded working toolchain first. A missing or rejected dependency blocks its dependent checks, not read-only planning. |
| TypeScript SDK: accepted package built from current agreed source, browser export and lock-compatible archive identity | Any SDK replacement and real current-Hub reads | Existing reconciled archive is the starting point. If a new SDK is required, owner delivers a local pack and source identity; Viewer updates only its own bindings. Never add a direct sibling source import or bypass validation. |
| Protocol + Hub: agreed current profile and actual discovery/query behavior | Interpreting capabilities, schemas, errors and history | Existing profile is `hub-http-v1@1.0.0`; recheck only relevant drift. Contract changes go to their owners. Viewer does not extend a wire contract by changing fixtures. |
| Hub: installed test target, browser-reachable trusted origin, `[http].allowed_origins`, scoped disposable invitations and known data | Core Task 5 | No target availability or trust setup was checked live in this run. Reuse known private configuration; Hub confirms ownership/availability. Missing setup is an acceptance blocker, not a reason to invent a new runner. |
| Hub: a test vehicle with at least 51 ordered drive records plus an empty vehicle; test-device revoke and fresh invitation operations | Three real UI pages at limit 25, empty state, revocation/re-pairing | Existing test expects five drives (`105` through `101`) and cannot prove the new page size. Hub must supply a dedicated seed through its existing tools; do not rewrite shared conformance fixtures from Viewer. |
| Hub coordinator: final three-platform receipts and coverage policy | Cohort compatibility promotion | Pending in execution state. A local working product can be recorded; broader claims and completion of the inherited matrix remain pending. |
| Local Docker engine and an agreed supported Node base image | Final phase 2 | Not inspected or operated during planning. Resolve ordinary setup in that phase, validate locally, and leave unsupported architectures unclaimed. |

Source-only distribution has a specific conditional dependency: the existing SDK tarball is already tracked. Do not add a replacement binary to the final push. If the accepted SDK stays unchanged, no new artifact workflow is needed. If it changes, the SDK/Hub source-bootstrap handoff must reproducibly create the required local tarball before `npm ci`, while Viewer records the exact source/version/hash binding. A bootstrap change belongs to its owner. Do not claim reproducible clean installation until this case is resolved and exercised.

## Working-product acceptance criteria

- [ ] Opening `/` offers live Hub connection and a clearly labelled demo, without requiring URL knowledge. An unpaired live page always opens setup.
- [ ] A user can enter HTTPS endpoint and expected UUID, inspect the Hub, correct mistakes without reload, submit a valid invitation, and see their actual Hub identity. Invalid/expired invitations and wrong identity leave a recoverable form.
- [ ] Normal browser certificate verification remains enabled. Untrusted certificates fail; identity mismatch never becomes a successful paired state. TLS failure copy does not pretend JavaScript can always distinguish CORS from network/certificate errors.
- [ ] Current data, vehicles and drive history reflect real responses. Zero, null, unknown freshness, empty results, loading, partial failure and retained stale values remain distinct. Unsupported charges/quality/remote device features cause no requests.
- [ ] Users can request another page of at most 25 drives per vehicle and reach a visible terminal state; cursors remain opaque and vehicle-bound, with no duplicates, infinite retry or silent truncation. Unknown continuation after a failure is not terminal. Refresh and 304 replay preserve the correct page/query cache.
- [ ] Temporary outage retains stale data with a usable retry; recovery refreshes it. Logout, endpoint change and authenticated 401 clear credentials, retained values and pagination state; late replies cannot restore them. Revocation is exercised by the Hub operator on the test device only.
- [ ] The production static Viewer, using the verified built SDK dependency, completes the ordinary UI route against a real installed current Hub with actual requests. Record supported browser/Hub target and unresolved targets honestly; fixture-only or Vite-preview-only evidence cannot close this criterion.
- [ ] Keyboard entry, focus after errors, readable status messages and existing mobile/reflow accessibility behavior survive the changes.
- [ ] Initial reads can be cancelled through a visible disconnect/change-connection action. Refresh/page failures never trap the user on an uneditable screen. No automatic retry of a single-use pairing claim is introduced.

The ordinary product supports a root-hosted Viewer (`/`) and a root HTTPS Hub origin. Subpath hosting, endpoint paths, mDNS/LAN scanning, automatic pairing persistence, background refresh scheduling, search/export, drive details and new charge/history APIs are outside this milestone. Current values are as of the last successful manual refresh; revocation is detected on the next authenticated operation, not pushed instantaneously to an idle browser.

## Ordered core implementation tasks

### 1. Reconfirm the baseline and dependency handoff

**Files:** Read `package.json`, `package-lock.json`, `artifacts/teslatlas-sdk.json`, `scripts/verify-sdk-artifact.mjs`, `bin/verify-source-install.mjs`, `compatibility/hub.json`; modify dependency bindings only if the SDK owner delivers a changed accepted artifact. `package.json` may gain an honest `engines` declaration for the selected supported tooling; do not upgrade dependencies merely to satisfy the obsolete README floor.

- [ ] Recheck branch, status and source identity. Inspect current reports before using historical counts or approval. Note task-owned paths without copying entire repositories or building new manifests.
- [ ] Obtain TypeScript SDK owner's current build/pack identity and protocol owner's current profile identity. Use the existing verified artifact path; no absolute sibling source imports, bypassed verifier or unrequested registry publication.
- [ ] Once execution is authorized, inspect `node --version`, `npm --version` and availability of `tar`. Start with the recorded Node `26.7.0` / npm `11.19.0` if available; otherwise choose a toolchain compatible with every locked engine requirement and record it. Do not advertise all Node versions above `22.12` as supported.
- [ ] Run `npm ci --include=dev`, `npm run typecheck`, `npm test -- --run`, and `npm run test:sdk-artifact` once for the baseline. Their existing lifecycle hooks perform SDK verification; run `npm run sdk:verify` separately only to diagnose an artifact problem. All current app/build libraries are devDependencies, so source builds must not use `--omit=dev`. Report actual outcomes. Resolve installation errors directly before adding harness work.
- [ ] Reconcile the source-only delivery requirement with the existing tracked generated SDK tarball: retain the current binding for local work; if it must change, arrange a reproducible local pack handoff with the SDK owner, without uploading a new tarball as part of this task. Do not silently break clean-source installation.

**Exit:** Known baseline and a working verified public SDK dependency. SDK transport/schema defects belong to the SDK/protocol owners; no duplicate transport in Viewer.

**Order:** Task 1 precedes code changes. Tasks 2–4 share files and run sequentially. Hub can prepare the Task 5 target independently after a future start instruction; no preparation service is started by this plan.

### 2. Make live setup reachable and editable; close M1/M2

**Files:** `src/main.tsx`, `src/app/App.tsx`, `src/components/connection-panel.tsx`, `src/data/sdk-data-source.ts`, `src/app/App.test.tsx`, `src/data/sdk-data-source.test.ts`; create `src/components/connection-panel.test.tsx` if component coverage is clearer there; update `e2e/viewer.spec.ts`.

- [ ] Keep the existing labelled demo as the default `/` view and add a prominent “Connect to my Hub” link to `/?mode=live`. Add an explicit “View demo” link to `/?mode=fixture` from live setup. This preserves fixture routes and avoids a new landing-page/router subsystem. Compute initial pairing as `mode === 'fixture' && search.get('paired') !== 'false'`; also enforce this in App's initial state so a caller cannot initialize a live App as paired. Test both `?mode=live` and `?mode=live&paired=true`.
- [ ] Validate the input as a root HTTPS Hub origin. Accept an optional trailing slash and trim surrounding whitespace; reject embedded username/password, non-root path, query and fragment with editable form feedback. `normaliseEndpoint` currently silently strips query/fragment, while the SDK's public endpoint parser rejects them. Align the input boundary with the SDK rather than sending a changed endpoint behind the user's back.
- [ ] Add “Edit connection” from discovery results, in-progress discovery and failures. Abort the previous inspection/claim, clear obsolete invitation/error/selection state, return to the form with non-secret connection values retained, then call existing `configure` and `discover` on resubmit. Merely setting the visual phase to `idle` does not stop the current effect: explicitly invalidate its attempt/controller. Guard against old discovery or claim results arriving after edit or unmount, including same-endpoint resubmission.
- [ ] Pass an abort signal into `dataSource.pair`; cancel it on edit/disconnect/unmount and suppress stale `onPaired`. Keep a session-generation check around credential-store callbacks in `SdkDataSource.requireClient` so a disposed client's late save/clear cannot affect a new client. Use the existing generation mechanism, not another credential store. If the SDK itself violates cancellation, send a minimal reproduction to its owner instead of copying its transport.
- [ ] Retain accepted invitation TLS identity as session display metadata in `SdkDataSource`; clear it on reconfiguration/logout. Do not overwrite the distinction between expected input and browser certificate trust or use a displayed pin as certificate verification.
- [ ] Add regressions: `/` offers live setup; live URLs do not call authenticated reads before pairing; wrong UUID can be corrected without reload; endpoint syntax errors stay editable; late old discovery/claim cannot replace new results; blank optional TLS input displays the successful invitation identity; malformed, expired, reused and mismatched invitations allow a new submission. Use a new invitation after a successful or uncertain claim; no automatic retry of a consumed secret.
- [ ] Run affected component/data-source tests, typecheck and the affected fixture browser route. Keep actual TLS/claim acceptance for Task 5 below.

**Accessible outcome:** Focus the first invalid field on validation failure, the connection form after edit, and the main view heading after successful pairing. Announce discovery and pairing progress. Do not expose the raw invitation in error copy or status announcements.

**Exit:** Ordinary setup works without hidden flags, reload recovery or misleading identity copy.

### 3. Expose bounded drive history navigation

**Files:** `src/data/types.ts`, `src/data/sdk-data-source.ts`, `src/data/fixture-data-source.ts`, `src/data/fixture-data.ts`, `src/app/use-viewer.ts`, `src/app/App.tsx`, `src/views/recent-sessions-view.tsx`; `src/data/sdk-data-source.test.ts`, `src/data/fixture-data-source.test.ts`, `src/app/use-viewer.test.tsx`, `src/app/App.test.tsx`, `src/views/views.test.tsx`. Update all existing test doubles implementing `ViewerDataSource` and snapshot factories in the same change.

**Internal interface to add in `src/data/types.ts`:**

```ts
export interface DrivePagingState {
  resource: ResourceState;
  hasMore: boolean | null; // null: no confirmed continuation yet
  loadedCount: number;    // count of unique displayed drives for this vehicle
}
// Add to HubSnapshot:
// drivePaging: Record<string, DrivePagingState>;
// Add to ViewerDataSource:
// loadMoreDrives(vehicleId: string, signal?: AbortSignal): Promise<HubSnapshot>;
```

The full-snapshot result reuses the existing display model. It is safe only with the serialized operation rules below. `useViewer` owns `loadMoreDrives(vehicleId): Promise<void>`, `loadingDriveVehicleId: string | null`, and `drivePageError: { vehicleId: string; message: string } | null`; views receive those through App and never call the SDK directly. Keep actual cursor strings in `SdkDataSource` only. No public SDK signature changes are needed: consume `HubClient.drives(vehicleId, { limit, cursor?, ifNoneMatch?, signal })` and its `page` / `notModified` union.

- [ ] Replace eager three-page truncation with one initial page of 25 records per vehicle and one additional SDK call per user action. Omit `fromMs`/`toMs` consistently for this unfiltered recent-history flow; Hub already has stable default bounds. Do not synthesize a changing `Date.now()` bound between page requests. Preserve Hub's newest-first order and render vehicle-specific groups with unique composite React keys `(vehicleId, drive.id)`.
- [ ] Show “Load more drives for [vehicle]”, that vehicle's unique loaded count, and loading/retry feedback. First successful empty page shows “No recent drives”; successful terminal page shows “End of available history”. `hasMore: null` cannot show an end marker. Missing `query.drives` shows unsupported copy and no paging action or drive request. A failed vehicle's notice must coexist with another vehicle's successful data. Do not add date filters, infinite scrolling or eager background page fetches.
- [ ] Keep one hook-owned refresh/page operation active at a time. Acquire the in-flight guard synchronously (a ref, not only asynchronous React state), disable all paging and refresh buttons while that operation runs, and release it in `finally`. Disconnect remains available. Double clicks must not send duplicate cursor requests. On disabled/unmounted/source-changed hook state, abort the active controller and invalidate its generation; an older promise must never publish a full snapshot into a new session.
- [ ] Bind cache entries to vehicle, endpoint/session generation, fixed limit and query. Build candidate pages locally and check abort/session generation before writing `driveCache` or `previousSnapshot`; the current inner-cache-before-parent-check sequence must not repopulate a cleared session. `loadMoreDrives` updates only the requested vehicle's drive slice and its paging state while preserving all other snapshot resources.
- [ ] Refresh revalidates every loaded page in order, even if page one returns 304; retain later pages only while their request cursor still matches the newly confirmed chain. If page one changes its next cursor, discard the obsolete suffix and rebuild at most the previously loaded number of pages using the new chain. Do not combine old suffix data with a new prefix. Commit a vehicle's refreshed window atomically; on failure retain its previous labelled window and retry through refresh. Prune removed vehicles' data/cache only when a successful vehicle-list response omits them, never because the list temporarily failed.
- [ ] On load-more transport/503 failure, keep the loaded window and cursor available for explicit retry; show a vehicle-specific page error rather than marking unrelated current/health data stale. A 304 requires a matching retained page/ETag and must never be sent for an uncached new page. A repeated/cyclic next cursor, or `invalid_cursor`/cursor-binding failure, stops continuation with “History changed; refresh to continue” and disables load-more until refresh succeeds. Do not fabricate cursors or loop automatically. Deduplicate any repeated `(vehicleId, id)` entries while preserving first-seen order.
- [ ] A page 401 uses the same terminal `AUTH_LOST` path as a snapshot 401; clear the full session and all paging state. A 403/404/capability error is not automatically revocation: show the SDK's bounded resource error, avoid successful-empty copy, and offer refresh. On refresh, a confirmed removed vehicle disappears with its cache. User logout and endpoint change clear everything regardless of resource outcome.
- [ ] Give `FixtureDataSource` the same method and state shape. Retain the existing default demo content and screenshot expectations where possible; use a dedicated larger in-memory dataset in the pagination tests. `createFixtureSnapshot` supplies paging state for every fixture scenario; changing scenario/logout resets fixture continuation. No fixture method makes network calls.
- [ ] Add focused regression cases: 51 records yield pages `25/25/1`; two vehicles may reuse a drive ID without cross-removal; terminal null; first-page failure; second-page failure/retry; unsupported drives; matching 304; unchanged first page with changed second page; changed cursor chain; repeated cursor; invalid cursor; refresh/load-more interleaving; duplicate click; vehicle removal; logout during a late response. Assert actual fake-client arguments and exact visible counts/content, not only snapshot hashes. Reuse `sdk-data-source.test.ts`'s existing HubClient fixtures and deferred-promise hook tests.

**Future validation commands:**

```sh
npm test -- --run src/data/sdk-data-source.test.ts src/data/fixture-data-source.test.ts src/app/use-viewer.test.tsx src/app/App.test.tsx src/views/views.test.tsx
npm run typecheck
```

The existing three-page tests must be rewritten to click/call continuation rather than removed. Their unchanged-first-page/changed-second-page protection remains mandatory. The larger real test seed is a Hub handoff, not an excuse to weaken these assertions.

**Exit:** History has a visible bounded continuation path without copying SDK pagination or adding server APIs.

### 4. Finish recovery and review triage

**Files:** `src/app/use-viewer.ts`, `src/app/App.tsx`, `src/data/sdk-data-source.ts`, relevant views/tests; `vite.config.ts` or `src/data/create-data-source.ts` only if measured loading cost justifies a small change.

- [ ] Exercise explicit refresh/retry for first-load failure, partial-resource outage and recovered connection. Preserve successful values alongside unavailable notices; keep timestamp-derived stale/unknown labels honest. Keep “Clear local session / Change connection” reachable during loading and error as well as ready state; abort pending requests before returning to setup. No background polling service is required for the working viewer.
- [ ] Add or retain focused regressions for 401 during snapshot/page loading, clearing local session, endpoint change, and late completion. Show a clear “Access ended; pair again” explanation without retrying rejected credentials indefinitely.
- [ ] Review M3 using the production build's actual bundle report and browser loading behavior. Prefer a small existing-boundary lazy import if useful; otherwise record the non-failing advisory and measured limitation. Do not replace validators, introduce a bundling framework or suppress warnings merely to obtain clean output.
- [ ] Run the affected tests and existing accessibility/reflow suite. Record M1–M3 dispositions with evidence in the final product review; no test repetition absent a change or unresolved concern.

**Presentation checks:** An HTTP 503 readiness body is a valid not-ready response, not necessarily transport failure. A 503 data read is unavailable, a current value of null is not a successful empty vehicle list, battery `0` remains `0%`, and missing observation time remains unknown. A fresh manual refresh must not label retained values current merely because the request itself just finished. Scope remains a viewer of Hub data; no vehicle wake or collection trigger is introduced.

**Future validation:** Run the affected App/hook/data-source/view tests once after the final recovery edit. Run `npm run test:e2e -- e2e/accessibility.spec.ts` after the UI changes, including the new live form and paging controls with redacted fixture/fake data. Real invitations stay out of trace/screenshot output. An existing fixture accessibility pass alone does not cover new controls unless the cases actually visit them.

**Exit:** Recovery behavior and review findings have explicit outcomes; no product-blocking finding is silently deferred.

### 5. Accept the ordinary production UI against real installed Hub

**Files:** `e2e/live-hub.spec.ts`, `playwright.hub.config.ts`, `playwright.config.ts`, `package.json`, `e2e/live-evidence.ts`; add `e2e/installed-hub.spec.ts` for a small read/claim-only external-target smoke, and concise `docs/verification.md` for the resulting reproducible procedure and redacted acceptance summary. Change `scripts/viewer-cli.test.mjs` only if production serving changes; retain its existing containment/shutdown checks.

**Dependencies:** Hub owner supplies an installed current Hub test target, trusted reachable HTTPS endpoint, expected UUID, disposable pairing invitation, known non-sensitive data with multiple drive pages, exact Viewer origin/CORS configuration, and controlled outage/revoke operations. Never use a real user's device revocation or production service interruption as a test. Reuse existing private configuration; do not copy invitations into docs.

- [ ] After core changes, run the full relevant unit suite, `npm run test:sdk-artifact`, `npm run test:cli` and `npm run test:e2e`. `test:cli` already runs `npm pack`, whose prepack rebuilds the app; don't precede it with another identical build unless a specific check needs it. Alternatively pass an already built archive through existing `TESLATLAS_VIEWER_PACKAGE`. Preserve pack/install/GET/HEAD/asset/version/fallback/containment/shutdown checks. No archive is uploaded.
- [ ] Make `playwright.hub.config.ts` accept `TESLATLAS_VIEWER_EXTERNAL_SERVER=1`: when set, omit `webServer` and require `TESLATLAS_VIEWER_PAGE_ORIGIN` to point to the already serving production Viewer. For the existing managed lane without that setting, use `node bin/teslatlas-viewer.mjs --host 127.0.0.1 --port <configured port>` instead of Vite preview after its normal build hook. Never silently connect to an arbitrary already-running dev server.
- [ ] Add `test:e2e:installed:hub` running only `e2e/installed-hub.spec.ts` under that config; exclude this file from default fixture `playwright.config.ts`. Require external-server mode for this test. Reuse `fillConnection`/bundle verification by a small local helper extraction if useful, not a new test framework. This smoke performs ordinary UI pairing, real reads, continuation, refresh and local logout only; it must not kill forwards, spawn SSH, alter services or rotate/revoke credentials.
- [ ] The installed smoke consumes the existing private Hub descriptor and invocation environment for expected identity, invitation path, known seed values and page origin. Use `TESLATLAS_BROWSER_CDP_URL` and the existing trust witness when running in the prepared browser, or the selected local browser with normally installed CA trust; in either case record browser identity and keep certificate validation enabled. No discovery of arbitrary runtimes, new generic descriptor schema or extra broker operations are needed. Document only the variables the selected path actually consumes.
- [ ] Adapt the managed live test to start at `/` and click the visible live connection route. Keep its failure-only request aborts explicitly labelled; never fulfill a real-Hub success response. Update the five-drive/hard-coded three-auto-page expectations for the new 25-row user-driven flow. Reuse the existing descriptor/CDP/SSH-forward inputs only for this managed lane. They remain required there until its code explicitly removes them, and remain outside ordinary Viewer prerequisites.
- [ ] Open a fresh normal browser session; pair through the UI; verify identity and known current values; load history beyond six entries through UI controls; refresh and observe conditional requests; verify terminal history and unsupported screens. Correct an identity error through the form. Repeat with untrusted certificate to prove normal trust rejection.
- [ ] In one continued ordinary-browser test session on the disposable target, coordinate a brief outage and restore with Hub owner, verify stale/retry/recovery, then have Hub revoke the exact test device through its supported operator control. Viewer code must not gain that control capability. Verify the next real authenticated request returns 401 and removes current/history data; submit a newly issued invitation and verify successful pairing/read again. Record the operator's actual revoke outcome and device correlation, not just a browser error. Separately verify local logout creates zero remote requests and reload requires a new pairing because credentials are memory-only.
- [ ] Retain the managed lane's separate `logout` and `auth-loss` scenarios. Label `auth-loss` explicitly as credential rotation; it is useful regression evidence but cannot satisfy operator revocation. Keep the spent-invitation rejection, but do not report it as successful re-pairing. Missing setup, omitted cases or skipped tests are blocked/untested outcomes, never passes. Stop only test processes owned by this run; do not close shared browsers or stop existing services without the coordinated test handoff.
- [ ] Keep a compact redacted record: date; branch/source identity and dirty scope; Viewer/SDK/profile identities; actual installed Hub version/source/target; browser/version/trust mode; Viewer and Hub origins; route/status/ETag-presence assertions; independently supplied seed IDs/counts; revoke and re-pair outcomes; cleanup. In public docs summarize results and limitations. Never persist authorization values, invitation JSON, full claim response bodies or raw cursor strings; compare cursors in memory and record only hashes/lengths when needed. Live traces/screenshots remain off by default.
- [ ] Give Hub the Viewer results for its existing macOS ARM64 / Debian 13 ARM64 / Debian 13 AMD64 installed matrix. Do not implement the old prepared generic matrix adapter merely to get one working-product result, and do not waive its outstanding matrix cases. `docs/product-versioning.md` currently defers tested-Hub arrays until that matrix is accepted: keep `compatibility/hub.json` as a candidate until Hub authorizes promotion under that policy. Record the narrower working-product result in `docs/verification.md` meanwhile.

**Future smoke invocation, after Hub/Viewer/browser setup is complete:**

```sh
# The existing private environment supplies endpoint, descriptor, browser and origin.
# This test does not start servers, install dependencies or change Hub service state.
TESLATLAS_VIEWER_EXTERNAL_SERVER=1 npm run test:e2e:installed:hub
```

`test:e2e:installed:hub` is a script to be added during Task 5, not an existing command. The retained managed command `npm run test:e2e:hub` has a prebuild hook; running both managed scenarios can rebuild twice today. Avoid that by building once and invoking its locked local Playwright executable with the same config for each scenario, or add a narrowly scoped combined script. Do not change test isolation or reuse an old bundle merely to save a build.

### Task 5 acceptance cases and proof boundaries

| Case | Required observation | Primary validation |
| --- | --- | --- |
| Start and trust | `/` → visible live link → root HTTPS origin/expected UUID → discovery; wrong UUID stays editable; normal CA succeeds and untrusted CA fails | Installed ordinary UI; component negatives supplement it |
| Pairing | Real successful claim precedes protected reads; malformed/expired/spent or mismatched invitation is rejected; no secret is persisted | Installed smoke plus existing managed spent-invitation case and unit negatives |
| Present/empty/partial data | Known vehicle identity; battery zero, null value and expected current value; separate empty vehicle; partial failure never hides another vehicle's success | Installed real reads; managed abort-only lane for partial failure |
| Paging | Exactly `25/25/1` seeded rows across three user-driven pages for the 51-drive vehicle, exact order and unique IDs, explicit terminal state; empty vehicle remains independent | Installed smoke; mock-client tests for repeated/invalid cursors |
| Revalidation | Loaded pages are revalidated, including later pages when page one is 304; counts/values stay correct | Installed requests/304s plus changed-later-page unit regression |
| Recovery | Known displayed values become retained/stale through an actual target/connection outage, refresh works after restoration, and first-load failure can exit to setup | Continued installed browser session with Hub-owned outage; managed lane is supplemental |
| Revoked access | Hub confirms actual test-device revocation; next request is 401; all private values/paging disappear; a new invitation allows a new successful session | Continued installed browser session; credential rotation alone is insufficient |
| Logout/reload | Zero logout requests, cleared UI/cache, no old response restores data; reload cannot reuse credentials | Installed smoke and late-response hook tests |
| Unsupported resources | No charges, quality, collector or remote-device requests; missing drives capability is visibly unsupported | Installed network observation for absent rich routes; capability-negative unit/component case |
| Production provenance | Browser-fetched HTML/assets/version match the built/installed Viewer; SDK has the accepted packed identity; TLS/CORS is real | Existing bundle comparison reused on production server |

Use already passing results for unchanged code; this table identifies evidence coverage rather than instructing duplicate suites. Keep browser engines other than the actually exercised Chromium target pending. Installed Hub platform and browser host platform are separate facts.

**Exit:** Core product works through the production user route on an explicitly identified real installed Hub. Broader platform/browser claims remain blocked until their actual runs pass. Only then begin the final phases below.

## Final phase 1 — AGENTS guidance after the core product works

**Files:** Every `AGENTS.md` inside this Viewer repository (currently the root file was found). Hub owns workspace-level edits and checks coverage across all seven products: Hub, Protocol, TypeScript SDK, Swift SDK, Viewer, Home Assistant and Edge. No task edits any `app/` AGENTS file or changes model defaults.

- [ ] Reinventory Viewer AGENTS files at execution and reconcile the existing short Astra section rather than appending another duplicate.
- [ ] Re-read the [official GPT-6 Astra guidance](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-6-astra), opened during planning on 2026-09-08. Its prompting section discusses follow-through, instruction sensitivity, writing style, delegation and proportionate testing. Apply these as brief practical instructions: finish authorized work, ask only material questions, retain user authority, scope delegation to useful independent work, and stop testing when relevant checks suffice.
- [ ] Preserve public-SDK-only and read-only Viewer constraints. Share the resulting coverage with Hub; other product tasks update their own repositories. Check consistency without adding agent orchestration machinery or overriding the user's explicit stop boundaries.

## Final phase 2 — Simple production Docker solution

**Create:** `Dockerfile`, `.dockerignore`, `compose.yaml`. Reuse `bin/teslatlas-viewer.mjs`; no new server implementation.

- [ ] Select and validate an official Node image matching the supported build toolchain, with an explicit patch tag/digest recorded in the Dockerfile; no floating `latest`. Prefer a Debian slim variant compatible with the locked native build dependencies. Check that the build stage has `tar`, which `scripts/verify-sdk-artifact.mjs` invokes; add only that prerequisite if the selected image lacks it. This is a local validation requirement, not a claim that a particular uninspected image currently exists.
- [ ] Use a multi-stage build. For the initial simple Dockerfile, copy the allowed repository context into the build stage before `npm ci --include=dev`, so `postinstall` sees `package.json`, lockfile, `bin/verify-source-install.mjs`, `scripts/verify-sdk-artifact.mjs`, `artifacts/teslatlas-sdk.json` and the accepted SDK archive together. Run `npm run build` with its verification hook. Do not optimize dependency-layer caching by accidentally omitting verification inputs or disabling install scripts.
- [ ] Copy only `/build/dist`, `/build/bin/teslatlas-viewer.mjs` and `/build/package.json` into a compatible non-root Node runtime stage, preserving the CLI's expected sibling `dist`/`package.json` layout. No runtime npm install or `node_modules` is needed by this CLI. Use exec-form entrypoint `node bin/teslatlas-viewer.mjs --host 0.0.0.0 --port 4173` so container SIGTERM reaches it directly. No Vite runtime or new orchestration layer.
- [ ] Exclude `node_modules`, `dist`, `.git`, test results, coverage, screenshot output, temporary/private evidence and `.env` files from the build context. Explicitly retain the SDK archive and metadata needed by source verification. Do not copy a credentials file into a layer and delete it later. Viewer needs no Hub token, invitation, private TLS key or Hub database at image-build time.
- [ ] Provide one Compose Viewer service with a loopback host binding by default. Intended shortest command, after the source SDK artifact prerequisite is satisfied: `docker compose up --build -d`. Standalone build: `docker build -t teslatlas-viewer:local .`. Validate the final commands before documenting them as supported.
- [ ] Bind the example host port as `127.0.0.1:4173:4173` and document the loopback URL and port override. For a host-installed TLS proxy this is reachable from the proxy; a proxy in another container instead needs an explicitly documented shared network route to `viewer:4173`. Do not claim container `localhost` reaches the host or that public access works with the loopback default. Keep the one Viewer Compose file usable alongside Hub's own documented deployment; do not invent a second Hub configuration/database in this repository.
- [ ] Document use alongside an existing Hub: the browser connects directly to the Hub's browser-reachable HTTPS hostname, not its Docker service name. Endpoint/UUID are entered in UI; arbitrary container environment variables do not configure a static browser bundle. Configure Hub's actual `[http].allowed_origins` with the exact browser Viewer origin through the Hub owner. Canonical scheme/host/port must match; `localhost` and `127.0.0.1` differ, and origins have no path/trailing slash. Hub rejects wildcard and `null` origins.
- [ ] Check real browser OPTIONS for GET/POST where required, permitted `Authorization`, `Content-Type`, `If-None-Match`, `Accept`, and exposed `ETag`/`X-Request-ID`. Confirm readable CORS-decorated 401 and 304 responses, not only happy-path 200. These are Hub/reverse-proxy responsibilities; don't add permissive CORS to the static server hoping it changes Hub responses.
- [ ] Use existing TLS termination for production HTTPS; local loopback HTTP serving is a local workflow, not public TLS deployment. Keep CA trust in the browser/OS, with no ignore-certificate flags. Viewer requires no persistent data volume; credentials vanish on reload. Hub storage/backups and any TLS certificates remain owned by their existing services; do not add a second database or certificate service.
- [ ] Validate `docker compose config`, `docker compose up --build -d`, root HTML, referenced JS/CSS and `version.json`, extensionless fallback, missing-asset 404, restart and clean stop using `docker compose down` for this project only. Verify the running process is non-root, starts without an SDK source tree or dependencies, and does not require a data volume. Run the installed smoke against the container-served external origin without triggering another unrelated build, then the coordinated outage/revoke case for that browser session where the deployment changes its origin/proxy behavior. Keep images local and no registry uploads. Record the actual tested image architecture; don't claim multi-architecture success from a single build.

**Exit:** One local image and one Compose example serve the actual built app through the same successful live UI route. Startup, networking, TLS ownership and persistence needs are documented from the run. Docker is packaging after core product acceptance, not the environment prerequisite for implementing the core UI.

## Final phase 3 — Reconcile all current Viewer documentation

**Files:** `README.md`, `docs/architecture.md`, `docs/browser-support.md`, `docs/privacy.md`, `docs/product-versioning.md`, `docs/protocol-learning-guide.md`, `docs/public-sdk-integration-roadmap.md`, `docs/verification.md`; add `docs/installation.md`, `docs/configuration.md`, `docs/troubleshooting.md` only where a separate page improves the shortest README route. Review existing historical plans for contradictory current claims; preserve their historical status.

- [ ] Document actual source prerequisites, verified local SDK build/pack handoff, Node/npm support actually tested, required `tar`, ordinary live/demo entry, invitation setup, browser trust, exact CORS origin, 25-row paging, manual refresh and memory-only sessions. State that Node is required by source build/local CLI, not by the user's browser or host running a prebuilt container. Separate fixture features from supported live resources.
- [ ] Document Docker startup/build/stop, production TLS arrangement, browser-vs-container networking and no Viewer persistence requirement. Include clean-source installation without relying on unpublished artifacts or private acceptance files as public prerequisites.
- [ ] Explain invalid UUID/invitation, CORS/certificate/network failures, revoked access, empty vs unavailable data, missing SDK artifact, build warnings and blocked installed targets. Keep the API guide focused on public SDK methods; no invented endpoints.
- [ ] Correct 78-versus-80 member and two-versus-three historical evidence claims, and replace timeless “proven today” assertions with dated/versioned evidence. Keep unsupported browser/platform claims explicit. Have Hub coordinate shared ecosystem docs without this task writing siblings.
- [ ] Review every current Markdown document and relative link, including screenshots and cross-repo prerequisites. Mark obsolete plans historical and remove misleading current instructions. Validate documented commands against the completed execution; do not rerun unrelated gates solely for documentation wording.

**Documentation review map:**

| Document | Required reconciliation |
| --- | --- |
| `README.md` | Shortest actual source/Docker entry, visible demo/live choice, tested prerequisites, live unsupported list, links to trust/setup and evidence limitations |
| `docs/architecture.md` | Existing public SDK boundary, serialized refresh/page ownership, per-vehicle paging/cache state and static serving; no fictional new backend |
| `docs/browser-support.md` | Dated browser/platform combinations actually run; fixture vs managed vs installed evidence; no blanket Safari/Firefox support claim |
| `docs/privacy.md` | In-memory secrets/session scope, no raw cursor or invitation capture, reload/re-pair behavior; update obsolete “no hosted deployment configuration” wording once Docker exists |
| `docs/product-versioning.md` | Existing cohort and profile rules; candidate remains pending until Hub accepts the matrix; no uncoordinated version bump |
| `docs/protocol-learning-guide.md` | Root HTTPS endpoint, exact SDK methods and supported response states; distinguish actual revocation from local logout/credential rotation |
| `docs/public-sdk-integration-roadmap.md` | Current artifact member count, completed live features and deferred richer profile functions; no claims of resource-local retry where only manual full refresh exists |
| `docs/verification.md` | Exact successful commands/environment prerequisites and redacted outcomes; required vs supplemental lanes; blocked cells and open findings |
| Existing two historical plans | Preserve historical intent; add a current-plan/status pointer only if needed to prevent accidental execution of obsolete work |

Do not add installation/configuration/troubleshooting pages solely to satisfy a document count: keep these sections in README or existing guides if concise. For cross-repo links, distinguish the local workspace path from a verified public source URL so this product's GitHub README works when cloned alone. Historical reports remain historical; update present-tense claims rather than rewriting old evidence to match new behavior.

## Final phase 4 — Final review and source-only GitHub update

**Files:** Only task-owned Viewer source and documentation changes, including this plan. No sibling or shared-state writes.

- [ ] Review the final product-specific diff against the baseline and acceptance checklist, with a fresh product reviewer after implementation if useful. Confirm M1/M2 fixes, M3 disposition, typed paging integration, cancellation/credential/cache guards, real operator revocation/re-pairing, dependency identity, real UI evidence, container evidence and documentation accuracy. Summarize passed, failed, blocked and untested checks distinctly. Review approval is not a substitute for a browser/installed run.
- [ ] Run remaining meaningful checks for changes not yet validated. Avoid repeating already passing unchanged gates. Keep generated bundles, images, SDK tarball replacements, private receipts, credentials and screenshots/artifacts out of the final source/docs upload. If artifact changes are necessary for reproducibility, resolve the source-build workflow with the SDK owner before proceeding; do not bypass the distribution restriction.
- [ ] Immediately before Git writes, verify `main`, actual remote URL, remote branch tip, current HEAD, staged diff and unrelated edits. Inspect all outgoing commits and files, not only the new working diff: an ordinary push can upload pre-existing unpushed commits or artifacts too. Any unrelated/forbidden outgoing content blocks the push until ownership/scope is resolved; do not rewrite history or force-push to conceal it.
- [ ] Stage only explicit task-owned paths/hunks; never `git add .` or include another task's edits. Preserve unrelated staged work, use a reviewed path-limited commit when required, and resolve overlapping ownership before committing. Reinspect the commit and complete outgoing diff before push. Keep local generated SDK/bundle/container artifacts out of new commits; preserve any already tracked unchanged archive without silently replacing it.
- [ ] After source/docs review and meaningful verification, commit only this product's owned change set and push its verified branch to its verified GitHub source repository as the final execution action. No force push, CI changes, release/tag creation, binaries or artifact uploads. This checkbox grants no Git-write authorization during planning; execution still requires the user's explicit start instruction.
