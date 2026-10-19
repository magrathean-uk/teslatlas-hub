# Product versioning

The npm package carries the shared ecosystem product version `2026.36.2` in
`package.json` and `package-lock.json`. The package remains private, and this
bounded compatibility number does not authorize registry publication.

The calendar product number is independent of the `hub-http-v1` profile the client
targets. The client reads profile `1.2.0` in place from
`../protocol/profiles/hub-http-v1/1.2.0`, and `npm run protocol:check` verifies that
profile's `SHA256SUMS` and the generated outputs.
