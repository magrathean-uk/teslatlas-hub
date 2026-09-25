# Teslatlas TypeScript SDK Development Plan

> Active model: **gpt-5.6-luna / max**. The owner has started development; old G0/hold paragraphs below are historical.

> **OWNER RESTART — 2026-09-12.** The owner lifted the 2026-09-10 global pause for Debian 13 ARM64 and Apple-silicon Mac work. Resume the preserved goal and dirty checkout; x86, x86_64, amd64, Intel Mac, Azure and full cross-architecture work remain paused. Revalidate runtime state and use wholly fresh expiring inputs. Current authority: [START_AUTHORIZATION.md](../../../docs/development/START_AUTHORIZATION.md).

> **Development authorized 2026-09-08; fresh native working-product goal activated 2026-09-09.** Execute ready owned work under the objective recorded below. Do not create a duplicate goal.

> **Scope exclusion — 2026-09-12.** Viewer development, packaging, integration and acceptance are excluded from this task's current goal. Historical Viewer evidence remains retained as historical evidence only; generic public browser SDK work remains in scope.

**Owner scope override (2026-09-09):** [ACTIVE_SCOPE](../../../docs/development/ACTIVE_SCOPE.md) supersedes the older full-matrix and strict R1-first ordering for this task. The immediate goal is a working public Node/browser SDK on the existing Debian 13 ARM64 and Apple-silicon Mac paths. x86, x86_64, amd64, Intel Mac, Azure and full cross-architecture acceptance are paused and cannot block this milestone.

**Goal:** Deliver usable packed public Node/browser consumers on Debian 13 ARM64 and Apple-silicon macOS. Keep named-source TeslaMate parity as a separate acceptance gap and defer broad platform matrices until the working products are usable.

**Architecture:** The package exposes the richer protocol client and a separate current-Hub client through root, node and browser exports. Generated types and validators come from the pinned Protocol bundle. Credential storage remains caller-owned; real browser trust, CORS and TLS are tested in the browser lane. This is a caller-owned library and never a Hub service. Active runtime work is limited to Debian 13 ARM64 and Apple-silicon macOS.

**Tech Stack:** Node 26.7.0 and npm 11.19.0 from package.json devEngines; TypeScript 5.9.3; Vitest 4.1.11; Playwright 1.62.1; Vite 8.2.2; openapi-typescript 7.13.0; AJV 8.20.0. Public targets are Node and browser consumers.

**Spec:** [PRODUCT_SPEC](../../../docs/development/PRODUCT_SPEC.md), [MASTER_PLAN](../../../docs/development/MASTER_PLAN.md), and [AGENTS](../../AGENTS.md).

## Status and non-goals

HEAD is b1cd548fef7ddd26be2637c14bb435480166cef7 with 31 preserved dirty entries. The package is @teslatlas/sdk 2026.36.2 with current-Hub bindings and examples/hub present. Recorded local evidence is format/lint/typecheck, 271 unit tests, 25 Node/browser conformance tests, 52 protocol tests, an 81-member package check, and external Node/Chromium consumer checks. These prove package/consumer behavior only.

The first working slice is the actual Debian 13 ARM64 Hub plus the TypeScript Node/browser clients, followed by the Apple-silicon Mac route. The current continuation records 03ddddf... as the replacement for the older 4602b950... Hub-bound archive with identical extracted members; inspect that exact tuple before reuse and rebuild only if source, package metadata or runtime inputs changed. Historical x86/amd64, Intel Mac, Azure and six-cell rows remain preserved as deferred backlog, not active prerequisites.

The SDK does not implement Hub storage/collection, pairing administration, service lifecycle, bootstrap, .deb/pkg/Docker Hub runtime, a product UI, server credentials, CI, npm publication, or mocked installed acceptance. It does not silently retry non-idempotent claim/rotation or parse/log opaque cursors.

## Prepared development VMs

The single source of detailed access instructions is [VM_ACCESS](../../../docs/development/VM_ACCESS.md); [ENVIRONMENT](../../../docs/development/ENVIRONMENT.md) records shared environment context. From the workspace root, use the shared wrapper: `scripts/dev/vm.sh debian ssh [COMMAND]` for the active Debian guest or `scripts/dev/vm.sh mac ssh [COMMAND]` for the active Apple-silicon Mac guest, for example `scripts/dev/vm.sh debian ssh uname -m` and `scripts/dev/vm.sh mac ssh sw_vers -productVersion`. The private SSH configuration is `~/dev/teslatlas-lab/access/ssh_config`; `credentials.json` may contain the Mac GUI password and must never be copied into this repository or any receipt. Do not use an x86/amd64/Intel guest or Azure target under this scope.

The active B1/B2/D1 path uses `debian13-arm64` (Debian 13.6, aarch64, 4 vCPU, 4 GiB RAM, 32 GiB disk), SSH alias `teslatlas-debian13-arm64`, user `bolyki`, forwarded endpoint `127.0.0.1:60022`, and `macos13-arm64` (macOS 13.7.4, arm64, 4 vCPU, 4 GiB RAM, 50 GB disk), SSH alias `teslatlas-macos13-arm64`, user `admin`, NAT with dynamic address currently observed as `192.168.64.10`. These are active development environments, not product acceptance by themselves. The retained `debian13-x86_64-20260909` guest and any Intel/Azure target are deferred and must not be used.

## Floors and source authority

| Requirement | Source | Floor/effect |
| --- | --- | --- |
| Node/npm | package.json devEngines, package-lock.json | Node 26.7.0; npm 11.19.0; mismatch fails |
| TypeScript/build | package.json, tsconfig*.json | TypeScript 5.9.3; strict ESM declarations |
| Generated protocol | protocol/lock.json, scripts/check-protocol.mjs | openapi-typescript 7.13.0; richer and current-Hub inputs independently hashed |
| Browser | vitest.config.ts, scripts/test-hub-browser.mjs | Playwright 1.62.1; real browser CDP and normal TLS |
| Package | scripts/check-pack.mjs, package.json | 81-member candidate inventory and root/node/browser exports |
| Client | src/hub/client.ts, src/hub/models.ts, src/hub/validate.ts | Caller-owned credentials, bounded bodies, ETags, typed errors, current-Hub routes |

## Active scope and shared gates

The owner scope is now: working Debian ARM64 product first, working Apple-silicon Mac path next, then optional active-platform companions. The old B1/B2/R1/D1/X1 sequence remains useful for labeling evidence but must not prevent ready ARM64/Mac usability work. Named-source TeslaMate admission and passive parity remain explicit acceptance gaps; they are not dependencies for independent package or consumer usability. The SDK is never a daemon, and npm publication remains separately authorized.

Active targets are the existing `debian13-arm64` and `macos13-arm64` environments. Paused targets are x86, x86_64, amd64, Intel Mac, Azure and final cross-architecture matrix rows. Paused rows remain neither passed nor failed and cannot block the active milestone. No new VM, retained deferred guest, image, build, runtime, test or cleanup action is permitted for a paused target.

## Milestones

### T1 — planning and environment hold

**depends_on:** Owner start; no Hub runtime handoff.

**Files/components:** docs/development/PLAN.md; package.json; package-lock.json; tsconfig.json; vitest.config.ts; scripts/build.mjs; scripts/check-pack.mjs; scripts/check-protocol.mjs.

Record Node 26.7.0, npm 11.19.0, lock identity and browser prerequisite availability. Preserve the independent main checkout and dirty files. Keep descriptors, invitations, CA files, browser profiles and receipts private. The owner-start hold is satisfied; continue only with fresh Hub-owned runtime handoffs and the active ARM64/Mac scope. Do not reuse closed fixtures or credentials.

**Verify:** node --version; npm --version; npm ci --ignore-scripts; npm run typecheck. Expected: exact runtime versions, lock-resolved install and declaration typecheck. An absent runtime or browser is recorded as a product-owned provisioning task; it does not create an immediate permission blocker or lower the floor.

**Done/failure:** Owner-start marker and environment record exist. Runtime, lock or compiler-floor changes invalidate later receipts and restart T1.

### T2 — B1 minimal Protocol corrections and Hub path

**depends_on:** T1; Hub and Protocol provide exact frozen hub-http-v1@1.0.0 bytes, canonical SHA256SUMS, generated-output hash and the active Debian ARM64 fixture contract. Apple-silicon Mac consumer work uses the same public profile. Paused platform admission is not a T2 dependency.

**Files/components:** protocol/lock.json; protocol/source/profiles/hub-http-v1/1.0.0/**; src/generated/hub-protocol.ts; src/generated/hub-validators.js; src/generated/hub-validators.d.ts; scripts/generate-protocol.mjs; scripts/check-protocol.mjs; src/hub/client.ts; src/hub/models.ts; src/hub/validate.ts; tests/unit/hub-client.test.ts; tests/unit/hub-models.test.ts.

Verify generated types derive from the exact profile and richer profiles remain independent. Review discovery/origin, capability gating, invitation URI/TLS-pin shape, typed errors, bounded decoding, ETag/304, cursor binding and non-idempotent claim/rotation. Make only contract-backed corrections; do not invent routes.

**Verify:** npm run protocol:check; npm run typecheck; npm run test:unit -- --run tests/unit/hub-client.test.ts tests/unit/hub-models.test.ts. Expected: lock/generated hashes agree, unsupported operations make zero requests and changed client tests pass. Profile mismatch restarts T2.

**Done/failure:** One profile/generated tuple is ready for B1. Formal package/archive admission remains T7. Local green tests do not admit a Hub.

### T3 — B1 first working Debian ARM64 Node/browser slice

**depends_on:** T2; Hub supplies selectable Debian 13 ARM64 core/bootstrap and a private descriptor for discovery, readiness, claim, vehicles, current and one drives page.

**Files/components:** src/hub/client.ts; src/hub/models.ts; examples/hub/**; tests/unit/hub-client.test.ts; tests/unit/hub-consumer-example.test.ts; scripts/test-hub-node.mjs; scripts/test-hub-browser.mjs; Hub-owned descriptor.

Run the ordinary real Node path and generic packed-browser path against the active Debian ARM64 Hub, then the Apple-silicon Mac routes when Hub supplies concrete handoffs. Keep paused platform rows separate. Require no source import, mock Fetch or synthetic success in an active consumer receipt.

**Verify:** npm run build; npm --prefix examples/hub run node -- --endpoint <hub-endpoint> --hub-id <hub-id> --invitation-file <private-invitation>. The SDK-owned browser helper uses the same descriptor and packed SDK only after a concrete browser handoff. Expected: discovery/readiness pass, claim yields a credential, vehicles is nonempty, current and one bounded drives page decode, and the exact source/profile/runtime tuple is recorded. Missing descriptor fails before requests.

**Done/failure:** A normal Hub-to-Node and generic browser-consumer path works on Debian ARM64. Bad TLS, invitation, identity, route or cleanup restarts T3 after the owner repairs the handoff.

### T4 — B2 applicable helper runtime support

**depends_on:** T3; Hub keeps the same profile/fixture identity and supplies the concrete active-platform handoff; browser and Swift owners provide consumer handoffs when applicable. The full matrix is deferred.

**Files/components:** scripts/test-hub-node.mjs; scripts/test-hub-browser.mjs; scripts/hub-acceptance-evidence.mjs; examples/hub/**; tests/conformance/node.test.ts; tests/conformance/browser.test.ts; tools/matrix-contract-node.json; tools/matrix-contract-browser.json; raw schemas/validators.

Run the Node helper in the active Debian ARM64 environment and the generic browser helper on an active real browser host, including Apple-silicon Mac when its handoff is ready. Check credential lifecycle, bounded responses, ETags, cursor continuation and actual error mapping. Use the existing harness; do not add a framework or require the deferred 21-case/full platform matrix.

**Verify:** npm run verify; npm run test:hub:node; npm run test:hub:browser when a real browser/descriptor is supplied. Expected: Node and applicable browser consumers report the same profile hash, real credentials remain private, browser normal TLS/CORS evidence is explicit, and missing inputs fail closed.

**Done/failure:** Each applicable helper has one real primary-slice receipt. A helper defect belongs in this root; a Hub fixture defect is handed back to Hub. No full-matrix claim is made.

### T5 — R1 passive data parity and recovery

**depends_on:** T3/T4 for active consumer behavior. Hub-supplied named TeslaMate observations remain a separate acceptance input and are not required to deliver or validate independent ARM64/Mac usability.

**Files/components:** src/hub/models.ts; src/hub/client.ts; tests/unit/hub-client.test.ts; tests/unit/read-operations.test.ts; docs/acceptance.md; docs/compatibility.md; Hub parity receipt.

Compare decoded current/drives fields, units, null/zero semantics, drive ordering/cursors, ETags, outage/reconnect and duplicate/retry behavior with available active Hub/browser evidence. Keep command/wake operations out of scope and preserve opaque values. If the named TeslaMate source is not selected, record the parity gap and continue independent product work.

**Verify:** npm run test:unit; npm run test:conformance; npm run test:protocol; targeted real consumer reads from the accepted B1 descriptor. Expected: decoded values match the admitted field semantics and recovery evidence is linked with limits. A changed wire unit restarts T2 and T5.

**Done/failure:** Active consumer/recovery behavior is usable on the selected ARM64/Mac path with its limits recorded. Named-source parity remains open until the owner supplies the source/backup and read-only receipt. A source change is revalidated before using old receipts.

### T6 — D1 selectable developer bundle and composition

**depends_on:** T2–T5 where available; Hub owns installer choices and selected-component composition. Minimum package/bootstrap work needed for the active ARM64/Mac products is in scope; polished distribution and paused platforms follow later.

**Files/components:** package.json; package-lock.json; scripts/check-pack.mjs; docs/acceptance.md; docs/compatibility.md; docs/docker.md; dist output; Hub-owned fixed TypeScript actor manifest.

Inspect the already rebound 03dddd candidate with the pinned Node/npm tuple. Verify tarball SHA256, ordered member list, root/node/browser entry hashes, source commit and profile hash; rebuild only if source, package metadata or runtime inputs changed. Present the SDK as an optional developer resource, never a service.

**Verify:** npm run verify; npm pack --json; node scripts/check-pack.mjs; npm run test:package; npm run protocol:check. Expected: 81 members, required exports, no source/tests/protocol inputs, and one exact source/archive/runtime tuple for Hub.

**Done/failure:** Hub can select the developer bundle by exact identity. Any source, lockfile, generated output or runtime change restarts T6.

The source-only D1 aggregate handoff is recorded at
`docs/development/d1-component-manifest-handoff-2026-09-09.json`. It binds an
external frozen source snapshot and the exact retained developer archive, while
explicitly blocking service selection: the TypeScript SDK is a caller-owned
Node/browser resource and never a Hub, Home Assistant or Edge service.

### T7 — Active ARM64/Mac consumer delivery; deferred cross-architecture matrix

**depends_on:** T3/T4/T6; Hub supplies a concrete active-platform target, package root, endpoint/trust/pairing controls and cleanup owner before any shared guest/runtime action. The source-bound Mac generic-browser contract is recorded in [macos-browser-sdk-consumer-proposal-2026-09-12.json](macos-browser-sdk-consumer-proposal-2026-09-12.json). Named-source parity is a separate acceptance gap.

**Files/components:** Hub-owned active-platform runner/session; scripts/test-hub-node.mjs; scripts/test-hub-browser.mjs; tools/matrix-contract-node.json; tools/matrix-contract-browser.json; raw schemas/validators; compatibility/hub.json only after accepted active receipts.

Run packed public Node and browser consumers on Debian 13 ARM64 and Apple-silicon macOS as active selectors. Require the applicable public import, pairing, current/history, CORS/TLS, recovery, process ownership and cleanup evidence for each active handoff. Do not require the historical six cells, x86/amd64/Intel Mac, Azure or final cross-architecture matrix before the active products are usable.

For the Apple-silicon generic browser slice, Hub must review and publish the fresh handoff defined by the proposal before the SDK starts its bounded runner. The SDK owns the disposable `http://localhost:4174` helper and redacted receipt; Hub owns the isolated Hub fixture and its cleanup.

The reviewed source-only correction was exercised once against the fresh r2 Apple-silicon handoff. The untrusted Chrome process and CDP endpoint were verified live immediately before the runner, but the runner then received `ECONNREFUSED 127.0.0.1:9232` on its first untrusted attach. The cause of that transition is unproven. The launcher detached/unreferenced Chrome without retaining exit, signal, error or stderr evidence; a separate benign detached-child probe survived tool return, so generic tool-return teardown is not demonstrated. No CA import, trusted browser, SDK route, witness or acceptance receipt was produced. The exact redacted failure receipt is [macos-browser-sequential-runtime-failure-2026-09-12-r2.json](macos-browser-sequential-runtime-failure-2026-09-12-r2.json); TypeScript and Hub cleanup both verified their scoped processes, trust state and listeners closed. The r2 handoff is consumed and must not be retried. The smallest future diagnostic, if explicitly authorized, is a persistent owner-controlled launcher with attach-time PID/lsof and Chrome exit/error/signal/stderr evidence; any future Mac attempt requires explicit owner/coordinator direction and a wholly fresh handoff.
The separately authorized no-Hub lifecycle diagnostic [macos-browser-control-diagnostic-2026-09-12-r1.json](macos-browser-control-diagnostic-2026-09-12-r1.json) used a wholly fresh 9233 profile and `about:blank` only. A non-detached, stderr-piped Chrome child stayed alive through the attach snapshot, one `Browser.getVersion` command and clean exit code 0; 9233 closed, with no Hub, keychain or SDK API action. This validates the supervision topology only and does not identify the r2 Chrome failure. Hub should review that supervision plan before any fresh full SDK handoff; no r2 retry is permitted.

On 2026-09-12, a wholly fresh static sequential-control root was prepared at `/Users/bolyki/dev/teslatlas-lab/runtime-fixtures/typescript-browser-sequential-20260912-r2` for the later `http://localhost:4175`, trusted `9234`, untrusted `9235` and Hub `18527` tuple. Its persistent supervisor and shell-free trust/control clients are mode `0700`, statically syntax- and Biome-checked, and the preparation manifest is mode `0600`. The preflight confirmed 4175/9234/9235 closed; no Chrome, Hub, keychain, credential, certificate, profile, socket, witness or acceptance receipt was created. This is preparation only: Hub must review the exact script hashes and publish a wholly fresh handoff before one live attempt; no prior r1/r2 tuple or certificate material is reusable.

The one authorized r5 attempt then consumed its single runtime allowance at the supervised control IPC boundary. The supervisor successfully launched and attached the untrusted Chrome child on 9235 and captured PID/lsof/command/CDP evidence, but the start response was lost because the Unix socket server did not keep its writable side open while awaiting Chrome readiness after the client half-close. No SDK runner, 4175 helper, Hub request, keychain mutation, trusted Chrome, witness or acceptance receipt ran. The child exited 0, 9235 closed, the supervisor stopped, the profile was removed and the fresh package consumer was moved to the explicit user Trash; the exact receipt is [macos-browser-sequential-runtime-failure-2026-09-12-r5.json](macos-browser-sequential-runtime-failure-2026-09-12-r5.json). The r5 handoff and all its runtime inputs are consumed and must not be retried.

The source-only correction is recorded in [macos-browser-control-ipc-correction-2026-09-12-r5.json](macos-browser-control-ipc-correction-2026-09-12-r5.json). The supervisor now creates its JSON-line Unix socket server with `allowHalfOpen: true` and explicitly finishes the writable side with `socket.end(...)` for both successful and failure responses. A fresh temporary-socket regression used client request half-close and covered immediate, delayed and failure responses, complete client close and socket removal; it passed under the pinned Node runtime. The legacy default behavior was separately reproduced as an empty response and timeout after the client half-close. That correction itself was source-only; the later separately authorized r6 runtime result is recorded below.

Hub independently accepted that correction source-only. TypeScript then prepared the wholly fresh r6 control/package root recorded in [macos-browser-sequential-control-preparation-2026-09-12-r6.json](macos-browser-sequential-control-preparation-2026-09-12-r6.json): new origin `4176`, trusted CDP `9236`, untrusted CDP `9237` and candidate Hub port `18528`, with the exact `2026.36.2` archive installed offline into a distinct 81-file package root. The pinned Node syntax, targeted Biome, IPC regression, package identity and closed-port checks passed at that preparation boundary; no runtime socket/state, evidence, browser profile, credential or certificate was present then.

Hub then accepted the r6 preparation and authorized exactly one fresh supervised attempt. The supervisor attached the untrusted Chrome child on 9237 and captured Chrome/152 CDP evidence, but the packed SDK runner never started because the operator-controlled stderr redirection setup had changed the evidence directory to mode `0600`, making the log path non-traversable. The attempt was consumed after Chrome spawn. The evidence directory mode was restored, the exact child exited 0, 4176/9236/9237 closed, the supervisor/socket/profile were removed and the fresh certificate remained absent from the login keychain; no CA import, trusted child, SDK route, Hub API request or acceptance receipt ran. The exact receipt is [macos-browser-sequential-runtime-failure-2026-09-12-r6.json](macos-browser-sequential-runtime-failure-2026-09-12-r6.json). The r6 tuple and attempt are consumed and must not be retried.

Hub rejected the first r6-bound file-mode correction because it could normalize consumed runtime artifacts and truncate existing runner outputs. The corrected reusable source/template is recorded in [macos-browser-control-preflight-template-correction-2026-09-12.json](macos-browser-control-preflight-template-correction-2026-09-12.json), with [browser-control-preflight-template.mjs](../../scripts/browser-control-preflight-template.mjs) and [test-browser-control-preflight-template.mjs](../../scripts/test-browser-control-preflight-template.mjs). It rejects every stale socket/state/event/profile/export/witness/acceptance/output artifact before any chmod or file creation, creates `runner.stdout` and `runner.stderr` exclusively at `0600`, requires `0700` directories, and passes a second-invocation sentinel-preservation test plus all stale-artifact pre-mutation cases. The source accepts an explicit fresh root and is not bound to r6; no r7 or runtime action occurred.

Hub then found one remaining v1 path-safety defect: lexical `resolve()` allowed a symlink passed as `controlRoot` to mutate its target. The superseding [macos-browser-control-preflight-template-correction-v2-2026-09-12.json](macos-browser-control-preflight-template-correction-v2-2026-09-12.json) binds the final source hashes, uses `lstat` to require a real directory before any artifact check or mutation, preserves nested-symlink rejection, and adds `control.sock` plus root-symlink target-preservation regressions. It remains source/template evidence only; r6 is consumed and no new runtime is authorized.

Hub accepted the v2 source-only correction. Under that authorization, TypeScript prepared exactly one wholly fresh r7 source/package candidate recorded in [macos-browser-sequential-control-package-preparation-2026-09-12-r7.json](macos-browser-sequential-control-package-preparation-2026-09-12-r7.json), with the runtime manifest at `/Users/bolyki/dev/teslatlas-lab/runtime-fixtures/typescript-browser-sequential-20260912-r7/preparation.json`. The candidate uses new origin `4177`, trusted CDP `9238`, untrusted CDP `9239` and candidate Hub port `18529`, installs the exact `@teslatlas/sdk@2026.36.2` archive offline into a distinct 81-file package root, and binds the accepted v2/template and control-script hashes. The corrected preflight passed with the root and selected ports fresh/closed; it created only empty exclusive `runner.stdout` and `runner.stderr` at mode `0600` beneath `0700` directories. Pinned syntax, targeted Biome, package identity and the browser-free IPC regression passed. No supervisor, Chrome, helper, Hub, keychain, SDK API, runtime state, profile, certificate, witness or acceptance receipt was created. This is source/package preparation only and awaits Hub review; no runtime is authorized from the receipt, and r6 inputs remain closed.

Hub then published the complete fresh r7 handoff [active-macos-typescript-browser-consumer-handoff-2026-09-12-r7.json](../../hub/docs/development/active-macos-typescript-browser-consumer-handoff-2026-09-12-r7.json), and TypeScript independently matched its immutable preparation, package, endpoint, certificate, credential, expiry, process-lineage and cleanup bindings without exposing the access token. The sole authorized attempt started the r7 supervisor and observed its `serving/idle` state, but the supervisor process disappeared before the control client connected; the exact `start-untrusted` request failed with `ECONNREFUSED` on `control.sock`. No Chrome child, untrusted CDP attach, helper, packed SDK runner, Hub API request, CA import, trusted browser, keychain mutation or acceptance route ran. The TypeScript socket/state and all scoped runtime paths were cleaned, ports `4177/9238/9239` were closed, and Hub then removed its credential and stopped its launcher/direct child and port `18529`. The exact failure evidence is [macos-browser-sequential-runtime-failure-2026-09-12-r7.json](macos-browser-sequential-runtime-failure-2026-09-12-r7.json), with the runtime receipt retained at `/Users/bolyki/dev/teslatlas-lab/runtime-fixtures/typescript-browser-sequential-20260912-r7/evidence/browser-acceptance-receipt.json`; r7 is consumed and must not be retried or repaired.

The bounded follow-up diagnosis is recorded in [macos-browser-supervisor-lifecycle-diagnosis-2026-09-12-r7.json](macos-browser-supervisor-lifecycle-diagnosis-2026-09-12-r7.json). It ran the exact r7 supervisor and untrusted-control source from fresh temporary copies under one awaited parent with `detached=false`, a fresh Unix socket, and no Hub, Chrome, credential or keychain action. The supervisor and client both exited and closed with code 0 and no signal; the client received `observed=true, closed=true, portClosed=true`; the supervisor persisted `status=stopped` and removed the socket. The retained r7 empty logs and stale `serving` state do not identify an external signal, but the one-parent result shows no supervisor implementation defect. The demonstrated correction is to own and await the server/client lifecycle in one parent and capture exit/close/signal/error evidence. No production supervisor correction or new browser runtime is authorized from this diagnosis.

Hub independently accepted this bounded diagnosis in [active-macos-typescript-browser-supervisor-lifecycle-diagnosis-review-2026-09-12-r1.json](../../hub/docs/development/active-macos-typescript-browser-supervisor-lifecycle-diagnosis-review-2026-09-12-r1.json), SHA-256 `ddcfeb5c1f303d217cc863f9f4ee31672daaee1c5aa2804bbbeab53f331e6476`. The review confirms no source defect was reproduced, preserves r7 as closed and non-reusable, and authorizes no fresh runtime.

The source-only one-parent orchestration entrypoint is prepared in [macos-browser-supervisor-orchestration-source-package-2026-09-12-r1.json](macos-browser-supervisor-orchestration-source-package-2026-09-12-r1.json). `scripts/browser-control-orchestrator.mjs` runs the accepted preflight in the same parent, binds the sequential trust mode, owns the supervisor, control requests, SDK runner and cleanup commands with `detached=false`, and persists each direct child's stdout/stderr plus error/exit/close/signal lifecycle. The browser-free regression passed success, supervisor-failure, runner-failure and control-failure fixtures, including cleanup after failures. The exact source invocation is `node scripts/browser-control-orchestrator.mjs <fresh-config.json>`; no Chrome, Hub, credential, keychain or live fixture was used.

**Verify:** After a concrete Hub handoff, the fixed runner invokes `test:hub:node` and `test:hub:browser` with the active target's private config, package root, tarball and digest; run `npm run verify` and `npm run test:package` only when SDK source changes require them; run `git diff --check`. Expected: one complete receipt per active target that is actually authorized. Missing active target leaves that active route pending; deferred rows do not block.

**Done/failure:** The active Debian ARM64 Node/browser route remains usable with explicit limits. The Apple-silicon Mac browser route remains unaccepted: r5 and r6 produced no SDK acceptance route, and the sole r7 attempt was consumed at supervisor IPC before Chrome spawn; r7 TypeScript and Hub cleanup passed and no retry or repair is admissible. Changed Hub/package/profile/reference or interrupted upgrade restarts the affected active boundary only under a new fresh authorization. Deferred x86/amd64/Intel Mac/Azure/full-matrix work remains paused, neither passed nor failed, and resumes only under a new owner direction. npm publication remains a separate instruction.

## Deferred platform backlog

The following requirements remain preserved as historical planning and contract
records but are not executable under the current scope: Debian x86_64/amd64,
Intel Mac, Azure Debian, six installed Node/browser cells, the complete
cross-architecture matrix, and final clean-host closure tied only to those
rows. Do not build, acquire an image, provision a VM, start a runtime, run a
test or perform cleanup for a deferred target. Their absence must not block the
working ARM64/Mac products.
