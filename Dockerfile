# SPDX-License-Identifier: AGPL-3.0-only
# syntax=docker/dockerfile:1.7

FROM rust:1.98-bookworm AS builder
WORKDIR /build

# Keep dependency resolution bound to Cargo.lock and outside the runtime layer.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples ./examples
COPY tests ./tests
RUN cargo build --locked --release --bin teslatlas-hub

FROM debian:13-slim AS runtime
ARG HUB_UID=10001
ARG HUB_GID=10001

RUN apt-get update \
    && apt-get install --no-install-recommends --yes ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid "${HUB_GID}" teslatlas \
    && useradd --uid "${HUB_UID}" --gid "${HUB_GID}" --create-home --home-dir /nonexistent --shell /usr/sbin/nologin teslatlas \
    && install --directory --owner="${HUB_UID}" --group="${HUB_GID}" --mode=0700 /var/lib/teslatlas-hub /tmp

COPY --from=builder /build/target/release/teslatlas-hub /usr/local/bin/teslatlas-hub
COPY LICENSE NOTICE /usr/share/doc/teslatlas-hub/

RUN chown root:root /usr/local/bin/teslatlas-hub \
    && chmod 0755 /usr/local/bin/teslatlas-hub \
    && chown -R root:root /usr/share/doc/teslatlas-hub \
    && chmod -R a=rX /usr/share/doc/teslatlas-hub

USER ${HUB_UID}:${HUB_GID}
WORKDIR /var/lib/teslatlas-hub
ENTRYPOINT ["/usr/local/bin/teslatlas-hub"]
CMD ["--config", "/etc/teslatlas-hub/config.toml", "serve"]
