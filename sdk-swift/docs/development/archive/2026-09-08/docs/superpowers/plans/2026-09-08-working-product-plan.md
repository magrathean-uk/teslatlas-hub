# Teslatlas Swift SDK Working Product Implementation Plan

**Current execution model policy (2026-09-08):** The ecosystem coordinator uses `gpt-6-astra` with `thinking=high`; this Swift product task, coding, goal execution, verification and review workers use `gpt-5.6-terra` with `high` reasoning; Hub product work uses `gpt-5.6-sol` with medium reasoning. This follows the current repository `AGENTS.md` and supersedes older model clauses in this plan and the historical goal snapshot. Preserve checkpoints at model transitions and verify the actual active model.

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to continue this long-running plan milestone by milestone. Use `superpowers:subagent-driven-development` only for bounded independent work with explicit file ownership. Completing one milestone does not complete or block the whole goal while another owned milestone is ready.

**Goal:** Deliver the four-product Swift package and its separate current-Hub client as a working, source-only product on native macOS, iOS simulator, and supported Linux Swift, then obtain installed-Hub acceptance for both Swift actors across every required target before the reviewed source is committed and pushed.

**Architecture:** Preserve the strict 1.2, historical Hub v1.0.0, and current `hub-http-v1@1.0.0` product boundaries. Keep Apple networking on URLSession/Security and Linux networking on the existing libcurl/OpenSSL shim. Swift owns its package, transports, worker, tests, consumer, Docker source, documentation, and final source inventory; Hub owns installed SessionInput construction, fixed launcher/worker registration, controller observations, runtime admission, target operation, and the compatibility ledger.

**Tech stack:** Swift tools 6.0; Swift 6.4/Xcode 27; iOS 17+ and macOS 14+ package floors; Foundation and Security on Apple; OpenSSL-backed libcurl plus libssl/libcrypto on Linux; SwiftPM/XCTest; the existing Python 3.11 matrix coordinator with `jsonschema[format-nongpl]>=4.26,<5`.

**Spec:** Workspace `../WORKSPACE_AUTHORITY.md` and `../docs/superpowers/specs/2026-09-05-hub-ecosystem-compatibility.md`; Hub `../hub/docs/compatibility/execution-state.json`, `../hub/docs/compatibility/matrix.json`, and `../hub/.superpowers/sdd/2026-09-05-hub-ecosystem-compatibility/task-10-final-integration-design.md`; this repository's `AGENTS.md`. Paths below are relative to `/Users/bolyki/dev/source/teslatlas-service/teslatlas-sdk-swift` unless explicitly prefixed with `../hub/` or `../teslatlas-protocol/`.

## Current checkpoint — 2026-09-08 18:39 UTC

The checkout is `main` at `51494612db6aa3d0b0dafd08001ce432befa7fbf`, with task work and unrelated `.DS_Store` files still dirty. The current goal record is `blocked`; it has no requested token/time budget. The exposed goal tools cannot resume or replace this unfinished record. Only the user's supported **Resume goal** control can reactivate it. That accounting state does not block already authorized READY product work: execution continues at M1 and through every locally possible milestone before waiting for an unavailable owner or runtime.

The earlier portion of this checkpoint audit was observed under `gpt-5.6-sol` with `high` effort. Following the current repository policy, all further planning, coordination, product development and verification for this Swift goal use `gpt-5.6-terra` with `high` reasoning. Neither a handoff boundary nor a short agent turn is a completion condition.

| Checkpoint evidence | Current result | Acceptance boundary |
| --- | --- | --- |
| Current source bundle | `/Users/bolyki/.codex/artifacts/teslatlas-interop/2026-09-08-task10-swift-adapter-current-final/swift-to-hub-admission-bundle.json`; SHA-256 `2cce7d873d8835cd3b51043cdac01f5f1c1e10d1683254bd12bd48ce48ed0a6d` | Hash verified read-only on 2026-09-08; source/synthetic only |
| Source/launcher bindings | `source-current.json` SHA-256 `af2e6aa8240b6343e4ffdddd290a100a54c90acea7200cff8b00e2c7f6aa2cc6`; Linux worker contract SHA-256 `d3f7a0ab940d18388694d89896fb1bf9974cacaa21d26faeda77a43a53bac764`; launcher interface SHA-256 `dc7ef1bfbbf84c99f324c798e305d44d82e5296239f8d335d70f735ce009ea38` | 18-file source snapshot and fixed source/product Docker registration are hash-bound; worker authority remains root-owned |
| Independent review | `independent-review.md`; SHA-256 `9dec4f558768afb1fccf74bf5e1409f94aaee94dac96f743a786e9a4e83c6326` | I1-I5 and I7-I9 source-closed; I6 materially source-closed with installed response-provenance residual; Linux contract and Hub integration blockers reviewed; supplemental Apple fixture reviewed |
| Swift/Python local gates | release non-live 139/139; adapter 27/27; all 25 synthetic cases admitted for both actor shapes; worker-support 11/11; private-read 1/1; native macOS Apple consumer 1/1 | `installed_service_runtime` remains pending; no installed/live claim |
| Consumer/package gates | current consumer 9 tests plus release build; external four-product import build; package dump/release build | Local package evidence only |
| Apple gates | native macOS Apple consumer 1/1; iOS device and simulator debug test targets compile; iOS release package compile; simulator execution attempt recorded | SwiftPM xctest helper rejects the iOS-simulator Mach-O before tests; no physical-device runtime acceptance |
| Docker/Linux | Explicit Colima Linux arm64 context: pinned image build, 148 non-live XCTest tests, release package build, external consumer release build, Swift/libcurl/OpenSSL runtime inventory, and owned-container cleanup all passed | Linux amd64 was not executed; installed/live rows remain separate |
| Hub ledger | current Swift source and Linux worker-contract re-review recorded; Swift registry entry absent; required cells are `swift__macos_arm64`, `swift__debian13_amd64`, `swift__debian13_arm64`; accepted rows `0` | Hub SessionInput/launch/admit/supplement/log/runtime callbacks and installed target evidence remain incomplete |
| ARM target | Hub payload-hash correction accepted C0/I0/M0 | Prior guest remains quarantined; a fresh preparation/runtime retry is required |

Completed history remains valid: all four library products and the external consumer exist; current profile bytes match Protocol canonical manifest `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`; adapter findings I1-I9 are source-closed at the level stated by the fresh review; exact phase-result shape, private reads, cursor/request provenance, ETag/cache-control witnesses, one monotonic worker cell deadline, and native Apple URLSession route/auth witnesses have focused regression evidence; the Apple test target is Darwin-gated so Linux builds retain their existing scope; repository AGENTS guidance, Docker source, and product documentation are present. The Linux worker contract is now hash-bound to the launcher interface and tested for host-only broker/completion flow. Do not reopen these areas without a reproduced defect or a changed admitted source.

## Remaining end-to-end objective

Full completion requires all of the following in one stable final source candidate:

- The public package and all four products build from an ordinary external SwiftPM consumer on recorded supported toolchains.
- Native macOS and iOS simulator tests exercise the Apple production transport and client lifecycle. Simulator compilation alone is retained as compile evidence, not runtime evidence.
- Supported Linux Swift builds and runs the relevant package/transport tests with recorded compiler, architecture, libcurl TLS backend, and native library versions.
- Cancellation, timeouts, body/header bounds, exceptional fixture/worker cleanup, and credential redaction terminate within their stated bounds without lingering processes, tasks, listeners, or connections.
- A coherent wrong-secret invitation reaches an actual isolated Hub claim endpoint and returns HTTP 401; TLS identity/pin validation still precedes secret transmission.
- `swift_macos` and `swift_linux` each pass all 25 contract cases against all three required Hub targets through the Hub-owned fixed registry and root supervisor. Partial cells, synthetic rows, or copied expected metadata do not substitute.
- Hub accepts the exact runtime/product/source identities and receipts; an independent review approves the final candidate; only then are task-owned source/docs committed and pushed to `main`.

## Global constraints

- Work in the existing independent `main` checkout. Preserve unrelated changes; do not branch, reset, clean, stash, or rewrite another task's index entries.
- Teslatlas App is outside this goal. Do not edit App or use it as the SDK's Apple acceptance harness.
- Hub alone edits Hub source, ledger, hosts, registry, services, and installed-target state. Swift reports exact dependencies and consumes the resulting fixed interface.
- Protocol owns canonical `hub-http-v1@1.0.0`; embed byte-identical approved resources and never weaken strict or historical validators to accept current-Hub data.
- GitHub is source storage only. The final publication phase may commit and push reviewed source after all required gates pass; no CI, release, tag, binary, package archive, or evidence-artifact upload.
- Installed runs use isolated Hub targets and owner-provided private credentials. Never restart production, wake a vehicle, send a vehicle command, expose secrets, or reuse the quarantined ARM guest.
- Run focused tests while changing source, then one proportionate final suite after the source is stable. Repeating green suites without a changed input is not progress.

## Milestone status

| ID | Status | Deliverable |
| --- | --- | --- |
| M1 | COMPLETE (LOCAL) | Close remaining Swift-owned actual-transport, error-mapping, cancellation, deadline, and exceptional-cleanup evidence; change production source only for a reproduced defect |
| M2 | PARTIAL; WAITING FOR IOS TEST HOST | Native macOS and Apple-target compile evidence is complete; SwiftPM still lacks an iOS XCTest application host for simulator execution |
| M3 | COMPLETE (LINUX ARM64 CONTAINER) | Execute supported Linux Swift and Docker image build/tests with a real daemon or equivalent approved Linux runtime |
| M4 | WAITING FOR OWNER/RESOURCE | Consume Hub's complete fixed Swift registry/SessionInput/worker/admission path and pass both actors through all 25 cases on all three installed targets |
| M5 | WAITING FOR OWNER/RESOURCE | Freeze final source identities, obtain final independent/Hub acceptance, then commit and push only reviewed source/docs |

M1 local evidence is complete. M2 native macOS and Apple-target compilation are complete, while iOS runtime execution waits on an application test host. M3 is complete for the observed Linux arm64 container; Linux amd64, M4 and M5 remain open under their listed dependencies.

## Dependency table

| Owner task ID | Owner scope | Exact missing input | Work still possible locally | Event that unblocks |
| --- | --- | --- | --- | --- |
| `01a07f89-4e6b-76d0-ae93-cf958ce1becd` | Swift M2 | An iOS XCTest application host/project or owner-approved simulator runner; an iOS 27 runtime and shutdown UDID are available | M1, native macOS URLSession evidence, Apple device/simulator test-target compilation, and simulator execution attempt | A host runs `CurrentHubAppleConsumerTests` on the selected UDID with a nonzero test count |
| `01a07048-a05c-72a2-9c7a-87dd59f52e9b` | Hub task 10 fixed registry | Source-fixed `FixedInstalledAdapter` entry for Swift with working `build_session_input`, `launch_adapter`, `admit`, `build_supplement`, `execution_logs`, and `runtime_inventory`; fixed `swift_macos` and `swift_linux` worker callables | M1/M2, final Swift source review, interface compatibility checks against read-only Hub source | Hub tests the complete callbacks, admits the exact current-bundle-bound source or its reviewed successor, and provides a dispatchable registry entry |
| `01a07048-a05c-72a2-9c7a-87dd59f52e9b` | Hub task 10a installed runtime | Isolated macOS, Debian amd64, and fresh Debian arm64 targets; private config/CA/invitation/seed; controlled broker/forwarding; Docker/Linux runtime; controller session and cleanup authority | M1/M2 and Docker static/source checks | Owner supplies current runtime inventory and target bindings; fresh ARM preparation passes without touching the quarantined guest |
| `01a07f89-4e6b-76d0-ae93-cf958ce1becd` | Swift final source and publication, coordinated with `01a07048-a05c-72a2-9c7a-87dd59f52e9b` | Stable final task-owned tree, accepted installed rows, final review, correct remote/main identity, and non-conflicting staged paths | Prepare source inventory and publication checklist without staging unrelated files | All product gates pass and Hub records the exact final source/receipt relationship |

## M1 — Close remaining local transport and exceptional-cleanup evidence

**Status:** COMPLETE (LOCAL)

**Files:**

- Modify only if a failing test demonstrates a defect: `Sources/TeslatlasCurrentHub/CurrentHubTransport.swift`, `Sources/TeslatlasCurrentHub/CurrentHubClient.swift`, `Sources/CurrentHubCurlShim/CurrentHubCurlShim.c`, `Sources/CurrentHubCurlShim/include/CurrentHubCurlShim.h`.
- Tests/support: `Tests/TeslatlasCurrentHubTests/CurrentHubTransportTests.swift`, `CurrentHubClientTests.swift`, `CurrentHubLiveTests.swift`, `CurrentHubMatrixWorkerTests.swift`, `CurrentHubMatrixWorkerSupportTests.swift`, `CurrentHubTestSupport.swift`, and existing fixtures.
- Contract files only if their admitted shape changes: `tools/matrix-contract.json`, `tools/matrix_contract.py`, `tools/test_matrix_adapter.py`, `tools/swift-raw-v1.schema.json`, `tools/swift-worker-v1.schema.json`, `tools/swift-launcher-interface-v1.json`, `tools/swift-linux-worker-contract-v1.json`, and both phase recipes.

**Interfaces:**

- Consumes `CurrentHubHTTPTransport.send(_:)`, `CurrentHubInvitationPinningTransport.send(_:validatingLeafCertificateSHA256:)`, `CurrentHubClient.claim`, `CurrentHubClient.rotateCredential`, and the C `current_hub_curl_operation_{create,perform,cancel,destroy}` lifecycle.
- Preserves the reviewed `MatrixWorkerChannel.phase`, `matrixValidatePhaseResultEnvelope`, `matrixPrivateRead`, exact worker-result schemas, and root-issued `remaining_cell_ms` contract.
- Produces bounded, redacted test evidence for plain and JSON 401/404/503 errors, cancellation, timeout, exact/over-limit bodies, Linux header bounds, late Ack rejection, and exceptional resource cleanup.

- [x] Inventory existing named tests against the review residuals. Added only missing failure-first coverage; the gap-to-test mapping is retained in the handoff.
- [x] Add a real loopback TCP response path that sends a coherent HTTP 401 through the production transport and client decoder. Assert `.unauthorized(requestID:)`, no credential or invitation content in the error text, and one request. This remains local HTTP evidence, separate from M4's actual-Hub wrong-secret 401.
- [x] Add cancellation/error cleanup probes that assert the server task finishes, listener and accepted descriptor are closed, and no child process or detached transport task survives. Success, cancellation during transfer, oversized body/header rejection, timeout, and unexpected transport failure are covered by the focused transport and worker fixtures.
- [x] Add worker exceptional-path coverage for late Ack, cumulative phase deadline, rejected Ack, evidence replacement, and bounded fixture cleanup; exact phase result order/count and cursor/ETag provenance remain unchanged. Premature EOF/nonzero child and installed cleanup remain owner-runtime evidence where the worker is launched.
- [x] For the reproduced iOS test-target defect, add only the platform guard around the macOS Process fixture. No production networking layer, validation framework, credential service, or process orchestrator was introduced.
- [x] Run focused tests first; after source changes, refresh the 27-test adapter evidence, 139-test Swift release non-live suite, Apple consumer/target gates, worker-support evidence, hash bindings, and review inventory. `swift build -c release` and `git diff --check` remain green from the stable candidate.

**Exit evidence:** local injected/fixture exceptional paths terminate within their bounded deadlines; descriptors/tasks/processes are joined or closed; error values remain typed and redacted; I6/I7/I9 source properties remain closed. Installed/root cleanup and actual-Hub behavior remain reserved for M4.

## M2 — Execute native Apple platform evidence

**Status:** PARTIAL; WAITING FOR IOS TEST HOST

**Files:** Create `Tests/TeslatlasCurrentHubTests/CurrentHubAppleConsumerTests.swift` as the SDK-owned iOS runtime test entry. Modify `Package.swift`, `CurrentHubTransportTests.swift`, `CurrentHubLiveTests.swift`, `Examples/CurrentHubConsumer/Package.swift`, and its source/tests only for a reproduced Apple build/runtime defect.

**Interfaces:** the public four-product SwiftPM surface, `CurrentHubURLSessionTransport`, Security trust/hostname/leaf-pin validation, caller-owned `CurrentHubCredentialStore`, and the existing SDK-owned consumer.

- [x] Record `/Applications/Xcode-beta.app` Xcode/Swift versions and native macOS architecture. The ordinary external four-product import build and current consumer tests passed from isolated temporary builds without modifying App.
- [x] Run the focused macOS public Apple consumer test through the production URLSession path with a deterministic URLProtocol fixture. It is synthetic URLSession/client evidence; no owner-supplied Hub or TLS trust handshake was used.
- [x] Inventory available iOS 27 simulator devices with `xcrun simctl list devices available -j`; ten shutdown devices were observed and `Teslatlas Fresh iPhone 17 Pro` (`E709788A-6E76-4A9B-B4CA-E91E46C524A6`) was selected as the candidate without booting it.
- [x] Build the SDK test target for arm64 iOS and arm64 iOS Simulator. The selected SwiftPM execution attempt is retained and fails before tests because its macOS `swiftpm-xctest-helper` cannot load the iOS-simulator Mach-O; this step remains runtime-open until an iOS XCTest application host is supplied.
- [x] No live config, invitation, or CA was staged because no simulator application host exists; the native fixture uses no private live inputs. The live/private-container rule remains mandatory for the owner-supplied runtime lane.
- [x] Record Xcode/Swift, iOS SDKs, simulator inventory, test-target build hashes, the macOS 1/1 run, the zero-test simulator attempt, and limits in `apple-platform-evidence.json`. Physical-device runtime remains untested.

**Exit evidence:** native macOS URLSession/client execution and iOS device/simulator test-target compilation are distinct and reproducible. iOS simulator execution remains open because SwiftPM has no iOS XCTest application host; compilation does not substitute for runtime evidence.

## M3 — Execute supported Linux Swift and Docker evidence

**Status:** COMPLETE (LINUX ARM64 CONTAINER)

**Files:** `Dockerfile`, `.dockerignore`, `docs/development.md`, `Package.swift`, Linux transport/shim source and tests. The Docker image is a local build/test environment, never an SDK service or Hub installed-row substitute.

**Required resource:** a working Docker daemon capable of the selected architecture, or an approved supported Linux host with equivalent package prerequisites. The approved `colima-interop-20260905` context supplied a Linux arm64 Engine for this run.

- [x] From the repository root, run the pinned-image build, the default non-live tests, the release package build, and the external consumer release build with explicit context and owned `--rm` containers.
- [x] Record image ID/architecture, actual container user, `swift --version`, Linux kernel, libcurl/OpenSSL backend and native library versions in `linux-docker-evidence.json`.
- [x] Verify the container executes 148 non-live XCTest tests with zero failures; live/installed and matrix-worker suites remain explicit skips rather than hidden acceptance.
- [x] Exercise the Linux cancellation, cumulative header/body bounds, informational/final header, repeated-auth, timeout and trust-related focused tests through the libcurl/OpenSSL implementation in the container suite.
- [x] Keep the architecture claim exact: this run proves Linux arm64 only; no amd64 or native performance claim is made.

**Exit evidence:** supported Linux toolchain and native TLS dependencies are observed at runtime, the package builds, relevant tests run and pass, cleanup is bounded, and the documented Docker commands work. This does not satisfy M4 installed rows.

## M4 — Pass the Hub-owned installed adapter and live lifecycle

**Status:** WAITING FOR OWNER/RESOURCE

**Swift-owned files if an installed failure reproduces a Swift defect:** `Tests/TeslatlasCurrentHubTests/CurrentHubLiveTests.swift`, `CurrentHubMatrixWorkerTests.swift`, `CurrentHubTestSupport.swift`, `tools/matrix_live.py`, `tools/matrix_wire.py`, `tools/matrix_contract.py`, schemas/phase contracts, and `compatibility/hub.json` only after Hub accepts evidence.

**Hub interface consumed read-only:** `hub/tools/interop/matrix_runner/installed_registry.FixedInstalledAdapter`; `hub/tools/interop/matrix_runner/swift_installed.{CONTRACT,REVIEWED_ADAPTER,execution_by_target,broker_kind_by_target,run_coordinator,validate_product_inventory}`; `SwiftLauncherCapability.remaining_cell_ms`, `run_worker`, and `controller_observations`.

The Hub already source-binds the reviewed Swift contract/topology and product inventory seam. It still lacks a Swift entry in `FIXED_INSTALLED_REGISTRY` because complete SessionInput staging, fixed macOS/docker worker launch callables, admit/supplement/log/runtime callbacks, and actual runtime evidence are not ready. The read-only Hub review also found a stale `matrix_live.py` source hash, no source-archive staging for the declared `swift_source` input, and no Docker container-path rebinding/binding-index implementation. Swift must not add or simulate that Hub-owned entry.

- [ ] Hub supplies one current, source-fixed entry whose six callbacks close over reviewed code and reject job-selected executables, argv, Docker sockets, SSH material, broker descriptors, or arbitrary commands.
- [ ] Hub stages closed, hash-bound SessionInput/profile/scenario/certificate/source/product/phase inputs and registers exactly `swift_macos` plus `swift_linux`; the native worker uses local execution and the Linux worker uses the reviewed Docker exec-pipe with one root-owned Unix broker.
- [ ] Run the public-client live journey against an isolated owned Hub: discovery, coherent wrong-secret claim returning actual HTTP 401, successful claim, health/ready, vehicles/current, three-page drives and terminal cursor, ETag 304, rotation/old-bearer rejection, restart/outage recovery, server revocation, and re-pair.
- [ ] Run all 25 contract cases for each actor on `swift__macos_arm64`, `swift__debian13_amd64`, and a fresh `swift__debian13_arm64`. Record exact runtime/package/source identities, controller observations, request transcripts, response provenance, deadlines, root close-before-Ack ordering, process exit, and exceptional cleanup.
- [ ] Keep the prior Debian arm64 guest quarantined. The accepted root payload-hash correction must be exercised only during a fresh owner-controlled preparation and retry. After Mac/VPS verification and the remaining Hub integration work, the owner will supply fresh Azure Debian arm64 and x86_64 VMs for the final bootstrap, `.deb` and Docker gates; do not provision them early or treat the local arm64 container as those rows.
- [ ] Hub independently admits the normalized cases, both actor outcomes, supplement, logs, runtime inventory, and cleanup before updating its matrix/ledger. A locally generated expected-equals-actual fixture never promotes a row.

**Exit evidence:** three accepted Swift cells, each with both actors and all 25 cases; actual wrong-secret HTTP 401; actual restart/revocation/re-pair; response cursor/header provenance; bounded cleanup; zero unexplained skips. Any missing target keeps full installed acceptance open while other target evidence remains useful and accurately labeled.

## M5 — Freeze, review, and publish the final source

**Status:** WAITING FOR OWNER/RESOURCE

**Files:** every task-owned source/test/tool/schema/resource file; `AGENTS.md`; `Dockerfile`; `.dockerignore`; `Examples/CurrentHubConsumer/**`; `README.md`; `docs/**`; `compatibility/hub.json`. Re-inventory immediately before staging.

- [ ] After M1-M4 are complete, recheck branch, HEAD, remote, index, untracked paths, and the entire task-owned diff. Preserve unrelated files and pre-existing index entries.
- [ ] Produce one final source inventory and retained-source/bootstrap handoff under Hub's accepted finalization design. If any admitted member changed, regenerate its hashes and installed rows rather than rebinding an old executable or receipt.
- [ ] Run one final proportionate package/consumer/platform/contract suite against the stable candidate; validate JSON/Markdown links/package contents and `git diff --check`. Do not repeat unchanged green suites.
- [ ] Obtain final independent review and Hub confirmation that the exact source, package, runtime, and receipts match. Keep final results outside the tested source snapshot to avoid a self-referential hash loop.
- [ ] Stage only reviewed task-owned paths/hunks. Inspect the staged diff and compare staged bytes with the tested inventory; resolve remote overlap without reset or force-push.
- [ ] Commit and push source/docs to the intended `main` remote, then verify the remote commit identity. Do not create CI, releases, tags, binary uploads, package archives, or evidence-artifact uploads.

**Exit evidence:** the remote source commit is the independently reviewed and Hub-accepted candidate; all required platform and installed gates are linked to that source identity; unrelated work remains intact. Only this state completes the whole goal.

## Continuation and completion rules

- The durable goal spans M1-M5. A milestone handoff, agent timeout, or ten-minute runtime boundary is a checkpoint, not a reason to mark the goal complete or blocked.
- When a dependency is unavailable, finish every other READY milestone and record the exact missing input plus unblock event. Do not create tiny scaffolds or rerun unchanged tests to appear active.
- Mark the whole goal complete only after M5 exit evidence exists. Mark it blocked only when no READY work remains and an external owner/resource is actually required; when resumed, continue from the first incomplete milestone without replacing the objective.
