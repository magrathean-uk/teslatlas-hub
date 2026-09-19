# TypeScript SDK full-product completion plan — 2026-09-19

Objective: complete the public TypeScript SDK as a reproducible, caller-owned Node and
browser package covering every documented supported contract and the final installed
Hub ecosystem.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[product specification](../../../docs/development/PRODUCT_SPEC.md),
[coordination](../../../docs/development/COORDINATION.md), and [STATUS.json](STATUS.json).

## Current position

G3/G4 remain accepted for the exact packed `@teslatlas/sdk@2026.36.2` candidate and
closed synthetic Mac Hub cohort. Preserve those receipts unchanged. They do not prove
the full public SDK surface, all documented profiles, a durable distribution workflow,
support floors, named-source semantics or a combined installed ecosystem.

`full_solution_state` is `NOT_ACCEPTED`. F0 passed independent review; F3 and F6 are active.

Commit `d94a80a2ccc30e7ace9688b74a1ff444ac149082` is the accepted source
foundation. Its exact Node `26.7.0`/npm `11.19.0` clean archive has 83 members
and SHA-256 `42348d3688c5a723bd154e3c1e8172bc07b20d1bf28944818ccfdbf3d97891f7`.
The follow-on platform/package-lifecycle harness is independently accepted and does not
replace that accepted identity until the exact toolchain rebuild is reviewed.

## Required completion

- **F0:** produce a support ledger for root, Node and browser exports; current-Hub and
  rich profiles `1.0.0`–`1.2.0`; typed results/errors; ETag/304, opaque pagination,
  replay/gap and uncertainty behavior; metadata/command APIs; cancellation/disposal;
  TLS, invitation, identity, credentials and CORS; examples, Docker tooling and every
  Node/npm/browser/toolchain claim. Implement and prove each claim or correct it.
- **F3:** build the package reproducibly from the exact Protocol inputs and install it
  into clean external Node and browser consumers. Exercise the full claimed public
  behavior against the exact F1 Hub, including failure and recovery paths, without a
  source-tree import. Bind generated validators, exported types and runtime code to the
  admitted Protocol bytes.
- **F5:** consume the fresh named-source/import and passive-capture Hub results to prove
  the SDK preserves real field, unit, null/zero, pagination and history semantics. This
  input-dependent gate is mandatory for the full solution, not optional follow-up.
- **F6:** produce a deterministic unpublished npm archive with ordered contents,
  checksums, licences, documentation, examples, exact toolchain/support policy and
  clean install/update/removal. The Hub six-source catalog must install and retain this
  exact package for consumers. No npm publication is required without separate owner
  authority.
- **F7:** run fresh Node and real-browser consumers from the F6 artifact in the final
  combined installed ecosystem, including TLS/CORS, pairing, reads/mutations that the
  selected profile supports, restart, credential rotation/reauthentication and cleanup.

The SDK is a developer package, never a daemon. Its Docker path proves the documented
ARM64 development/consumer workflow only. The support ledger must state exact runtime
and browser floors from evidence rather than silently narrowing the claim to the
already accepted Chrome/Node tuple.

## Work slices

1. **L1:** finish F0 and close package/API defects needed by F3.
2. **L2:** prove all declared ARM64/macOS browser and Node support floors against F1.
3. **L3:** finish deterministic distribution/docs and catalog integration for F6,
   consume F5 real-input semantics, then pass F7.

## TS-02 independently accepted source/test boundary

At published commit `3cc72520c6d857b20e10129be039965f4ca1f4ab`, the public Node
factory supplies a fresh, non-pooled HTTPS claim transport that retains CA and
hostname verification, compares the invitation pin with the connected leaf's
DER SHA-256 during TLS identity verification, and sends the claim body only
after that handshake succeeds. A missing or malformed pin fails local
invitation validation before discovery; a pin mismatch is the typed
`hub_tls_pin_mismatch`; ordinary trust failures remain a body- and cause-free
`transport_error`. The browser factory retains normal Web PKI for reads and
rotation and rejects pairing before network I/O unless the caller supplies a
pin-capable claim transport.

Focused synthetic tests cover pin match, mismatch, absence, ordinary TLS trust
failure, unavailable browser transport, null-body statuses and invalid native
status handling. Independent GPT-5.6 Sol/high review accepted the exact
four-file source/test/documentation patch with no P1/P2 findings after exact
Node 26.7.0/npm 11.19.0 build, typecheck, Protocol lock, 55 focused tests and an
88-file package-content check passed. This accepts the SDK-owned TS-02
source/test boundary; it does not certify a new unpublished archive, satisfy F3
runtime acceptance against the final F1 Hub, provide a browser pairing bridge,
or broaden the platform floors.

## Accepted platform/package foundation

The first independent Sol/high review returned `REJECT` with two P1 and three
P2 findings. The same reviewer then accepted the closure delta with no P1/P2
findings. Three non-semantic Ajv `strictTypes` warnings remained in catalog
refinement schemas; explicit object types plus a strict, zero-warning compile
regression were then accepted by the same reviewer with no findings:

- **P1 candidate provenance:** the gates now derive a deterministic manifest
  from an exact frozen source export and require a separate independently
  accepted receipt binding source commit/digest/file count, archive
  hash/member count, package version and exact toolchain. The historical
  `42348d...91f7` archive is explicitly rejected as the changed candidate.
- **P1 predecessor provenance:** lifecycle admission now requires a separate
  independently accepted predecessor receipt, frozen source export and exact
  predecessor catalog cohort. Missing, draft or self-asserted evidence fails
  before a temporary consumer or npm work exists.
- **P2 catalog:** the Hub-owned schema-1 shape and its stricter validator
  require exact Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge
  identities, while the final target separately validates exact Hub
  source/artifact identity; placeholders, missing/extra fields and mismatches
  fail.
- **P2 input admission:** the JSON reader rejects duplicate keys, while the
  bounded tar reader rejects duplicate members, links, special entries, unsafe
  paths/modes and malformed payloads before npm or Docker.
- **P2 cleanup:** Docker cleanup always attempts task-owned image cleanup and
  temporary-context cleanup, aggregating failures from both.

- The official Node OCI index is immutable and its Linux ARM64/v8 child is
  recorded from a registry readback. The package-only Docker lane rejects any
  non-native Linux ARM64 engine and contains no SDK repository source.
- Node `26.7.0`, npm `11.19.0`, and Chrome `153.0.8010.52` on macOS 27 ARM64 are
  recorded as the accepted evidence tuple. Node/npm/browser floors remain null
  and open; null does not mean unbounded support.
- The lifecycle gate requires exact candidate and predecessor admissions,
  frozen source manifests and catalog cohorts before any npm work. It covers
  fresh install, update, rollback and removal, and cannot turn same-candidate
  reinstall into a predecessor lifecycle claim.
- The final consumer gate installs the exact archive into a fresh external
  Node consumer and reuses the existing strict Node-pin and real-browser gates.
  It requires the future F1 Hub descriptor to bind an exact final artifact and
  does not start Hub.
- F3 runtime/platform floors, F5 input semantics, F6 native ARM64 Docker,
  exact predecessor, non-empty final catalog and source bootstrap, and F7
  combined installed Node/browser acceptance all remain open.

## Independently accepted candidate-only package

The exact clean Git export of commit
`56a07dd7c1a55e5f783ab71640a01f4b56d908c4` produced byte-identical packages in
two independent Node `26.7.0`/npm `11.19.0` builds. The unpublished 88-member
candidate is SHA-256
`070906b5e3ead04a32223ca996d88ebf6f22be252821e56ef1839da3a13e23d7`.
Its 285-file deterministic source manifest is SHA-256
`546b517f6ce86556252e7fbcde19bee320d7e30017e3bd4d8ad075220b939d2b`.
Fresh external Node and browser imports plus removal passed, and the
package-cleanliness harness now includes the packaged `tools/` directory and
reproduces the same archive hash.

Independent GPT-5.6 Sol/high review accepted the exact source, toolchain,
archive, receipt, cleanup and claim-integrity slice with no P1/P2 findings. This
is candidate-only admission, not F3 or F6 acceptance. There is still no valid
older predecessor or non-empty Hub catalog, so update, rollback, catalog and
source-bootstrap acceptance remain blocked. Docker/VM, browser-floor, final-Hub,
F5 and F7 evidence were not run or claimed. The exact receipt is
[`f3-f6-candidate-package-2026-09-19-r1.json`](f3-f6-candidate-package-2026-09-19-r1.json).

## Start and boundaries

The sent goal authorizes bounded source changes, generation, tests, unpublished
packages/runtimes and validated source commits/pushes. It does not authorize npm
publication, releases, tags, CI, production or private-input reuse. Preserve the dirty
`main` tree and accepted G3/G4 artifacts. Exclude App, Viewer, x86/amd64/Intel and
Azure. Never embed or retain Hub, Tesla or pairing credentials.
