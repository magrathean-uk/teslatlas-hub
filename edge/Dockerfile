# syntax=docker/dockerfile:1.7

FROM rust:1.98-bookworm AS edge-build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:13-slim AS bridge-build
ARG GO_VERSION=1.27.0
ARG TARGETARCH
ARG GO_SHA256_AMD64=675c26c449cbb18fc24b74650de1eabbae6e16f64326fd85a283fb3b58280685
ARG GO_SHA256_ARM64=51798d2c42d0e1c6ed7fd9f48728b4193abac9e8aad6dbac2fe96a81f5909bda
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install --no-install-recommends --yes ca-certificates curl file patch python3 tar \
    && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) GO_ARCH=amd64; GO_SHA256="$GO_SHA256_AMD64" ;; \
      arm64) GO_ARCH=arm64; GO_SHA256="$GO_SHA256_ARM64" ;; \
      *) echo "unsupported target architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl --fail --silent --show-error --location \
      "https://go.dev/dl/go${GO_VERSION}.linux-${GO_ARCH}.tar.gz" \
      --output /tmp/go.tar.gz \
    && printf '%s  /tmp/go.tar.gz\n' "$GO_SHA256" | sha256sum --check --status \
    && tar -C /usr/local -xzf /tmp/go.tar.gz \
    && rm /tmp/go.tar.gz
WORKDIR /src
COPY scripts ./scripts
COPY packaging/fleet-telemetry-bridge ./packaging/fleet-telemetry-bridge
ENV GO_BINARY=/usr/local/go/bin/go \
    RUNNER_TOOL_CACHE=/usr/local/go
RUN mkdir -p /out \
    && scripts/build-fleet-telemetry-bridge.sh \
      --target "linux-${TARGETARCH}" \
      --output /out/tesatlas-fleet-telemetry

FROM debian:13-slim AS runtime
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install --no-install-recommends --yes ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 tesatlas-edge \
    && useradd --uid 10001 --gid 10001 --create-home --no-log-init tesatlas-edge
COPY --from=edge-build /src/target/release/tesatlas-edge /usr/bin/tesatlas-edge
COPY --from=bridge-build /out/tesatlas-fleet-telemetry /usr/bin/fleet-telemetry
COPY scripts/run-with-spool-format-guard.sh /usr/local/libexec/tesatlas-edge/run-with-spool-format-guard.sh
COPY LICENSE /usr/share/doc/tesatlas-edge/LICENSE
COPY docs/legal/third-party-notices.md /usr/share/doc/tesatlas-edge/third-party-notices.md
RUN chmod 0755 /usr/bin/tesatlas-edge /usr/bin/fleet-telemetry \
      /usr/local/libexec/tesatlas-edge/run-with-spool-format-guard.sh \
    && mkdir -p /etc/tesatlas-edge /var/lib/tesatlas-edge \
      /run/tesatlas-edge-receiver /run/tesatlas-edge-hub-tls \
      /run/tesatlas-edge-vehicle-tls \
    && chown -R 10001:10001 /var/lib/tesatlas-edge /run/tesatlas-edge-receiver \
      /run/tesatlas-edge-hub-tls /run/tesatlas-edge-vehicle-tls \
    && chmod 0700 /var/lib/tesatlas-edge /run/tesatlas-edge-receiver \
      /run/tesatlas-edge-hub-tls /run/tesatlas-edge-vehicle-tls
USER 10001:10001
ENTRYPOINT ["/usr/local/libexec/tesatlas-edge/run-with-spool-format-guard.sh", "/usr/bin/tesatlas-edge", "/var/lib/tesatlas-edge/spool", "--", "--config", "/etc/tesatlas-edge/config.toml", "serve"]
