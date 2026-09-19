# Swift SDK full-product completion plan — 2026-09-19

Objective: complete all four public SwiftPM libraries on their documented supported
platforms and prove the exact package in the final installed Hub ecosystem.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[product specification](../../../docs/development/PRODUCT_SPEC.md),
[coordination](../../../docs/development/COORDINATION.md), and [STATUS.json](STATUS.json).

## Current position

G3/G6/G7 remain accepted within their exact recorded scope. The maintained
`TeslatlasCurrentHub` consumer passed on macOS 27 arm64; that does not prove macOS 14,
iOS 17, supported Linux ARM64, the other public Swift products, reproducible package
handoff, real-source semantics or final combined installation. Preserve all accepted
receipts. `full_solution_state` is `NOT_ACCEPTED`; F0 passed independent review, the
bounded F3/F6 source foundation is published, and the next F3 platform harnesses are
prepared but pending independent review and exact-floor execution.

## Accepted bounded F3/F6 foundation

Published commit `d7ac4488fc5908015e8de55cd57983ea87172266` updates only the
live strict Protocol pin to runtime-source commit
`53b5c6483990db84e5214176755f398e93d87b1b`; the Docker evidence-only
descendant is not used as the source identity. Historical Hub-v1 and earlier
handoff evidence retains its original Protocol references.

The candidate also adds a deterministic source-only handoff for all four
libraries. Initial independent review rejected the first freeze because source
selection followed a symlinked root, verification did not reject every special
entry or unexpected directory, JSON duplicate keys were accepted, and staged
metadata depended on the caller's umask. The corrected delta rejects symlinked
selection roots and escaped resolved inputs, performs an exact `lstat`-based
tree inventory without following links, rejects duplicate JSON keys, and fixes
and verifies directory mode `0755`, regular-file mode `0644`, and modification
time `2000-01-01T00:00:00Z` under both umask `022` and `077`.

The corrected handoff stages exactly 124 inputs (769736 bytes) under the
canonical `teslatlas-sdk-swift` basename and builds a separate SwiftPM consumer
importing all four products. The exercised candidate identity is
`733071fcd8c7db0e64b38547dab90dd354ad728dc2d9efee7fd2de438a14876e`.
The temporary stage and build scratch were cleaned. Same-reviewer Sol/high
delta review accepted the corrected bounded foundation with no remaining P1 or
P2 findings. This is source/package preparation on macOS 27 arm64, not macOS
14, iOS 17, Linux ARM64, live Hub, catalog lifecycle, F5, or final F3/F6/F7
acceptance; see [receipt r1](f3-f6-source-handoff-foundation-2026-09-19-r1.json).

## Review-pending F3 platform-gate preparation

From the clean published foundation HEAD, the review candidate adds only the
missing platform-harness prerequisites. The SDK-owned iOS host now has a shared
`CurrentHubRuntimeHost` scheme whose app launch imports and exercises all four
libraries before its test bundle runs. Its deterministic invocation requires an
exact available iOS 17.0 iPhone 15 simulator and isolated DerivedData. No
simulator or iOS build/test ran during preparation.

The macOS lane prepares the accepted source-only handoff in a temporary root and
builds its isolated all-four external consumer only after proving the host is
native arm64 and exactly macOS major 14. It therefore rejects the current macOS
27 host as floor evidence. The Linux lane pins the official Swift 6.0.3 Jammy
`linux/arm64/v8` child digest, refuses non-native Linux ARM64 hosts and Docker
Engines, and does not invoke `matrix_wire.py` or any emulation route. No Docker
build or container ran during preparation.

`tools/platform_gate.py verify` fails closed on the accepted Protocol identity,
the `733071f...` source-handoff identity, canonical root, 124 inputs, 769736
bytes, four product names, shared iOS scheme and direct ARM64 image digest.
The accepted handoff regression suite passed 7/7, focused platform preparation
checks passed 11/11, and the external platform consumer manifest parsed on
macOS 27 arm64. This is not macOS 14, iOS 17, native Linux
ARM64, live-Hub, catalog, F5, F3, F6 or F7 acceptance. See the
[preparation notes](platform-gate-preparation.md) and the review-pending draft
receipt.

Initial independent Sol/high review rejected this preparation with one P1 and
two P2 findings. The P1 was that the Linux command verified and discarded the
accepted handoff, then built the live repository with a consumer outside that
identity. The corrected lane now materializes the immutable published source
commit, reproduces the accepted identity, and supplies that canonical package
plus the handoff's separately checksummed external consumer as the actual
Docker context; both inputs are root-owned/read-only to the unprivileged build
user and SwiftPM scratch is isolated under `/tmp`.

The P2 documentation finding is closed by replacing superseded live-repo,
multi-architecture, old-digest and two-example instructions in `README.md` and
`docs/development.md` with the direct ARM64 child, parent-index provenance,
actual handoff inputs and explicit no-emulation rule. The P2 validation finding
is closed by exact-value checks for the official catalog URL and Dockerfile
source commit plus mutation regressions. The same reviewer accepted the frozen
three-closure delta with no findings; no platform runtime ran. This accepts only the bounded
platform-gate preparation, not macOS 14, iOS 17, Linux ARM64 runtime behavior,
live Hub, catalog lifecycle, F5, F3, F6 or F7.

## Required completion

- **F0:** inventory all documented behavior in `TeslatlasCurrentHub`,
  `TeslatlasHubSDK`, `TeslatlasCommands` and `TeslatlasHubV1Compatibility`:
  discovery/auth/trust,
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
2. **L2:** prove macOS 14, iOS 17 and supported Linux ARM64 claims for F3. The
   current source-only consumer build does not close this slice.
3. **L3:** finish reproducible source distribution/catalog/docs for F6, consume F5
   semantics, and pass the Swift portion of F7.

## Start and boundaries

The sent goal authorizes bounded SDK-owned VM/harness work, source changes, package
resolution, builds/tests, unpublished runtimes and validated source commits/pushes.
It does not authorize tags, releases, binary publication, CI, production or App use.
Preserve the dirty `main` tree, contract separation and accepted receipts. Exclude
App, Viewer, x86/amd64/Intel and Azure; use fresh one-use runtime inputs.
