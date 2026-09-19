# Compatibility record

[`compatibility/hub.json`](../compatibility/hub.json) is the machine-readable
record for this repository's current-Hub HTTP claim. G3 r2 accepted the record
for product `2026.36.2`, profile `hub-http-v1@1.0.0`, and manifest SHA-256
`b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`.
The tested versions, content-bound Hub source fingerprints, and six G4/G5/G6
receipt paths are populated from accepted bounded evidence.

The record's identities have separate meanings:

| Field | Meaning |
| --- | --- |
| `product_version` | Hub ecosystem product cohort, chosen with the Hub owner |
| `profile.id` / `profile.revision` | `hub-http-v1` wire profile identity |
| `profile.sha256` | SHA-256 of the exact `SHA256SUMS` bytes in the complete profile directory |
| `tested_hub_versions` | Hub product versions exercised by accepted evidence |
| `tested_hub_source_fingerprints` | Content-bound Hub source identities for those runs |
| `required_capabilities` / `optional_capabilities` | Discovery capabilities required or conditionally used by the client |
| `test_receipt_paths` | Redacted, repository-local or otherwise reviewable evidence references |

Do not fill or widen these fields from a generated bundle alone. A verified
record requires exact profile, source/artifact, and accepted receipt bindings.
Keep invitations, bearer values, cursors, raw vehicle data, and private
descriptors outside Git.

Hub owns the shared checker that admitted and reads back the tested fields.
Protocol owns this profile's content and local validation; the active SDK and
Edge owners bind their own artifacts and report their own acceptance. Do not
edit sibling pins or infer a broad supported-version range from one product
cohort.

The active evidence is
[`g3-compatibility-admission-2026-09-19-r2.json`](development/g3-compatibility-admission-2026-09-19-r2.json)
(SHA-256
`5df27073463ca985f409332a043b5f46aa753ba4b1b65fe214bc434521ef1865`).
It is metadata admission, not a new runtime. Its accepted scope is the recorded
macOS 27 Apple-silicon and Debian 13.6 ARM64 synthetic journeys. Installed or
release artifacts, declared minimum floors, real Tesla data, App integration,
and the full platform matrix remain unaccepted. The admission is not evidence
that any source was published or tagged.
