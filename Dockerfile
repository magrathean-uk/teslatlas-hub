# SPDX-License-Identifier: AGPL-3.0-only
# syntax=docker/dockerfile:1.7

FROM rust:1.98-bookworm@sha256:828077e0f5ed0401fbd9cb5b4d5dedca23bd13c7fe032f3e8b7313e7acd2a57f AS builder
ARG TESLATLAS_HUB_SOURCE_COMMIT
WORKDIR /build

# Keep dependency resolution bound to Cargo.lock and outside the runtime layer.
COPY Cargo.toml Cargo.lock build.rs source_identity.rs ./
COPY src ./src
COPY examples ./examples
COPY tests ./tests
COPY fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql ./fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql
COPY packaging/com.teslatlas.hub.plist.in ./packaging/com.teslatlas.hub.plist.in
RUN TESLATLAS_HUB_SOURCE_COMMIT="${TESLATLAS_HUB_SOURCE_COMMIT}" \
    cargo build --locked --release --bin teslatlas-hub

FROM debian:13-slim@sha256:a99cfc517144bc59b1978475ec53b46ecabec7e43635402ee5b77cc54cd1b20a AS runtime
ARG HUB_UID=10001
ARG HUB_GID=10001

# Keep the runtime independent of mutable package repositories. The pinned
# builder supplies the public root bundle needed by Hub's outbound TLS clients;
# the private Compose health probe trusts only its explicitly mounted CA.
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /build/target/release/teslatlas-hub /usr/local/bin/teslatlas-hub
COPY LICENSE NOTICE /usr/share/doc/teslatlas-hub/
COPY packaging/docker/initialize-volume.sh /usr/local/libexec/teslatlas-hub-initialize-volume
RUN chown 0:0 \
        /etc/ssl/certs/ca-certificates.crt \
        /usr/local/bin/teslatlas-hub \
        /usr/local/libexec/teslatlas-hub-initialize-volume \
        /usr/share/doc/teslatlas-hub/LICENSE \
        /usr/share/doc/teslatlas-hub/NOTICE \
    && chmod 0644 \
        /etc/ssl/certs/ca-certificates.crt \
        /usr/share/doc/teslatlas-hub/LICENSE \
        /usr/share/doc/teslatlas-hub/NOTICE \
    && chmod 0755 \
        /usr/local/bin/teslatlas-hub \
        /usr/local/libexec/teslatlas-hub-initialize-volume

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
USER ${HUB_UID}:${HUB_GID}
WORKDIR /var/lib/teslatlas-hub
ENTRYPOINT ["/usr/local/bin/teslatlas-hub"]
CMD ["--config", "/etc/teslatlas-hub/config.toml", "serve"]
