# Verification evidence and limits

This page records the current bounded evidence for product `2026.36.2`. It
distinguishes accepted source/runtime journeys from installation, publication,
real-data, and full-platform claims that have not been accepted.

Neither a published profile nor a passing reference adapter establishes that a
current installed Hub, SDK, or App implements it.

## Profile status

| Contract | Recorded position |
| --- | --- |
| Rich semantic HTTP and events | The local gate covers `1.0.0`, `1.1.0`, and `1.2.0`. |
| Current-Hub HTTP `hub-http-v1@1.0.0` | Historical accepted binding retained below and in [`compatibility/hub.json`](../compatibility/hub.json). |
| Current-Hub HTTP `hub-http-v1@1.1.0` | Candidate successor; the earlier acceptance record does not admit it. |
| Hub sync `hub-sync-v1@1.3.0` | Candidate changes-since contract with 49 deterministic conformance cases; Hub and App runtime admission remains open. |
| Edge delivery `edge-delivery-v2@2.0.0` | Separate delivery contract with bounded historical synthetic evidence. |

## Canonical bindings

| Contract | Revision | Manifest SHA-256 |
| --- | --- | --- |
| Current-Hub HTTP, G3 r2 accepted | `hub-http-v1@1.0.0` | `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926` |
| Current-Hub HTTP, candidate | `hub-http-v1@1.1.0` | `47278485e4962aaa3ae177a3fc916281191b0d066edd1b40aad73fdad20b0f85` |
| Edge delivery | `edge-delivery-v2@2.0.0` | `e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224` |

G3 r2 admitted and read back the five active compatibility records for
`hub-http-v1@1.0.0`; it does not admit the `1.1.0` candidate. The receipt
is
[`g3-compatibility-admission-2026-09-19-r2.json`](development/g3-compatibility-admission-2026-09-19-r2.json)
(SHA-256
`5df27073463ca985f409332a043b5f46aa753ba4b1b65fe214bc434521ef1865`).
The focused six-case checker proved active-product-only operation, idempotence,
fail-closed receipt/profile identity checks, divergence rejection, exact
readback, and byte identity of the TypeScript vendored Protocol record.

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
| `./tools/check` | Generated-artifact consistency, unit checks, and reference conformance cases | No installed or remote product acceptance |
| Reference adapter | Agreement between the runner and embedded vectors | No independently implemented server is exercised |
| Actual-Hub adapter | Behavior of the owned Hub described by the supplied private descriptor | Limited to the exact source, artifacts, host, cases, and receipts |
| `tools/check-current-hub` | Bounded read-only remote-wire smoke | No pairing, lifecycle, native process provenance, or complete-history proof |
| Developer bundle verification | Archive structure, membership, and content integrity | No runtime or installation evidence |
| Docker gate | Checks inside the exact built image | No container-to-Hub or installed-service acceptance by itself |

## What remains unaccepted

- No App integration or Viewer acceptance is claimed by these gates.
- No installer, notarization, package-service, service-manager, or release
  artifact acceptance is claimed.
- No Tesla account, real vehicle, named-source collection, real-data parity,
  production, or public-ingress acceptance is claimed.
- Declared minimum OS floors and a full platform matrix were not exercised.
- x86, x86_64, amd64, and Intel targets remain outside the active scope.
- The compatibility admission does not state that source was published or
  tagged.

Run local schema and conformance checks when changing contract bytes, but do
not treat those checks as substitutes for the accepted runtime receipts above.
The September runtime fixtures and external receipts referenced by historical
metadata were later removed; their bindings remain provenance, not proof of a
current running installation. Fresh implementation claims need a new run tied
to the exact profile digest, source or artifact identity, platform, invocation,
result, and cleanup.
