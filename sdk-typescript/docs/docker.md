# Docker package gate

Docker is a native Linux ARM64 package-consumer lane for this private SDK. It
does not build from repository source, publish an image, or start a Hub.

The base is the official Node `26.7.0-bookworm-slim` OCI index pinned at
`sha256:4db36457f406501e6f608802e5da617e5fbd0e80b75901b6a09de1ae5a667d32`.
The registry index readback contains Linux ARM64/v8 child
`sha256:2b028cd57303b2761d24173789c85a013558d6cf20e78f51723385f368b6e34d`.
[`tools/node-image-lock.json`](../tools/node-image-lock.json) records that
provenance. This registry readback is not a build or runtime pass.

## Run the lane

First create the clean deterministic archive with the exact Node `26.7.0` and
npm `11.19.0` toolchain and record its SHA-256. Then, on a native Linux ARM64
Docker engine, run:

```bash
npm run gate:docker-package -- \
  --tarball /absolute/private/teslatlas-sdk-2026.36.2.tgz \
  --sha256 EXACT_LOWERCASE_SHA256 \
  --candidate-receipt /absolute/private/candidate-admission.json \
  --candidate-receipt-sha256 EXACT_RECEIPT_SHA256 \
  --source-export /absolute/private/candidate-source-export \
  --catalog /absolute/private/catalog.json \
  --catalog-sha256 EXACT_CATALOG_SHA256
```

The runner validates the archive, frozen source export, independently accepted
admission receipt and exact five-companion catalog before contacting Docker. Its
strict tar admission rejects duplicate members, links, special entries, unsafe
paths and unsafe modes before npm or Docker can consume the archive. It refuses
a Docker engine whose server platform is not `linux/arm64`, and the Dockerfile separately
requires `BUILDPLATFORM`, `TARGETPLATFORM`, and `uname -m` to be native ARM64.
Emulation and cross-architecture builds are outside this lane.

The runner creates a private temporary context containing only:

- the exact admitted `teslatlas-sdk.tgz` archive;
- the bounded installed-package smoke check;
- the five external Hub consumer files; and
- the package-only Dockerfile.

No `src/`, `protocol/`, build output, lock install, test tree, or other live
repository source enters the context. The Dockerfile verifies the archive hash,
installs it with scripts disabled, reads back package metadata and imports all
three public entry points. The runner reads the resulting image identity, then
always attempts both task-owned image and temporary-context cleanup. Cleanup
failures are aggregated so one cannot prevent the other; any cleanup failure
fails the lane.

## Runtime boundary

The final Node and browser Hub journey is separate. Use
`gate:final-hub-consumers` only after Hub supplies an exact final-artifact
handoff and the non-empty exact companion catalog is available. That gate
installs the same archive into a fresh external consumer and reuses the strict
Node pin and real-browser Web PKI/CORS gates. It does not start Hub.

Current accepted runtime evidence is limited to Node `26.7.0`, npm `11.19.0`
and Chrome `153.0.8010.52` on macOS 27 ARM64. No broader Node, npm, browser, or
operating-system floor is claimed. Docker ARM64 execution remains pending.
