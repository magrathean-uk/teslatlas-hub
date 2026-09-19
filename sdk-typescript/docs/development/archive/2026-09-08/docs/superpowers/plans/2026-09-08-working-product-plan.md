# Teslatlas TypeScript SDK Working Product Development Plan

**Latest user model policy (2026-09-08):** This coordinator and delegation use `gpt-6-astra` with `thinking=high`. This product task, coding, goal execution and all development workers use `gpt-5.6-terra` with `thinking=high`. This supersedes every earlier model instruction in this plan and its historical goal snapshot. Preserve checkpoints at model transitions and verify the actual new turn model.

> **For executing agents:** REQUIRED SUB-SKILL: use `superpowers:executing-plans` or `superpowers:subagent-driven-development` to continue the remaining milestones. Execution is already authorized. The goal spans the complete working product; a milestone boundary, a short run duration, or one waiting dependency is not goal completion while another SDK-owned milestone is `READY`.

**Checkpoint — 2026-09-08 20:47 Europe/London:** SDK `main` remains at `b1cd548fef7ddd26be2637c14bb435480166cef7` with the task-owned dirty working tree and unrelated `.DS_Store` preserved. The canonical current-Hub profile is `hub-http-v1@1.0.0` SHA-256 `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`. The final local 81-member SDK archive is version `2026.36.2`, SHA-256 `03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c`, with installed-content manifest `4b1a44f7d2cd0ab6c772642125358703aa25c1efe32d164a119a6fc9d858ded6`. Exact Node `26.7.0`/npm `11.19.0` verification passed without a developer-engine override: 271 unit, 25 conformance and 52 protocol tests, strict clean consumers, identical clean/stale packs, and contract source gate 6/6. Hub binds the exact archive and current SDK manifest/validator hashes; its focused installed/registry source checks pass 11/11. Installed capability is still false: all three hosts are available but stopped with `package_installed=false` and `registration_ready=false`, no installed session has run, and the Docker daemon is unavailable. No commit, push, publication, CI, release, tag, artifact upload, service operation or vehicle command has occurred.

**Model policy:** This checkpoint refresh is planning work on `gpt-5.6-terra` with maximum reasoning. All TypeScript SDK development, debugging, verification and review agents use `gpt-5.6-luna` with maximum reasoning. The active goal has no token or time budget. Its goal-database status was `blocked` when read at 2026-09-08 18:44 Europe/London; the user's supported Resume goal control is required to restart its native loop. The parent can dispatch ordinary development now; do not create a duplicate.

**Goal — remaining end-to-end objective:** Prove the source-bound final archive with the exact supported Node `26.7.0`/npm `11.19.0` toolchain, run its public Node and real Chromium consumers through all required behaviors against installed Hub on macOS arm64 and Debian 13 amd64/arm64, validate the Docker test and consumer images, reconcile the final evidence and compatibility metadata, then complete the authorized source-only GitHub commit and push. Full acceptance requires all six installed SDK cells, normal TLS and browser CORS enforcement, independent pairing, rotation, revocation and reauthentication, exact pagination/304 behavior, outage recovery, clean teardown and artifact-bound identities with no required pending case.

**Architecture:** Retain the dedicated `createHubClient` adapter and generated `hub-http-v1@1.0.0` contract. Preserve the separate richer `createClient` protocol API. Use the existing package checks and Hub-owned fixed `typescript_node`/`typescript_browser` installed adapters; SDK code owns public behavior and artifact identity, while Hub owns installed targets, private session inputs, lifecycle controls and aggregate receipts.

**Tech stack:** TypeScript 5.9.3, Node 26.7.0, npm 11.19.0, Fetch, Ajv generated validators, Vitest, Playwright/Chromium, npm tarballs and a simple multi-stage Dockerfile.

**Specification and authority:** User's 2026-09-08 planning, execution and goal-continuity requests; workspace `/Users/bolyki/dev/source/teslatlas-service/WORKSPACE_AUTHORITY.md`; workspace `docs/superpowers/plans/2026-09-05-hub-ecosystem-compatibility.md` (Tasks 3–5 and 10); Hub ledger `hub/docs/compatibility/execution-state.json`; SDK evidence `docs/acceptance.md`; goal checkpoint `docs/superpowers/plans/2026-09-08-goal-checkpoint.json`. Paths in tasks are relative to this SDK repository unless explicitly prefixed with another repository.

## Current continuation control

Planning and coordination now use `gpt-5.6-terra` with `thinking=max`; subsequent product development remains `gpt-5.6-luna` with `thinking=max`. The native goal remains blocked. With the available tools, resumption requires the user's supported Resume goal control. The parent can dispatch ordinary authorized development turns now; this does not activate the native goal. Do not replace an unfinished goal, mark it falsely complete, or edit internal app state. Continue READY owned work without waiting for native resumption; retain the full proposed objective and acceptance gates.

## Milestone checkpoint

| Milestone | Status | Exact owned files and interfaces | Evidence or completion gate |
| --- | --- | --- | --- |
| M0 — Source, package and handoff | DONE | `src/hub/client.ts`, `src/hub/models.ts`, generated `hub-http-v1` files, `examples/hub/*`, `tools/matrix-contract-*.json`, `tools/matrix_contract_*.py`, public docs and package surface | Final archive `03dddd…`, SDK contract 6/6, Hub source binding 11/11, local verify 271/25/52. This is source/package evidence, not installed acceptance. |
| M1 — Exact supported-toolchain reproducibility | DONE | `package.json#devEngines`, `.npmrc`, `package-lock.json`, `scripts/check-pack.mjs`, `scripts/test-clean-package.mjs`, `docs/acceptance.md`; no production source change unless exact Node exposes a defect | Node `26.7.0` and npm `11.19.0` completed clean install, full verify, pack and strict external Node/browser consumers without `--force`; final archive and member/runtime hashes are bound for future installed runs. |
| M2 — Six installed Node/Chromium cells | WAITING FOR OWNER/RESOURCE | SDK public exports `@teslatlas/sdk/node` and `@teslatlas/sdk/browser`, `examples/hub/*`, `docs/acceptance.md`; Hub owns `tools/interop/matrix_runner/typescript_installed.py`, fixed registry, private SessionInput and host lifecycle | Hub must review/install/register current packages, provide endpoint/Hub UUID/CA/origin/invitations/revocation and runner invocation, then all 21 Node and 23 browser cases must pass on each of macOS arm64, Debian 13 amd64 and Debian 13 arm64 with clean close/stop receipts. |
| M3 — Docker runtime | WAITING FOR OWNER/RESOURCE | `Dockerfile`, `.dockerignore`, `docs/docker.md`, `examples/hub/node.mjs` | When `docker version` reports a server, build the `test` and `consumer` targets from a clean context, run full verification in `test`, then run the packed Node consumer against one accepted installed Hub using private read-only inputs and normal CA validation. |
| M4 — Final evidence and source-only publication | WAITING FOR OWNER/RESOURCE | `docs/acceptance.md`, `compatibility/hub.json`, this plan, task-owned source/tests/docs; Git remote `https://github.com/magrathean-uk/teslatlas-sdk-typescript.git` on `main` | After M1–M3 and six-cell acceptance, reconcile exact final hashes and candidate metadata with Hub, review/stage only task-owned files, commit and push source, then verify remote commit identity. No CI, release/tag, registry publication or artifact upload. |

Full product acceptance remains **false** until M1–M4 pass. A `DONE` source milestone does not satisfy an installed cell, and an installed cell does not authorize a broader ecosystem or live-service claim.

## Dependency and continuation table

| Owner task ID | Exact missing input | Work still possible locally | Event that unblocks dependent work |
| --- | --- | --- | --- |
| SDK goal `01a07f89-4bdf-7890-8a1b-4d88ad49f2a8` | None for M1: Node `26.7.0`/npm `11.19.0` verification and final package binding are complete | Retain the exact archive and source/package evidence; do not repeat unrelated green suites | A Hub owner supplies a reviewed installed target and private runner input bound to `03dddd…` |
| Hub task `01a07f89-45cb-7ea2-b96e-aa89de18fd14` | macOS package lifecycle review and install/register/session; Debian amd64 current package install/register/session; a fresh reviewed Debian arm64 package and quarantine-safe recovery; per-target endpoint, Hub UUID, trusted CA/browser origin, independent invitations/revocation and fixed-runner private SessionInput | Finish M1 and review the final SDK diff/evidence; do not simulate installed rows or mutate Hub-owned state | Hub ledger marks a concrete target installed and registered and supplies a runner-owned invocation/session input bound to the final SDK archive and pinned runtime |
| Parent coordinator `01a07048-a05c-72a2-9c7a-87dd59f52e9b` / local Docker resource | A reachable Docker server; current read-only probe recorded no daemon | No further Docker scaffolding is useful; M1 and evidence review remain independent | `docker version` succeeds with both client and server, and the accepted Hub endpoint is reachable from the container with normal trust |
| SDK goal plus Hub task above | Exact accepted final archive identity, all six installed receipts, Docker receipts and compatible candidate metadata | Keep final diff and explicit file ownership current; publication preparation may proceed read-only | M1–M3 are accepted, Hub confirms the final digest/metadata, and no overlapping dirty-tree changes require reconciliation |

## Constraints

- The planning-only restriction ended when the user explicitly started execution. Implementation, builds, tests and local dependency use are authorized within this SDK checkout; installed services, Docker/VM operations and GitHub writes remain gated by the ordered plan and their stated prerequisites.
- Future work remains in the existing independent `main` checkout. Preserve unrelated changes; no branches, worktrees, reset, clean or stash. Reinspect status before execution because other work may change the baseline.
- This task owns SDK files only. Hub coordinates shared state, host lifecycle, shared ecosystem docs and workspace AGENTS coverage. App development and every App AGENTS.md are excluded.
- GitHub is source storage only. No CI, release, tag, registry publication, binaries, tarball uploads or other artifact uploads. Commit/push is the final future execution phase only.
- Credentials remain caller-owned. Do not retain secrets in source, examples, browser bundles, images, logs or public receipts. Local logout is not server revocation.
- A passed local check or native user-process test is not installed-service acceptance. No generic new validation framework to work around missing runtime prerequisites.

## Current evidence and remaining gaps

Execution began from SDK `main` at `b1cd548` (`Add Hub client, validators, and protocol profile bindings`) with an unrelated `.DS_Store` preserved. The task-owned working tree now contains the lifecycle/bounded-body fixes, minimal current-Hub consumer and acceptance record. Do not recreate or overwrite unrelated work. Fresh local verification is recorded in `docs/acceptance.md`; installed Hub cells remain blocked.

| Area | Evidence read | What remains |
| --- | --- | --- |
| Current-Hub source | `src/hub/client.ts`, `models.ts`, `validate.ts`; `src/node.ts`, `src/browser.ts`, `src/index.ts` | Lifecycle generation, serialized credential mutations, explicit-discovery invalidation and bounded body reading are implemented and covered locally. Keep installed transport proof pending. |
| Package | `package.json`, build/package scripts; Hub report `task-4-clean-build-review.md` | Earlier 81-member candidates are historical. The final local 81-member candidate is `03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c`; Hub now binds that exact digest and installed-content manifest. Installed receipts must use it. |
| Local validation | Final `npm run verify` on this working tree under Node 26.7.0/npm 11.19.0: 271 unit, 25 conformance and 52 protocol tests, package checks and clean/stale archive comparison | Final local digest, changed-member comparison and packed-consumer witness are in `docs/acceptance.md`; installed evidence must use the final digest. |
| Real transport | `task-10-node-browser-lanes-report.md`, correction-2 review, execution state | Interim packed Node: 20 passed/0 failed/1 pending; browser: 22/0/1. Real pairing, rotation, revocation/reauth, outage/restart, pagination and TLS/CORS ran against native macOS Hub as an owned user process. Both pending cases are installed service runtime. |
| Reviews | `task-4-clean-build-review.md`, `task-10-node-browser-lanes-fix-2-review.md` | Both bounded reviews accepted with C0/I0/M0. Earlier 304, declaration and lane-cleanup findings are closed in their reviewed scope. Do not represent superseded findings as open or the whole matrix as accepted. |
| Installed targets | Hub `current_runtime_inventory_2026_09_08` and `task10_typescript_installed_lane_2026_09_08` | macOS arm64 and Debian 13 amd64/arm64 hosts are available but stopped; none has the current package installed or registration ready. Current macOS and amd64 package candidates exist but still need Hub-owned lifecycle review/install; Debian arm64 needs a fresh reviewed package and quarantine-safe recovery. Hub owns private sessions and installed receipts. |
| Examples and docs | `examples/node.mjs`, `examples/browser/*`, README, API and compatibility docs; `examples/hub/*`, `docs/acceptance.md`, `docs/docker.md` | The minimal current-Hub consumer is source-complete and locally installed from a packed candidate. Public docs now cover lifecycle, identity, TLS/CORS, pagination and Docker; Docker runtime validation and installed Hub acceptance remain pending. |
| Browser coverage | Existing real browser evidence is Chromium on Linux VM; SDK browser script uses localhost and starts with a Node-produced credential | Full shared lane already does independent browser pairing. Reuse it. Neither that evidence nor a VM startup proves installed Hub or Safari/Firefox support; demonstrate any additional browser before claiming support. |

Reports above live under `../hub/.superpowers/sdd/2026-09-05-hub-ecosystem-compatibility/`. The shared full lane lives under `../hub/tools/interop/client_lanes/`; it is Hub-owned. SDK `scripts/test-hub-node.mjs` and `scripts/test-hub-browser.mjs` remain focused smoke scripts, not replacements for the full lane.

## Detailed plan re-review: corrections and evidence limits

The second review inspected implementation, package scripts, test definitions, CLI invitation output and lane admission before the current source changes. The table preserves why each correction was required and records its current disposition; it is historical rationale rather than a list of open source defects.

| Planning issue found | Source basis | Correction in this revision |
| --- | --- | --- |
| A bare package import from a file in the SDK repository can resolve the package's own checkout | `tests/unit/package-surface.test.ts` runs imports with repository cwd; package name is `@teslatlas/sdk` | Task 3 uses a separately named example package and validates a copied consumer with an installed tarball. |
| “Unit tests” are not all browser-free | `tests/unit/example-smoke.test.ts` imports Playwright and launches Chromium | Docker test stage includes Chromium and its system dependencies. No misleading browser-free `test:unit` claim. |
| Blanket “no late saves after dispose” exceeds the current persistence interface | `dispose(): void`; `HubCredentialStore.save(): MaybePromise<void>` | Distinguish a save not yet dispatched from caller I/O already in progress; await logout for durable clearing. |
| An explicit failed rediscovery can retain old cached identity | Earlier `src/hub/client.ts:120–139` only replaced discovery on success | Implemented generation-bound invalidation and targeted stale-identity coverage in Task 2. Installed transport proof remains M2. |
| Cancellation and size guarantees need narrower evidence | Earlier body decoding received the call signal and checked 1 MiB only after `response.text()` | Implemented composed-signal streaming reads, stale completion checks and bounded-body regressions in Task 2. |
| Installed lane command is not available merely by passing another descriptor | Hub lane README explicitly describes a fresh user-process Hub; descriptor rejects installed-service assertions | Hub now has fixed v2 TypeScript adapter source and exact contract bindings; private installed SessionInput and actual target runs remain M2 inputs. |
| A new tarball may fail hard-coded shared admission even with a correct digest | `../hub/tools/interop/client_lanes/run.mjs` binds version `2026.36.2` and the exact 81-member inventory | Hub now binds archive `03dddd…`, installed-content manifest `4b1a44f7…`, runtime entries and 81 members. |
| Machine-readable compatibility completion conflicts with current unpublished rules | `compatibility/hub.json` remains candidate; Hub version checker requires empty test arrays, bootstrap requires candidate for unpublished cohorts | Record scoped results in `docs/acceptance.md`. Hub must reconcile tested-but-unpublished semantics before any status/test-array update. Never mark published to satisfy a check. |
| Final docs affect the tarball already tested in core acceptance | `package.json.files` includes README and public docs | Final repack and member comparison are recorded in `docs/acceptance.md`; earlier receipts remain historical and installed runs must use `03dddd…`. |

Task 8 corrected the two documentation facts identified by the review: current-Hub generated hashes are pinned, and browser/native Fetch rely on configured CA/hostname trust because `CreateHubClientOptions` exposes no TLS-pin verification option. A UUID from the same untrusted discovery response is not an independent expected identity.

## Dependencies and ownership handoffs

| Owner | Input needed by this SDK | SDK output / completion boundary |
| --- | --- | --- |
| Protocol | Immutable current-Hub profile and raw contract agreement if a mismatch is found | SDK keeps its existing input/output pins. Protocol owner exports any necessary revised contract into its source repository; SDK never edits generated bytes to conceal drift. |
| Hub | Installed macOS/Debian targets; correct TLS hostname/CA; allowed browser origin; invitation/revocation CLI; verified installed lane integration | SDK supplies tar digest, version, member/export delta, runtime entry identities and final test facts. Hub changes its admission/supervisor and shared ledger. |
| Viewer | A consumer that installs the agreed SDK artifact and uses only public APIs | Viewer owner reconciles its dependency/lock/digest and verifies its own live route. SDK completion does not claim Viewer completion. |
| Swift, Home Assistant, Edge | No SDK implementation prerequisite | Their unresolved reviews remain separately owned. They may block whole-ecosystem acceptance, not completion of this SDK's six required cells. |

Continue every `READY` SDK-owned milestone while Hub prepares installed targets. Runtime-dependent portions of Tasks 5 and 7 wait for concrete inputs, while completed local work in Tasks 2–4, 6 and 8 remains valid. Source-only publication stays last, after exact artifact, installed and Docker evidence are reconciled. Do not repeat green suites or add scaffolding solely to create activity.

## Working-product acceptance criteria

- [ ] One final locally packed SDK installs into a clean consumer and resolves root types and both runtime factories without source aliases, sibling source imports or missing declarations. Strict consumer compilation uses `skipLibCheck: false`.
- [ ] Node and real Chromium independently discover the expected Hub UUID without credentials, pair using their own invitation, persist only validated credentials through the caller's store, rotate, and read seeded vehicles/current values.
- [ ] Drive queries preserve millisecond bounds and opaque cursors; three known pages contain exactly the seeded IDs without duplicates; terminal cursor, strong ETag/empty 304 and continuation after 304 work. Wrong vehicle/filter cursor use and invalid server cursors produce the expected local or HTTP error.
- [ ] Wrong Hub/endpoint, expired/bad/replayed invitation, missing capability, unknown vehicle, malformed responses and bare 401 produce bounded typed/local failures. Initial/explicit discovery identity mismatch blocks credential-bearing requests, and redirects are rejected; no automatic retry of claim/rotation. This is a discovery-bound identity contract, not continuous UUID attestation of every request.
- [ ] `await logout()` orders SDK-owned credential mutations so successful completion leaves the store cleared. Disposal aborts requests and prevents new saves from stale responses, but synchronous `dispose()` does not erase existing credentials or cancel caller I/O already dispatched. Pending operations must not report stale success. Consumers await logout when clearing credentials, then dispose. Store failures reject visibly; no false claim of successful erasure.
- [ ] Server revocation causes auth failure; the consumer requires explicit re-pairing. A transient outage can recover with explicit retry/rediscovery while preserving expected identity. A failed rediscovery cannot silently reuse a prior cached identity. Current Hub gets no invented SSE/event support or automatic mutation retries.
- [ ] HTTPS uses normal certificate and hostname validation. Browser allowlisted origin performs real OPTIONS and reads exposed ETag/request ID; denied origin and untrusted CA fail. Browser Fetch does not implement invitation TLS-pin verification, and documentation never implies it does.
- [ ] Both packed runtime lanes pass against installed macOS Hub and installed Debian 13 amd64 and arm64 Hub with current package/service evidence. No required SDK cell is pending or skipped. Browser client OS and Hub server OS are recorded separately.
- [x] Minimal Node/browser consumer instructions install from the packed package in an independent local consumer and expose the intended public Node/browser entrypoints.
- [ ] The same consumers pass against accepted installed targets. This is part of M2; M1 and other independent SDK-owned work continue while installed inputs are pending.
- [x] Bounded responses and cancellation have explicit unit coverage. Invalid or oversized bodies do not leak payloads through errors; an aborted request cannot report a successful stale response. A readiness response with HTTP 503 and valid `not_ready` data remains a typed readiness result. This is local source evidence only.

## Execution record and remaining tasks

### 1. Reconcile baseline and dependencies

**Files:** Read existing package/lock, source, tests, `compatibility/hub.json`, protocol lock and the reports above. Update only this plan's checkboxes/evidence notes as work proceeds.

- [x] Recheck SDK branch/HEAD/status and compare current source to the relevant retained reviews. Preserve `.DS_Store` and any new unrelated edits. Classify work as already implemented, failing, or awaiting installed environment.
- [x] Confirm the pinned Node/npm and browser prerequisites. Resolve missing dependencies directly during authorized execution; do not replace checks with synthetic receipts or a new framework.
- [x] Obtain the canonical Protocol identity and Hub's fixed TypeScript source-lane binding. Hub now binds profile `b80d940e…`, archive `03dddd…`, installed-content manifest `4b1a44f7…` and the current Node/browser contract hashes.
- [ ] Obtain concrete installed-target readiness from Hub and final public-package consumption confirmation from Viewer. No source rewrite is required merely because runtime inputs are pending.
- [x] Confirm product version with Hub before changing `package.json`, `package-lock.json` or `compatibility/hub.json`. Keep product version separate from `hub-http-v1@1.0.0` and richer profile `1.2.0`; do not independently invent an ecosystem version bump.

**Validation:** Read-only inventory matches actual files and blockers; future test commands below use the manifest toolchain.

### 2. Close demonstrated lifecycle and query gaps

**Likely files:** `src/hub/client.ts`; `src/hub/models.ts` only to clarify existing public contract; `src/http/fetch-transport.ts` or `src/http/empty-body.ts` only for a reproduced shared defect; `tests/unit/hub-client.test.ts`, `tests/unit/hub-models.test.ts`. Inspect `src/http/response-decoder.ts` for bounded-reading logic before introducing another utility.

- [x] Reuse existing identity, capability, drive/304, error, pairing and logout tests. Add the specific missing cases below with controllable promises/streams, rather than wall-clock sleeps. Existing source guards late discovery; use that generation concept for stale response handling where appropriate.
- [x] Define the smallest lifecycle rule consistent with `HubCredentialStore.load/save/clear(): MaybePromise`: retain each operation's session generation and composed signal across awaits; serialize SDK-owned credential mutations/clears where needed. `await logout()` must not finish before an earlier SDK-dispatched save followed by clear completes. Reject new credential mutation during logout, or serialize it after logout; document the selected behavior and test it. Do not introduce a generic transaction manager or change `dispose()` into an undocumented async API.
- [x] Preserve successful credentials on ordinary dispose. If a caller save already started before disposal, it may finish; reject stale operation success and document that limitation. A consumer needing deletion uses `await logout()` before dispose. Clearing failure must reject without automatic replay of the server mutation.
- [x] Thread the same composed operation signal through fetch, body reading and completion checks. Read oversized bodies with a byte bound instead of checking only after full buffering, reusing local helpers where their semantics match. Preserve the 1,048,576-byte cap unless a real supported Hub response demonstrates a contract issue; coordinate any cap change with Hub/protocol.
- [x] Invalidate cached discovery on explicit refresh failure/identity mismatch; authenticated calls must rediscover or fail before loading/sending credentials. This protects the explicit reconnect path. Normal Fetch CA/hostname validation does not continuously prove a server UUID between requests; do not claim an unimplemented certificate-pin guarantee.
- [x] Preserve `drives(): HubDrivesResult` with `kind: "page" | "notModified"`, `fromMs/toMs/limit/cursor/ifNoneMatch`, independent capabilities and existing browser 304 fix. Add a targeted regression only for newly observed contract mismatches.

| Regression / observed contract | Required outcome |
| --- | --- |
| Claim or rotation response is delayed until after logout/dispose | No new `save` dispatch from that stale response; operation rejects. |
| An async save starts, then logout begins, then save resolves | Logout waits for ordered clearing; after successful logout no credential remains. |
| Store save or clear rejects | Caller sees failure; claim/rotation POST dispatch count remains one; no automatic replay. Document that server rotation may already have invalidated the old token, requiring explicit recovery. |
| Logout while credential load or request body read is pending | Original operation aborts; no later success or cursor-cache update. |
| Disposal during already dispatched caller persistence | No false promise of cancellation/erasure; operation cannot report fresh session success. |
| Valid discovery A, then explicit discovery with wrong UUID or failed transport | Old cache cannot authorize the next request; no credential-bearing request before successful rediscovery. |
| A streamed body crosses 1 MiB or is aborted while waiting for another chunk | Reader is cancelled, bounded error/abort returned, no full-body buffering or raw payload in error. |
| Valid `/readyz` HTTP 503 versus invalid content | First returns typed `not_ready` plus status 503; second rejects validation. |
| Drive limit 1/500, invalid 0/501; equal/negative time bounds; unknown cursor | Preserve server contract errors, exact integer serialization and cursor opacity. Do not locally synthesize HTTP success or add automatic page iteration. |

SDK already exposes single-page `drives`, not a current-Hub page iterator. The minimal consumer handles a bounded number of pages and stops on terminal/repeated cursor without printing cursor bytes. No new paginator abstraction is needed.

**Validation:** Future `npm run test:unit -- tests/unit/hub-client.test.ts tests/unit/hub-models.test.ts`, then `npm run typecheck`. If a shared transport file changes, include its existing targeted unit tests and Node/browser conformance. Expected outcome: reproduced defect fails before its fix and relevant checks pass after it.

### 3. Deliver minimal real current-Hub consumers

**Files:** Create `examples/hub/package.json`, `examples/hub/node.mjs`, `examples/hub/index.html`, `examples/hub/app.js`, `examples/hub/serve.mjs`; modify root `package.json` scripts and existing example/package smoke tests as necessary. This is one minimal consumer, not another product or UI framework.

- [x] Give `examples/hub/package.json` a distinct private name (`@teslatlas/hub-consumer-example`) and `type: "module"`. Install a specific local SDK tarball into this consumer during setup. Node imports `@teslatlas/sdk/node`; the static server resolves `@teslatlas/sdk/browser` from this consumer's installed package and serves that single self-contained bundle as `/sdk.js`. It must not fall back to the repository's `dist` or accept an arbitrary path from HTTP requests.
- [x] Node consumer accepts non-secret options `--endpoint`, `--hub-id`, and exactly one of `--invitation-file` or `--credential-file`, plus optional `--credential-out`. Invitation input is the existing HubInvitation JSON. Credential files use `{endpoint, hubId, credential}` with the existing HubCredential nested under `credential`; check the outer endpoint/Hub ID before use. No token or invitation inline in argv. Keep credentials in memory unless an output path is requested; write new output privately with exclusive creation. Credential input and output use the same envelope shape.
- [x] Node discovers, optionally claims, lists vehicles and reads current plus a bounded drive page only for an available vehicle/capability. A zero-vehicle result is a useful empty state, not a crash; missing `query.drives` is reported as unavailable. Print counts/status only and dispose in `finally`. Use a finite caller AbortSignal timeout (default 15 seconds per request, configurable for slow environments); no background polling or implicit rotation.
- [x] Browser imports `createHubClient` from `/sdk.js`, accepts endpoint/expected UUID and full invitation JSON in local controls, and keeps credentials in memory. Provide Connect, Refresh and Disconnect with busy states; serialize operations. Disconnect awaits logout then disposes; clear sensitive controls and displayed private data. Do not serve secrets through `/config.json`, log full errors, put credentials in URLs or persist them to localStorage.
- [x] Use the actual `HubInvitation` fields (`pairingId`, `secret`, `expiresAtMs`, `endpoint`, `tlsPin`, `pairingUri`) and exact camelCase response models. On 401 clear the active state and require a new invitation before another authenticated refresh. The minimal example has no background polling; document that integrating applications must stop their own polling on auth loss. Explain manual endpoint configuration; SDK HTTP discovery is not browser mDNS scanning.
- [ ] Obtain invitation JSON through Hub's supported `pair --json --label ... --expires-in-seconds 900` command with its owner-selected config and identity; the output already has camelCase fields. The QR URI lacks the full invitation expiry, so do not invent it or claim the URI alone satisfies `HubInvitation`. Obtain expected Hub UUID independently through trusted setup/operator context; document that invitation JSON itself does not contain `hubId`.
- [x] Default browser server to loopback `127.0.0.1:4174`; Hub must allow the exact page origin (scheme, host, port). `localhost` and `127.0.0.1` are different origins. Require matching certificate hostname and normal browser trust before pairing. Do not add a proxy to avoid CORS. The normal SDK Node path also does not verify `tlsPin`; use configured CA/hostname trust.
- [x] Add root convenience scripts forwarding to the nested consumer's `node`/`browser` scripts without running SDK build. Keep these examples source-distributed by default; do not add them to the SDK tarball unless a consumer need warrants that change. README commands must say where source example files come from and how to copy them into a standalone consumer.

Implemented setup interface, used from SDK root with the final candidate pack:

```sh
npm --prefix examples/hub install --no-save --package-lock=false --ignore-scripts "$SDK_TARBALL"
npm run example:hub:node -- --endpoint "$HUB_ENDPOINT" --hub-id "$HUB_ID" --invitation-file "$INVITATION_FILE"
npm run example:hub:browser
```

`SDK_TARBALL` and `INVITATION_FILE` are absolute private input paths; `HUB_ENDPOINT` and `HUB_ID` are operator-confirmed values. These scripts now exist. For acceptance, copy only the five source example files to a fresh consumer directory outside the SDK package scope, install the tarball there, and assert import resolution points to that consumer's `node_modules/@teslatlas/sdk`. Do not copy the SDK checkout or `dist` beside it.

**Validation:** Node example returns seeded counts; real browser independently pairs, reads a page and disconnects. Also cover empty vehicles, missing drive capability, failed credential file persistence, 401 requiring re-pair, and manual refresh after an outage. Existing fixture examples remain explicitly fixtures; tests do not inject fake Fetch into real acceptance.

### 4. Verify the final package surface

**Files:** Existing `scripts/build.mjs`, `scripts/check-pack.mjs`, `scripts/test-clean-package.mjs`, `tests/typecheck/public-package-api.ts`, `tests/unit/package-surface.test.ts`, public entrypoints and `package.json`; change only when required by prior tasks.

- [x] Run `npm run verify` using installed prerequisites. Preserve the clean/stale-output package regression, generated Hub validator declarations and both frozen profile boundaries.
- [x] Pack and install the candidate in a clean consumer; compile public APIs with strict declarations, import both runtime subpaths, and verify the browser bundle contains no Node-only requirement. Keep root exports as types/errors/safe constructors; do not widen into arbitrary route execution.
- [x] Record package version, tarball digest, member inventory and tested source identity in `docs/acceptance.md`, deliberately excluded from the package files whitelist. This avoids a tarball digest embedded in the tarball itself. Raw secret-bearing receipts stay private; committed notes contain sanitized evidence references and outcomes only.
- [x] Give Hub and Viewer the exact local package identity plus changed members, exports/declarations and dependency/version changes. Hub's fixed lane now checks version `2026.36.2`, archive `03dddd…`, installed-content manifest `4b1a44f7…` and exactly 81 members. Viewer owns its dependency/lock pin and final public-consumer confirmation. Never remove the guard or change sibling files from this task.

**Validation:** Existing package checks plus practical consumer imports pass. Repeat only checks affected by later changes. If package bytes change, retain original receipts under their original identities; invalidate them as evidence for the new archive until Task 9's explicit review and required validation are complete.

### 5. Complete practical installed end-to-end acceptance

**Files:** Reuse SDK `scripts/test-hub-node.mjs`, `scripts/test-hub-browser.mjs`, `scripts/hub-acceptance-evidence.mjs`; create `docs/acceptance.md`. Read `compatibility/hub.json`; update only under the agreed shared metadata contract described below. Hub owns its full lane, installed supervisor and shared ledger.

- [ ] Wait for Hub to prepare valid installed-service targets and exact package/endpoint identities. Current macOS and amd64 package candidates exist but are not installed or registered; Debian arm64 needs a fresh reviewed package and quarantine-safe recovery. Hub also supplies private browser trust, invitation/revocation and lifecycle inputs.
- [x] Confirm Hub's fixed TypeScript Node/browser installed-adapter source branch. The v2 `{schema_version:2, kind:"installed-client-lane", mode:"node"|"browser", session_input:FileBinding}` path and SDK contract binding pass focused source checks; `installed_service_runtime` remains runner-owned.
- [ ] Obtain Hub's runner-supplied invocation and private SessionInput for each installed target. The user-process `node ../hub/tools/interop/client_lanes/run.mjs PRIVATE_DESCRIPTOR` path is historical/interim and cannot satisfy installed acceptance. Do not construct a fake installed descriptor, use a caller service-mode boolean, or alter admission from the SDK repo.
- [ ] For each of macOS, Debian amd64 and Debian arm64, run the core-acceptance candidate tarball in Node and Chromium. Assert all product criteria above, including exact seeded values/pages, independent browser pairing, revocation/reauth, outage/recovery, successful and denied CORS, normally trusted TLS and untrusted control. Hub performs scoped stop/revoke/restart on owned test instances only. Task 9 establishes final delivery identity after the required documentation phases.
- [ ] Run each minimal consumer against an accepted target from the public package. Capture runtime versions, Hub version/service mode, package digest, scenario pass/fail/pending and short redacted failure details. Retain existing proportionate evidence; no new generic receipt architecture.
- [ ] Close core acceptance only when all six SDK target/runtime combinations have no required pending cases. Hub separately owns full ecosystem/21-cell acceptance; other products' unresolved adapters do not become SDK source defects.
- [ ] Reconcile SDK machine-readable compatibility with Hub: `hub/scripts/sync-ecosystem-versions.py:_check_candidate_records` currently requires empty tested arrays for `candidate`; `hub/tools/companions/recipes.py:validate_publication_status` requires `candidate` for unpublished cohorts. Keep those fields intact under current rules and record successful scoped tests in `docs/acceptance.md`. If Hub introduces an agreed tested-but-unpublished representation, this task updates only its own `compatibility/hub.json` and validates it through the existing shared checker. No invented status, duplicated schema, or `published` claim. The metadata representation issue must be documented even if it does not prevent runtime completion.

Minimum acceptance record, filled from actual runs rather than expected outcomes:

| Installed Hub target | Packed Node | Packed Chromium | Initial status for this plan |
| --- | --- | --- | --- |
| macOS, real installed app/LaunchAgent identity | Required | Required | Package candidate exists; Hub lifecycle review, install, registration and private session pending |
| Debian 13 amd64, installed deb/systemd | Required | Required | Current deb exists; Hub install, registration and private session pending |
| Debian 13 arm64, installed deb/systemd | Required | Required | Fresh reviewed package and quarantine-safe recovery/install/session pending |

For each cell record client runtime/OS/architecture, Hub version/source fingerprint/service evidence, SDK tar digest, fixture/scenario identity, individual scenario pass/fail/pending and cleanup result. Preserve Node versus browser independent invitations. Negative local validation cases belong in unit tests; installed lanes additionally prove the real network cases they claim. No fixture-only case may satisfy a required real HTTP observation.

**Blocker rule:** If installed supervision or browser trust is unavailable, leave that cell explicitly blocked and give the owner the concrete prerequisite. Do not bypass certificate validation, open permissions or substitute mocked Fetch.

## Completed local hardening and remaining runtime/publication work

### 6. Update SDK AGENTS.md using official Astra guidance

**Files:** `AGENTS.md` and every additional SDK-owned `AGENTS.md` found at execution time; no other repository edits.

- [x] Re-read the [official GPT-6 Astra guidance](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-6-astra) on 2026-09-08. Keep concise guidance for carrying authorized work to completion, retaining session authority, resolving routine choices, proportionate verification, goal continuity and plain reporting.
- [x] Reconcile the existing Astra section rather than append duplicates. Preserve SDK-specific contract, browser safety and caller-owned credentials rules. No changes to model defaults or elaborate agent machinery.
- [x] Report SDK AGENTS coverage to Hub. The SDK has one root `AGENTS.md`; Hub coordinates workspace files and coverage across the other repositories. Each task owns its own files; no task edits App AGENTS.md.

**Validation:** Reviewed the sole SDK `AGENTS.md` for conflicts and updated its official-guidance review date. No App or sibling files were changed by this task.

### 7. Create and validate the simple Docker solution

**Files:** Create `Dockerfile`, `.dockerignore`, `docs/docker.md`; use the Node consumer from Task 3 and existing package scripts.

- [x] Resolve and pin the official `node:26.7.0-bookworm-slim` image; explicitly install npm 11.19.0 and verify both versions in each image stage. Build with the root lockfile and `.npmrc` using `npm ci`; no floating tag or engine bypass is used. The registry tag and daemon build remain to be verified when Docker is available.
- [x] Keep one Dockerfile with dependency/build, `test` and `consumer` targets. The complete test target installs Playwright Chromium and required system packages, then runs existing `npm run verify`; browser fixture/conformance checks remain local evidence, not installed Hub TLS/CORS acceptance.
- [x] Build the SDK from source and pack it in the build stage. The non-root consumer target copies only the five example source files, installs that local SDK tarball and production dependencies, and runs `node node.mjs`; it contains no SDK development tree or browser runtime.
- [x] Use explicit `COPY` inputs. `.dockerignore` excludes `.git`, `node_modules`, prior `dist`, caches, test output, tarballs, `.DS_Store`, private directories and historical evidence while retaining the protocol snapshot and fixtures needed by verification.
- [x] Define the short build/test commands: `docker build --target test -t teslatlas-sdk-test .` and `docker build --target consumer -t teslatlas-sdk-consumer .`. Neither command publishes an image; the test target does not mask failures.
- [x] Document the private-input invocation with matching UID/GID, private read-only input and separate writable credential output. No credentials are baked into image layers or passed through environment variables.
- [x] Document container networking and trust: container `localhost` is isolated, the Hub hostname must be routable and certificate-covered, and the Node consumer needs no published port or database volume.
- [ ] Validate a clean image build, full verification in the test target and the packed Node consumer against an accepted Hub. At the 2026-09-08 checkpoint the Docker client had no reachable server; resume when `docker version` reports both client and server and the accepted Hub endpoint is reachable from the container.

Planned Node consumer command (later, from a directory with a private `private/` input directory):

```sh
docker run --rm --user "$(id -u):$(id -g)" \
  --mount type=bind,src="$PWD/private",dst=/run/teslatlas,readonly \
  -e NODE_EXTRA_CA_CERTS=/run/teslatlas/ca.pem \
  teslatlas-sdk-consumer \
  --endpoint "$HUB_ENDPOINT" --hub-id "$HUB_ID" \
  --credential-file /run/teslatlas/credential.json
```

The endpoint must be reachable from the container and covered by the server certificate. Use `--invitation-file /run/teslatlas/invitation.json` instead for a fresh pair. Omit the CA environment option when normal image trust already covers the endpoint. The entrypoint is the Node example; options above are its Task 3 interface. For deliberate persistence add a private writable `/run/teslatlas-output` mount and `--credential-out /run/teslatlas-output/new-credential.json`. No secrets are passed through environment variables or baked into layers.

### 8. Reconcile all current SDK documentation

**Files:** `README.md`, `docs/api.md`, `docs/architecture.md`, `docs/compatibility.md`, `docs/product-versioning.md`, `docs/protocol-dependency-gate.md`, new `docs/docker.md`; add `docs/troubleshooting.md` only if necessary for clear setup recovery.

- [x] Put the current-Hub Node/browser path in README with prerequisites and minimal commands. Exact API/result shapes remain separate from the richer protocol contract.
- [x] Cover local packed installation, browser serving, pairing/rotation, credential ownership, identity validation, pagination/304, reconnect, revocation versus logout, CA trust, CORS and generic browser failure reporting.
- [x] Document Docker commands, persistence/networking behavior, private package/no registry publication status, ESM-only imports and tested runtime limits. Public docs do not claim CommonJS, Safari or Firefox support from Chromium evidence; precise matrix results remain in the unpacked acceptance note.
- [x] Reconcile stale current-Hub hash statements in `docs/product-versioning.md` and `docs/protocol-dependency-gate.md`; document the candidate metadata rule. Added `docs/docker.md` to the package files, pack checker and package-surface test; Hub must reconcile the resulting 81-member archive.
- [x] Inventory current task-relevant Markdown and local links; older planning records remain historical and shared ecosystem docs remain Hub-owned.

**Validation:** Follow documented installation/example/Docker commands using the final package, inspect links and run existing `tests/unit/docs-surface.test.ts`. Do not add a documentation framework.

### 9. Review and update this product's GitHub source repository last

**Files:** Only task-owned SDK source, tests, configuration and docs from the phases above.

- [x] Review the final product-specific diff for correctness, scope, credential leakage and generated/public exports. Resolve actionable findings and rerun only affected meaningful checks. Review retained warnings/failures explicitly; do not treat an earlier accepted source identity as automatic approval of new code. Final local review, secret scan, format/lint and full verification completed; the Node 26.7.0 prerequisite warning and external installed/Docker blockers remain explicit.
- [x] Repack after final documentation changes and compare complete tar member inventories against the core-acceptance candidate. Runtime/declaration and reviewed documentation members changed, so the earlier receipts remain historical coverage only; the final 81-member archive is `03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c`. Record the old-to-new archive comparison in unpacked `docs/acceptance.md`, run final archive package/declaration/clean-consumer checks and local smoke appropriate to the changed delivery, and describe this as retained runtime coverage plus final-package verification rather than six fresh installed runs of the new archive.
- [ ] If runtime, declarations, exports, dependency metadata, generated contract or other behavior-affecting members changed, rerun the affected required gates/installed lanes against the new archive. A changed package file list/member count requires Hub's admission reconciliation and final-package checks, even if runtime bytes match. If Hub's final aggregate requires an exact archive hash and has no reviewed equivalence path, coordinate the required final-archive lane reruns; leave that exact-artifact gate pending until they pass. Do not weaken the runner or edit old receipts to claim success.
- [x] Verify remote URL and `main` branch, remote divergence and current staged/unstaged changes. Preserve unrelated edits. If concurrent changes overlap, reconcile narrowly; never reset or force-push. Read-only verification confirms `origin` points to `https://github.com/magrathean-uk/teslatlas-sdk-typescript.git`, `main` is at `b1cd548`, and the dirty working tree is preserved.
- [ ] Stage explicit task-owned files/hunks only, inspect the staged diff and commit source/docs. Push to the verified SDK GitHub repository as the final execution action, then verify the remote commit identity. Exclude `.DS_Store`, `node_modules`, build outputs, tarballs, images, private evidence and secrets.
- [ ] Report checks, installed target results and limitations with the commit identity. Do not create CI, releases, tags, registry publication or upload artifacts.

Source-only GitHub publication is the terminal milestone after exact-toolchain, installed-cell and Docker evidence are accepted and the final diff is reconciled. Work on any earlier `READY` milestone continues without treating this publication gate or a short goal run as whole-goal completion. The user's execution request authorizes the eventual source-only GitHub action; it does not authorize CI, releases, tags, registry publication or artifact uploads.
