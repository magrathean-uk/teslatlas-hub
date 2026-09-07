# Product versioning

The npm package carries the shared ecosystem product version `2026.36.2` in
`package.json` and `package-lock.json`. The package remains private, and this
candidate number does not authorize registry publication.

The calendar product number is independent of richer protocol profile `1.2.0`
and the separate `hub-http-v1@1.0.0` candidate. `protocol/lock.json` pins the
immutable richer input and generated output by SHA256 while keeping the
current-Hub input and output hashes null until that profile is exported.

`compatibility/hub.json` contains an untested candidate binding. Empty tested
version, source fingerprint, profile hash, and receipt fields mean that no Hub
compatibility is currently claimed.
