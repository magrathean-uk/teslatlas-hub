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

The [Dockerfile](../Dockerfile) is a local Swift build and fixture-test
environment. It does not start a Hub, expose a port, use a database, mount a
Docker socket, or provide installed-matrix acceptance. The base image is the
official `swift:6.0.3-jammy` manifest pinned to
`sha256:1ad73b8f2a2300c650da0949519418565661d802765b9a99435df22bc947e2b4`.
The image installs `ca-certificates`, `libcurl4-openssl-dev`, `libssl-dev`,
and `python3`; the Linux transport therefore uses the system OpenSSL-backed
libcurl and its libssl/libcrypto libraries.

The build context is deliberately small. [`.dockerignore`](../.dockerignore)
starts with a deny-all rule and re-includes only package manifests, sources,
tests, fixtures, and the two source-only examples copied by the Dockerfile.
The external consumer's nested `.build` output, private configuration,
credentials, receipts, artifacts, virtual environments, and macOS build files
remain outside the context. Add a new input to both allowlists deliberately;
do not replace the explicit `COPY` instructions with `COPY .`.

With a running Docker Engine, the shortest checks are:

```sh
docker build -t teslatlas-swift-sdk .
docker run --rm teslatlas-swift-sdk
docker run --rm teslatlas-swift-sdk swift build -c release
```

The first `docker run` executes the non-live test selection as the unprivileged
`swiftuser`. Its output must contain a nonzero selected test count and zero
failures. The third command overrides the image `CMD` and checks a release
build. To build the public external consumer in the same environment:

```sh
docker run --rm teslatlas-swift-sdk sh -lc 'cd Examples/CurrentHubConsumer && swift build -c release'
```

Live tests remain an explicit, private-input operation. Mount a private config,
invitation, and CA only at run time when an owned Hub and reviewed trust setup
are available; do not copy them into the context, pass secrets as build args,
disable TLS validation, or assume container `localhost` is the Mac host. A
live container run still does not establish installed-matrix acceptance.

## Validation record

On 2026-09-08, the pinned manifest digest and Linux `amd64`/`arm64/v8` entries were
checked before using the explicitly selected `colima-interop-20260905` context. The
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

Only Linux arm64 was executed. This evidence covers package and local container
behavior; it does not establish Linux amd64, installed-Hub, live-Hub, iOS, or
physical-device acceptance.
