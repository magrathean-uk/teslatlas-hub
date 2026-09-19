# Product versioning

This repository carries the shared Teslatlas Hub ecosystem product version
`2026.36.2` in `YEAR.WEEK.REVISION` form. G3 r2 accepted the bounded
compatibility records for this cohort. The number alone still does not imply a
tag, package publication, release artifact, or support outside those records.

The product version appears in `VERSION`, `pyproject.toml`, and the root package
entry in `uv.lock`. It does not replace the rich semantic profiles `1.0.0`,
`1.1.0`, or `1.2.0`, the current-Hub HTTP profile `hub-http-v1@1.0.0`, or the
Edge delivery profile `2.0.0`. These identities are selected independently.

The current-Hub profile is already exported at
`profiles/hub-http-v1/1.0.0/`. Its `version` discovery field reports the Hub's
product version; it does not advertise the profile revision. Its OpenAPI 3.1.0
document uses adjacent schemas, so consumers must bind the complete profile
directory by its manifest digest.

`compatibility/hub.json` is accepted for product `2026.36.2` and
`hub-http-v1@1.0.0`, manifest SHA-256
`b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`.
Its tested Hub version, content-bound source fingerprints, and G4/G5/G6 receipt
paths were admitted by
[`g3-compatibility-admission-2026-09-19-r2.json`](development/g3-compatibility-admission-2026-09-19-r2.json).
That metadata acceptance retains each receipt's synthetic runtime and platform
limits; it is not installed-service, publication, real-data, App, or full-matrix
acceptance.
