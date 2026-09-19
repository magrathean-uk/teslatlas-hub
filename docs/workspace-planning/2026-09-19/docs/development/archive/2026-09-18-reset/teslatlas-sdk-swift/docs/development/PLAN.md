# Teslatlas Swift SDK Development Plan

> Active model: **gpt-5.6-luna / max**. The owner has started development; older full-matrix wording remains historical.

> **Development authorized 2026-09-08; fresh working-product goal active 2026-09-09; owner restart 2026-09-12.** The global 2026-09-10 pause is lifted for Debian ARM64 and Apple-silicon Mac work. Resume from preserved checkpoints and use fresh expiring inputs; x86 remains deferred. Older full-plan wording remains historical. Current authority: [START_AUTHORIZATION](../../../docs/development/START_AUTHORIZATION.md) and [ACTIVE_SCOPE](../../../docs/development/ACTIVE_SCOPE.md).

**Goal:** Deliver a normal usable SwiftPM consumer on supported Apple-silicon macOS and supported Debian 13 ARM64, with package shape, public `TeslatlasCurrentHub` consumer usability, strict current-Hub/profile and transport boundaries, and the minimum ARM64/Mac packaging or lifecycle evidence needed for a usable product. Preserve x86/amd64/Intel Mac/Azure/full-matrix work as paused, keep named-source parity as a separate acceptance gap, require concrete Hub handoffs for shared runtimes, and do not modify `app/`.

**Architecture:** The package keeps three explicit contract boundaries: strict TeslatlasHubSDK/TeslatlasCommands, hash-pinned TeslatlasHubV1Compatibility, and separately bound TeslatlasCurrentHub. Apple uses URLSession/Security; Linux uses the existing OpenSSL-backed libcurl shim. SwiftPM is a library/developer bundle and consumer, never a Hub service or installer daemon.

**Tech Stack:** Swift tools 6.0 from Package.swift; Swift 6.4/Xcode 27 on the active local Mac; iOS 17+ and macOS 14+ package floors; Foundation/Security on Apple; libcurl plus libssl/libcrypto on Linux; SwiftPM/XCTest; Python 3.11+ and jsonschema[format-nongpl]>=4.26,<5 for matrix tooling. Dockerfile uses digest-pinned swift:6.0.3-jammy for ARM64-local package checks.

**Spec:** [PRODUCT_SPEC](../../../docs/development/PRODUCT_SPEC.md), [MASTER_PLAN](../../../docs/development/MASTER_PLAN.md), [ACTIVE_SCOPE](../../../docs/development/ACTIVE_SCOPE.md), and [AGENTS](../../AGENTS.md).

## Owner scope override

The 2026-09-09 owner direction supersedes older strict stage-order and full-matrix blockers for current execution:

- **Restart:** The owner explicitly resumed this existing task on 2026-09-12. Preserve its single native goal, dirty checkout and completed receipts; do not create a replacement goal or reuse closed runtime cohorts.

- **Active targets:** the existing Apple-silicon Mac runtime at macOS 27.0, and the existing Debian 13 ARM64 guest (`debian13-arm64`).
- **Immediate product:** a normal SwiftPM consumer using `TeslatlasCurrentHub` on those supported runtimes, with only the minimum packaging or lifecycle work needed to run it.
- **Paused for this task:** x86, x86_64, amd64, Intel Mac, the prepared macOS 13 guest as a supported Swift target, Azure, and final cross-architecture matrix rows. These are deferred, neither passed nor failed, and cannot block the active ARM64/Mac product.
- **Preservation:** keep code, schemas, historical receipts, and compatibility records for deferred targets unchanged. Do not remove support or weaken validators to obtain an ARM64/Mac result.
- **R1 boundary:** missing named-source TeslaMate inputs, real import, passive provenance, and full parity remain explicit acceptance gaps, but they do not stall independent consumer usability on the active targets.
- **Coordination:** Hub owns Hub code, shared installer/runner code, and shared guest/runtime changes. Swift may make SDK-owned source and documentation changes; a concrete Hub handoff is required before any shared runtime or installed action.
- **Resources:** acquire `~/dev/teslatlas-lab/locks/heavy-build` before every host/guest build or install. Do not create VMs, touch `app/`, commit, push, publish, run CI, alter production/vehicle state, or consume usage resets.

The prepared `macos13-arm64` guest is an environment mismatch, not a reason to change the package floor: it reports macOS 13.7.4 while Package.swift requires macOS 14. The active local Mac reports arm64, macOS 27.0, Swift 6.4 and Xcode 27, so it is the supported Apple runtime for this objective.

## Status and non-goals

HEAD is `51494612db6aa3d0b0dafd08001ce432befa7fbf` with preserved dirty entries. The package contains four library products, one executable example product, and an external consumer package. Current-Hub bindings carry profile digest `b80d940e...c7d926` and product version `2026.36.2`. Existing package, adapter, iOS simulator, and ordinary macOS consumer evidence remains retained evidence; it is not rerun merely because the fresh goal was recreated.

The first working product is the public current-Hub consumer on the active local Mac and Debian 13 ARM64 where the applicable Hub handoff exists. Swift does not implement Hub storage/collection, service lifecycle, bootstrap, .deb/pkg/Docker Hub runtime, UI, Rust FFI, server-held credentials, invented routes, CI, publication, or a native Home Assistant service. Strict protocol, historical Hub-v1 and current-Hub models never share routes or conformance claims implicitly.

## Active environments and floors

| Requirement | Source | Active effect |
| --- | --- | --- |
| Swift language/tools | Package.swift | Swift tools 6.0; local Mac evidence uses Swift 6.4/Xcode 27 |
| Apple package floor | Package.swift | macOS 14+; active local Mac is macOS 27 arm64 |
| Linux active target | ACTIVE_SCOPE / VM_ACCESS | Debian 13 ARM64 (`debian13-arm64`); use the existing guest only |
| Linux transport | Package.swift CurrentHubCurlShim | libcurl, libssl and libcrypto; system CA/hostname validation |
| Linux test tooling | docs/development.md; tools/requirements-dev.txt | Python 3.11+; jsonschema[format-nongpl]>=4.26,<5 |
| Local Linux image | Dockerfile | swift:6.0.3-jammy manifest digest; unprivileged swiftuser; ARM64 use only in this scope |
| Contract boundaries | Sources/TeslatlasHubSDK, Sources/TeslatlasHubV1Compatibility, Sources/TeslatlasCurrentHub | Separate profile/binding hashes and unsupported operation sets |

The macOS 13 guest, x86/x86_64/amd64 targets, Intel Mac, Azure, and full cross-architecture rows remain preserved deferred work. They must not be selected for a new build, image, VM, runtime, cleanup, or acceptance action under this plan.

## Historical evidence boundary

B1/B2/R1/D1/X1 and G0–G6 labels remain useful for reading prior receipts and compatibility schemas. This amendment changes current execution scope only; it does not promote old evidence, delete historical receipts, change wire schemas, or claim parity. A future owner direction may reopen the paused targets.

## Milestones

### S1 — active environment and floor confirmation

**Depends on:** Owner scope override; no new VM or shared runtime handoff.

**Files/components:** Package.swift; VERSION; Dockerfile; .dockerignore; docs/development.md; tools/requirements-dev.txt; docs/development/STATUS.json.

Record the active local Mac architecture/version/toolchain and the existing Debian 13 ARM64 environment. Treat the macOS 13 guest as below the declared floor. Preserve live config, invitation, CA, bearer, browser/consumer state and receipts. Do not inspect or use deferred x86/amd64/Intel/Azure targets.

**Verify:** `uname -m`; `sw_vers -productVersion`; `swift --version`; `xcodebuild -version`; `swift package dump-package`; and the existing read-only Debian ARM64 access check when a fresh environment fact is needed. Expected: active Mac is arm64 and macOS 14+, Package.swift retains iOS 17/macOS 14, and the ARM64 guest remains Debian 13. The 2026-09-09 preflight found Debian 13 aarch64 with Docker available only through root-owned access, but no native Swift or clang; this is a Hub-owned toolchain/container or fixed-runner dependency, not permission to install packages or start shared containers. Missing Linux libraries or a Hub handoff is recorded as a dependency; the package floor is not changed.

**Done/failure:** A supported local Mac and active Debian ARM64 lane are identified. The macOS 13 guest and all deferred architecture rows remain non-accepting.

### S2 — strict contract bindings and current-Hub consumer boundary

**Depends on:** S1; Hub and Protocol provide exact frozen `hub-http-v1@1.0.0` bytes, canonical SHA256SUMS, product identity and an applicable active-target descriptor.

**Files/components:** Sources/TeslatlasCurrentHub/Binding/**; Sources/TeslatlasCurrentHub/CurrentHubBinding.swift; CurrentHubClient.swift; CurrentHubModels.swift; CurrentHubTransport.swift; Sources/TeslatlasHubSDK/**; Sources/TeslatlasHubV1Compatibility/**; the corresponding tests.

Keep the strict protocol, deployed Hub-v1 and current-Hub products isolated. Review endpoint identity, origin, TLS/leaf-pin rules, credential store, bounded response/body handling, ETags, cursors, typed errors and unsupported zero-I/O behavior against the admitted profile. Correct only source-backed defects. This contract work is active for Apple-silicon Mac and Debian ARM64; deferred architecture rows cannot block it.

**Verify:** Use the existing package and non-live evidence unless source changes invalidate it. A changed binding/profile/product identity requires a new focused check before consumer work continues; do not rerun unchanged green suites solely for the scope amendment.

**Done/failure:** One Swift/profile/product tuple is ready for a normal active-target consumer. Local evidence remains distinct from live or installed acceptance.

### S3 — normal SwiftPM consumer on active targets

**Depends on:** S2; Hub supplies a concrete active-target endpoint/trust/invitation handoff when a new live or service run is required.

**Files/components:** Examples/CurrentHubConsumer/**; Sources/TeslatlasCurrentHub/**; Tests/TeslatlasCurrentHubTests/CurrentHubLiveTests.swift; CurrentHubAppleConsumerTests.swift; CurrentHubTestSupport.swift; Hub-owned active-target descriptor/fixture.

Deliver the ordinary public consumer route on the supported local Mac and, where the existing Debian ARM64 environment supports it, Debian ARM64. The route covers discovery, readiness, claim, vehicles, current state and a bounded drives page. Use the application-owned credential store and keep invitation, CA, bearer and receipt inputs outside the repository. Minimum launch/setup integration needed to use the consumer is in scope; Swift must not become a Hub service.

**Verify:** Build or run only when the source or a concrete active-target Hub handoff requires it. The applicable receipt must identify the active runtime, source/profile/product identity, TLS/trust result and cleanup. Missing named-source TeslaMate input is not a reason to block this consumer route.

**Done/failure:** A normal usable SwiftPM consumer is available on the supported Apple-silicon Mac and, when the active Hub handoff supports it, Debian ARM64. No x86/amd64/Intel/Azure row is needed for this milestone.

### Current working-product boundary (2026-09-09 scope clarification)

For this Swift goal, “normal usable SwiftPM consumer” means the public
`Examples/CurrentHubConsumer` executable can be built and completes one
bounded current-Hub journey on each active runtime for which a concrete Hub
handoff exists. The Debian ARM64 R3 receipt satisfies the Debian side. The
existing macOS `CurrentHubLiveTests` receipt proves the SDK's native
URLSession client path, but it is not a live receipt for the external
`CurrentHubConsumer` executable on the supported Apple-silicon Mac. That is
the only remaining current working-product criterion.

The sample deliberately uses an in-memory credential store and clears it when
the process ends; a new process therefore requires a fresh claim. Persistent
pairing state belongs to an application-owned credential store, while Hub
restart and durable data preservation belong to the Hub/host product. They are
not Swift SDK-owned acceptance criteria for the current developer-resource
goal. If cross-product restart/data-preservation acceptance is later required,
the smallest additional Hub handoff is one fresh supported-Mac endpoint,
owner-only trust/configuration and invitation, plus a Hub-owned restart
boundary and retained-data sentinel. It does not require a formal installed
Swift runner or full matrix.

Formal installed Swift rows, fixed-runner admission, upgrade/removal
lifecycle, named-source parity and deferred architectures remain separate
backlog gates. They must not replace the single missing Mac public-consumer
receipt as the next action for this goal.

### S4 — active-target reliability and data correctness

**Depends on:** S3; relevant active-target observations or a bounded Hub handoff.

**Files/components:** Sources/TeslatlasCurrentHub/CurrentHubModels.swift; CurrentHubClient.swift; CurrentHubLiveTests.swift; docs/current-hub.md; docs/architecture.md; compatibility/hub.json.

Keep Swift-decoded current/drives fields, units, signed IDs, null/zero semantics, cursor ordering, ETags, outage/reconnect and duplicate/retry behavior correct on the active Mac/ARM64 consumer. Preserve actor-owned credential lifecycle and avoid commands/wake operations. Named TeslaMate source admission, real import, passive provenance and full parity remain separate acceptance work; they must not block a usable seeded or synthetic consumer.

**Verify:** Use existing accepted evidence when unchanged. Add a focused source or active-target check only for a new defect, profile change or concrete Hub handoff. Do not build or run deferred architecture rows.

**Done/failure:** Active-target consumer reliability is documented with explicit limits. This does not claim real TeslaMate parity or collection.

### S5 — ARM64/Mac selectable Swift developer bundle

**Depends on:** S2–S4 as applicable; Hub owns installer selection and runtime composition.

**Files/components:** Package.swift; VERSION; Examples/CurrentHubConsumer/**; README.md; docs/development.md; docs/current-hub.md; Dockerfile; .dockerignore; compatibility/hub.json; Hub product-selection records.

Keep Swift as an optional developer resource. Bind the exact SwiftPM source/product/profile identity already reviewed by Hub. The local Docker image is a package/test environment only and may be used for ARM64 development; it has no Hub socket, database, service manager or installed acceptance. Do not invent a Swift daemon package, and do not build or acquire an amd64 image for this scope.

**Verify:** `swift package dump-package`, the active-target consumer import/build when source changes, and `git diff --check`. Docker checks are ARM64-only and require the shared heavy-build lock plus an explicit active-target need. Existing green package evidence is retained rather than rerun unchanged.

**Done/failure:** Hub can select the optional Swift developer resource with exact source/product/profile identity. No archive, image, service, x86/amd64 target or installed lifecycle is implied by source selection.

### S6 — active Apple-silicon Mac and Debian ARM64 lifecycle closure

**Depends on:** S3/S5; a concrete Hub-owned active-target SessionInput, fixed Swift runner/launcher callbacks, observed runtime inventory and cleanup authority.

**Files/components:** Tests/TeslatlasCurrentHubTests/CurrentHubLiveTests.swift; CurrentHubMatrixWorkerTests.swift; CurrentHubMatrixWorkerSupportTests.swift; CurrentHubTestSupport.swift; tools/matrix_contract.py; tools/matrix_live.py; tools/matrix_wire.py; tools/swift_* schemas/contracts; compatibility/hub.json; Hub active-target receipts.

When Hub supplies the required handoff, exercise only the active selectors: supported Apple-silicon macOS and Debian 13 ARM64. Require real TLS, wrong-secret 401, claim/replay/rotation/revocation/re-pair, restart/outage recovery, cursor/ETag provenance, cumulative deadlines, process exit and exceptional cleanup. The existing iOS host evidence remains historical/secondary and does not block the normal Mac/ARM64 consumer. Use the fixed Hub-owned runner for installed work; a local package image is not an installed substitute.

**Verify:** Hub must report complete active-target Swift cells and receipts tied to the same source/product/profile/runtime identity. Run the SDK adapter gate or Swift tests only after relevant source or handoff changes. Do not run the deferred x86, amd64, Intel Mac, Azure or full cross-architecture rows.

**Done/failure:** Active Mac and Debian ARM64 rows are accepted only with target-specific lifecycle and cleanup evidence. Deferred rows remain neither passed nor failed and cannot block the active working-product objective.

## Deferred work (preserved, not current blockers)

- x86, x86_64, amd64 and Intel Mac compilation, image acquisition, VM provisioning, runtime/lifecycle tests and acceptance.
- The prepared macOS 13.7.4 guest as a Swift acceptance target because it is below the macOS 14 package floor.
- Azure, full cross-architecture matrix closure, polished broad distribution and final clean-host cross-architecture evidence.
- Any new VM, retained-guest repair, or use of the unadmitted Debian x86_64 guest.

These rows and their historical schemas/receipts remain intact. Reopening them requires a new owner direction; none may be used to block the supported Apple-silicon Mac or Debian ARM64 working product.
