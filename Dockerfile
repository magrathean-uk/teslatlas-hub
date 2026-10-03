# SPDX-License-Identifier: AGPL-3.0-only

ARG SOURCE_DATE_EPOCH

FROM rust:1.98-bookworm@sha256:828077e0f5ed0401fbd9cb5b4d5dedca23bd13c7fe032f3e8b7313e7acd2a57f AS builder
ARG TESLATLAS_HUB_SOURCE_COMMIT
ARG SOURCE_DATE_EPOCH
WORKDIR /build

# SOURCE_DATE_EPOCH is a predefined BuildKit argument. The mount is deliberately
# BuildKit-only so the legacy builder cannot silently produce a distributable
# image with wall-clock layer and configuration timestamps.
RUN --mount=type=tmpfs,target=/tmp/teslatlas-buildkit-required \
    case "${SOURCE_DATE_EPOCH}" in \
        ''|*[!0-9]*) exit 64 ;; \
    esac

# Keep dependency resolution bound to Cargo.lock and outside the runtime layer.
COPY Cargo.toml Cargo.lock build.rs source_identity.rs ./
COPY LICENSE NOTICE ./
COPY src ./src
COPY examples ./examples
COPY tests ./tests
COPY fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql ./fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql
COPY packaging/com.teslatlas.hub.plist.in ./packaging/com.teslatlas.hub.plist.in
COPY packaging/docker/initialize-volume.sh ./packaging/docker/initialize-volume.sh
RUN TESLATLAS_HUB_SOURCE_COMMIT="${TESLATLAS_HUB_SOURCE_COMMIT}" \
    SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH}" \
    cargo build --locked --release --bin teslatlas-hub \
    && install -d -m 0755 \
        /image-root/etc/ssl/certs \
        /image-root/usr/local/bin \
        /image-root/usr/local/libexec \
        /image-root/usr/share/doc/teslatlas-hub \
    && install -d -o 10001 -g 10001 -m 0755 /image-root/var/lib/teslatlas-hub \
    && install -m 0644 /etc/ssl/certs/ca-certificates.crt \
        /image-root/etc/ssl/certs/ca-certificates.crt \
    && install -m 0755 /build/target/release/teslatlas-hub \
        /image-root/usr/local/bin/teslatlas-hub \
    && install -m 0755 packaging/docker/initialize-volume.sh \
        /image-root/usr/local/libexec/teslatlas-hub-initialize-volume \
    && install -m 0644 LICENSE NOTICE /image-root/usr/share/doc/teslatlas-hub/ \
    && find /image-root -exec touch -h -d "@${SOURCE_DATE_EPOCH}" {} +

FROM debian:13-slim@sha256:a99cfc517144bc59b1978475ec53b46ecabec7e43635402ee5b77cc54cd1b20a AS runtime
ARG TESLATLAS_HUB_SOURCE_COMMIT
ARG SOURCE_DATE_EPOCH
ARG HUB_UID=10001
ARG HUB_GID=10001

# Keep the runtime independent of mutable package repositories. One staged
# rootfs copy carries exact ownership, modes, file contents and commit-epoch
# mtimes. The pinned builder supplies the public root bundle needed by Hub's
# outbound TLS clients; the private Compose health probe trusts only its
# explicitly mounted CA.
COPY --from=builder /image-root/ /
LABEL org.opencontainers.image.title="Teslatlas Hub" \
      org.opencontainers.image.version="2026.36.2" \
      org.opencontainers.image.source="https://github.com/magrathean-uk/teslatlas-hub" \
      org.opencontainers.image.revision="${TESLATLAS_HUB_SOURCE_COMMIT}"

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
USER ${HUB_UID}:${HUB_GID}
WORKDIR /var/lib/teslatlas-hub
ENTRYPOINT ["/usr/local/bin/teslatlas-hub"]
CMD ["--config", "/etc/teslatlas-hub/config.toml", "serve"]
