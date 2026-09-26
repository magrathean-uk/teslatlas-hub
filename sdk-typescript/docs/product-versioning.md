# Product versioning

The npm package carries the shared ecosystem product version `2026.36.2` in
`package.json` and `package-lock.json`. The package remains private, and this
bounded compatibility number does not authorize registry publication.

The calendar product number is independent of richer protocol profile `1.2.0`
and the separate `hub-http-v1@1.0.0` profile. `protocol/lock.json` pins the
immutable richer input and generated output by SHA256. The current-Hub
binding is also content-addressed: its input bundle is
`68dca68355cd1011c31004f7d39eaabfef068706fb89ef0a5823496f26d3727e`, its
generated output is
`3769abc73e927d20cd5842eed3a076b05bf436d1eba3026e324b5d5dfbe7c6b9`, and the
profile bundle is
`b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`.

`compatibility/hub.json` is accepted for product `2026.36.2`. It binds the
profile digest above to one content-bound Hub source fingerprint and the
historical Hub G2/G4
and [TypeScript G4](https://github.com/magrathean-uk/teslatlas-sdk-typescript/blob/main/docs/development/macos-arm64-packed-node-browser-g4-2026-09-18-r1.json)
receipts. [G3 r2](https://github.com/magrathean-uk/teslatlas-protocol/blob/main/docs/development/g3-compatibility-admission-2026-09-19-r2.json)
admitted and read back that exact record; its receipt has SHA-256
`5df27073463ca985f409332a043b5f46aa753ba4b1b65fe214bc434521ef1865`.

The G4 evidence is the reviewed 81-member package archive on Node 26.7.0 and
Chrome 153 against a source-built synthetic Hub on macOS 27 Apple silicon. It
does not establish an installed Hub service, registry publication, real Tesla
data, App integration, the documented macOS 13 Hub floor, or a full platform
matrix. No source publication or tag is implied.

## Reading historical acceptance

The compatibility record is an exact historical cohort, not a compatibility promise for every Hub with the same version string. The later [Docker receipt](https://github.com/magrathean-uk/teslatlas-sdk-typescript/blob/main/docs/development/f6-native-linux-arm64-docker-package-2026-09-20-r1.json) records a package-only pass for archive `070906b5e3ead04a32223ca996d88ebf6f22be252821e56ef1839da3a13e23d7`. The [development record](https://github.com/magrathean-uk/teslatlas-sdk-typescript/blob/main/docs/development/PLAN.md) explains the subsequent removal of local artifacts.

`protocol/lock.json` still labels the current-Hub input `candidate`; `compatibility/hub.json` records an accepted bounded product cohort. These fields describe different records. Do not relabel or broaden either record through a documentation change.

The Hub G2/G4 receipt path recorded in `compatibility/hub.json` is absent from the current workspace. The compatibility record and retained SDK receipt preserve the reference; this guide does not claim a fresh readback of that missing Hub receipt.
