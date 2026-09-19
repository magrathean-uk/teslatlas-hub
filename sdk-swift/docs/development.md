# Development

Run SwiftPM commands from the repository root. The normal local verification
selection leaves live and installed-matrix journeys opt-in:

```sh
swift package dump-package
swift test --skip CurrentHubLiveTests --skip CurrentHubMatrixWorkerTests --skip CurrentHubAppleConsumerTests --skip LiveHubBlackBoxTests
swift build -c release
swift test -c release --skip CurrentHubLiveTests --skip CurrentHubMatrixWorkerTests --skip CurrentHubAppleConsumerTests --skip LiveHubBlackBoxTests
uv run --python 3.11 --with 'jsonschema[format-nongpl]>=4.26,<5' python -m unittest discover -s tools -p 'test_matrix_adapter.py'
```

The adapter check requires Python 3.11 or newer and the pinned development
requirement in `tools/requirements-dev.txt`; an equivalent isolated virtual
environment is fine when `uv` is unavailable.

`CurrentHubLiveTests`, `CurrentHubMatrixWorkerTests`, and
`LiveHubBlackBoxTests` require separately provisioned inputs and a reachable
Hub. They are not made safe or meaningful by running inside this image. The
image's default command also skips `CurrentHubAppleConsumerTests` when that
SDK-owned Apple-only suite is present.

## Linux Swift with Docker

The [Dockerfile](../Dockerfile) is the native Linux ARM64 platform-gate image.
It does not start a Hub, expose a port, use a database, mount a Docker socket,
or provide installed-matrix acceptance. It pins Docker Official Image
`swift:6.0.3-jammy` directly to the `linux/arm64/v8` child
`sha256:c84da0197afcc90ef90a64194d4d451be7c090a845bcbf632755f9c16334ba8f`.
The locked parent OCI index is
`sha256:e2b0410500126d7f569d387b5817426cef5c38cc02dc494c3dc5edc8e10304d6`.
The image installs `ca-certificates`, `libcurl4-openssl-dev`, `libssl-dev`,
and `python3`; the Linux transport therefore uses the system OpenSSL-backed
libcurl and its libssl/libcrypto libraries.

The live repository is not a valid context for this Dockerfile. The gate
materializes published commit `d7ac4488fc5908015e8de55cd57983ea87172266`,
prepares and verifies the accepted 124-file handoff, and supplies only its
canonical `teslatlas-sdk-swift` directory plus its separately checksummed
`external-four-library-consumer`. The Dockerfile copies only those two roots.
SwiftPM build output goes to `/tmp`; the package input remains unchanged.

The current-host, non-runtime checks are:

```sh
python3 tools/platform_gate.py verify
python3 tools/platform_gate.py command linux-arm64
```

The run command is authorized only on a native Linux ARM64 host with a Docker
Engine that independently reports Linux ARM64:

```sh
python3 tools/platform_gate.py run linux-arm64
```

The runner rejects emulation, never invokes `matrix_wire.py`, builds the
separately checksummed four-library consumer, then removes the temporary
handoff and owned image tag. It accepts no private inputs and does not run live
tests. This lane remains preparation until exact-floor execution and review.

## Validation record

Historical evidence only: on 2026-09-08, the then-pinned multi-architecture
manifest was checked before using the explicitly selected
`colima-interop-20260905` context. The
context supplied Docker client 29.8.0, Engine 29.5.2, Linux arm64, and kernel
6.8.0-117-generic. The buildx component was unavailable, so the documented build ran
with Docker's legacy builder and the image was tagged
`teslatlas-swift-sdk:20260908-arm64` (image ID
`sha256:254cd5a67ffc0e947159f9f38682b10cd3c4ab71b8608ec836ba32e5f052b565`). The
image runs as `swiftuser` UID/GID 10001 from `/workspace/teslatlas-sdk-swift`.

The default command completed with 148 selected XCTest tests and zero failures. A
release package build and the external `Examples/CurrentHubConsumer` release build
also completed successfully. Runtime inspection recorded Swift 6.0.3 targeting
`aarch64-unknown-linux-gnu`, libcurl 7.81.0 with its OpenSSL 3.0.2 TLS backend,
libcurl4/libssl3 package versions `7.81.0-1ubuntu1.27` and `3.0.2-0ubuntu1.29`, and
Python 3.10. All owned `--rm` containers were absent after the checks. The full
receipts and logs are retained in
`/Users/bolyki/.codex/artifacts/teslatlas-interop/2026-09-08-task10-swift-adapter-current-final/`.

Only Linux ARM64 was executed in that historical run. It does not accept the
new immutable handoff-input lane, installed Hub, live Hub, iOS, or physical
device behavior.
