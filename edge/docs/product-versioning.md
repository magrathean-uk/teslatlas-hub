# Product versioning

The Edge Cargo package carries the shared ecosystem product version
`2026.36.2`. The calendar number identifies the release cohort and does not
change Edge delivery envelopes, configuration format `1`, or the pinned
upstream Fleet Telemetry bridge. The current spool format is `3`; it is a
guarded forward-only correction over spool format `2` that binds receipt
deletion to the original admission.

`compatibility/hub.json` is accepted for product `2026.36.2` and the
independently authored `edge-delivery-v2@2.0.0` contract profile, manifest
SHA-256
`e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224`.
It binds one content-bound Hub source fingerprint and the accepted
[Hub G5](../../hub/docs/development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json)
and [Edge G5](development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json)
receipts. [G3 r2](../../teslatlas-protocol/docs/development/g3-compatibility-admission-2026-09-19-r2.json)
admitted and read back the exact record; its receipt SHA-256 is
`5df27073463ca985f409332a043b5f46aa753ba4b1b65fe214bc434521ef1865`.

G5 is bounded source-built synthetic evidence on Debian 13.6 ARM64. It is not
package, installer, service-manager, real Tesla data, App, production, or
full-platform acceptance, and it does not state that source was published or
tagged.
