# Verification evidence and limits

This page records the current bounded evidence for product `2026.36.2`. It
distinguishes accepted source/runtime journeys from installation, publication,
real-data, and full-platform claims that have not been accepted.

Neither a published profile nor a passing conformance run establishes that a
current installed Hub, SDK, or App implements it.

## Profile status

The table separates what a Hub serves from what a runtime has accepted. A serving
state describes the retained profile files and the Hub source; it is not an
installed-Hub, App or SDK acceptance.

| Contract | Recorded position |
| --- | --- |
| Current-Hub HTTP `hub-http-v1@1.2.0` | Served. A fresh native conformance run through the interop smoke passed 61 of 61 cases, complete, from the G2-2 P1 commit: native 61/61, receipt `receipt-conformance-1.2.0-<P1 sha>.md` (PROVISIONAL until R-P runs). The offline profile checks run in `./tools/check`. |
| Hub sync `hub-sync-v1@1.3.0` | Served (candidate contract). 65 deterministic conformance cases run in `./tools/check` and `./conformance/run`. Hub and App runtime admission is separate and remains open. |
| Hub sync `hub-sync-v1@1.4.0` | Served through its selector (candidate contract). Its profile files are retained and digest-bound; the unit suites run in `./tools/check` and the conformance runner does not run it. Hub and App runtime admission is separate and remains open. |
| Hub sync `hub-sync-v1@1.5.0` | Candidate, not served: no Hub serves the live overlay yet. Its profile files are retained and digest-bound; the unit suites run in `./tools/check`. No runtime acceptance is claimed. |
| Receiver envelope `receiver-v1@1.0.0` | Served by the Hub receiver route. Evidence: the independent profile tests T1 (`tests/test_receiver_profile.py`) and the Hub route tests T2 (`receiver_answers_the_profile_vectors` in `src/api/server/tests/receiver.rs`) against the same vectors. Edge delivery (`edge-delivery-v2@2.0.0`) was retired; its dated evidence below is historical. |

## Canonical bindings

| Contract | Revision | Manifest SHA-256 |
| --- | --- | --- |
| Current-Hub HTTP | `hub-http-v1@1.2.0` | `7aa26deeaa9052bc77e0a6f58b89422fa21ef0ec2fd53cec1adab0361db5f7e1` |

## Accepted bounded evidence

| Gate | Target and consumer | Accepted behavior | Receipts | Limit |
| --- | --- | --- | --- | --- |
| G4 | macOS 27.0 Apple silicon; packed `@teslatlas/sdk@2026.36.2`; Node 26.7.0 and Chrome 153 | Normal TLS, browser rejection before scoped trust and after trust removal, seven CORS preflights, discovery/readiness, claim/replay rejection, two current results, drives `2/2/1`, cursor plus `304`, and credential rotation | Hub `active-macos-arm64-hub-typescript-g2-g4-2026-09-18-r1.json`; TypeScript `macos-arm64-packed-node-browser-g4-2026-09-18-r1.json` | Source-built synthetic Hub and reviewed packed SDK only; no installer, service, real data, or minimum-floor result |
| G5 | Debian GNU/Linux 13.6 ARM64; source-built Edge and Hub | One guarded synthetic event survived Hub outage and Edge restart, retained stable retry identity, committed before occurrence-bound ACK, projected exactly one record, drained pending state, and survived Hub restart | Hub and Edge `g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json` | Synthetic source-built lane only; no package, installer, service manager, Tesla account, or vehicle |
| G6 | macOS 27.0 Apple silicon; Swift 6.4 external SwiftPM consumer | Exact discovery/profile/product/capabilities, normal TLS, claim/replay rejection, health/readiness, two current results, drives `2/2/1` with three `304`s, rotation, old-token rejection, and restart continuity | Hub `g6-macos-arm64-swift-hub-acceptance-2026-09-19-r2.json`; Swift `g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json` | Synthetic source-built external consumer only; declared macOS 14 SDK and macOS 13 Hub floors were not exercised |

The accepted cohorts were closed with their owned private roots, processes,
listeners, credentials, certificates, and build lock absent as recorded in the
receipts.

## What current checks establish

| Check | Evidence produced | Limit |
| --- | --- | --- |
| `./tools/check` | Retained-generator consistency, unit checks, and the 65 `hub-sync-v1@1.3.0` fixture cases | No installed or remote product acceptance |
| `./conformance/run --profile hub-http-v1@1.2.0 --config <descriptor>` | Behavior of the owned Hub described by the supplied private descriptor | Limited to the exact source, artifacts, host, cases, and receipts |
| `tools/check-current-hub` | Bounded read-only remote-wire smoke, held to `hub-http-v1@1.2.0` (discovery must name it) | No pairing, lifecycle, native process provenance, or complete-history proof |
| Docker gate | Checks inside the exact built image | No container-to-Hub or installed-service acceptance by itself |

## What remains unaccepted

- No App integration or Viewer acceptance is claimed by these gates.
- No installer, notarization, package-service, service-manager, or release
  artifact acceptance is claimed.
- No Tesla account, real vehicle, named-source collection, real-data parity,
  production, or public-ingress acceptance is claimed.
- Declared minimum OS floors and a full platform matrix were not exercised.
- x86, x86_64, amd64, and Intel targets remain outside the active scope.

Run local schema and conformance checks when changing contract bytes, but do
not treat those checks as substitutes for the accepted runtime receipts above.
The September runtime fixtures and external receipts referenced by historical
metadata were later removed; their bindings remain provenance, not proof of a
current running installation. Fresh implementation claims need a new run tied
to the exact profile digest, source or artifact identity, platform, invocation,
result, and cleanup.
