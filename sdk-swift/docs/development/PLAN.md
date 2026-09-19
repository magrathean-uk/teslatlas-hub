# Swift SDK full-product completion plan — 2026-09-19

Objective: complete all three public SwiftPM products on their documented supported
platforms and prove the exact package in the final installed Hub ecosystem.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[product specification](../../../docs/development/PRODUCT_SPEC.md),
[coordination](../../../docs/development/COORDINATION.md), and [STATUS.json](STATUS.json).

## Current position

G3/G6/G7 remain accepted within their exact recorded scope. The maintained
`TeslatlasCurrentHub` consumer passed on macOS 27 arm64; that does not prove macOS 14,
iOS 17, supported Linux ARM64, the other public Swift products, reproducible package
handoff, real-source semantics or final combined installation. Preserve all accepted
receipts. `full_solution_state` is `NOT_ACCEPTED`; this plan is not started.

## Required completion

- **F0:** inventory all documented behavior in `TeslatlasCurrentHub`,
  `TeslatlasHubSDK` and `TeslatlasHubV1Compatibility`: discovery/auth/trust,
  current/history, rich queries/events/metadata/commands, errors, pagination/ETag,
  cancellation, limits, credential stores and all examples. Record and resolve the
  Swift 6, iOS 17+, macOS 14+ and supported Linux ARM64 build/runtime claims, including
  Security/URLSession and OpenSSL/libcurl differences.
- **F3:** use clean external SwiftPM consumers to prove all public products and their
  supported recovery behavior against the exact F1 Hub and F3 Protocol. Exercise
  macOS 14 Apple silicon, iOS 17 with an SDK-owned harness where needed to substantiate
  the existing SDK claim, and the documented Linux ARM64 native/container path. The
  harness must not inspect or modify the App.
- **F5:** consume fresh named-source/import and passive-capture Hub evidence and prove
  Swift decoding preserves real units, null/zero/unknown values, history pagination
  and supported metadata/event semantics. This external-input gate is mandatory.
- **F6:** create a deterministic source-only SwiftPM handoff with exact manifest,
  checksums, licences, examples, toolchain/platform policy and clean resolve/build/
  update/removal. Integrate that exact source into the Hub six-repository catalog.
  Tagging or publication requires separate owner authority.
- **F7:** run external consumers built only through the F6 package in the combined
  installed ecosystem, covering pairing/trust, supported reads and mutations, restart,
  credential rotation/reauthentication, recovery and cleanup.

## Work slices

1. **L1:** complete F0 and repair any public-surface/package gaps.
2. **L2:** prove macOS 14, iOS 17 and supported Linux ARM64 claims for F3.
3. **L3:** finish reproducible source distribution/catalog/docs for F6, consume F5
   semantics, and pass the Swift portion of F7.

## Start and boundaries

This plan does not authorize VM/device start, source change, package resolution,
build, test, runtime, commit, push, tag or publication. Preserve the dirty `main` tree,
contract separation and accepted receipts. Exclude App, Viewer, x86/amd64/Intel and
Azure. Use only SDK-owned consumers/harnesses and fresh one-use runtime inputs.
