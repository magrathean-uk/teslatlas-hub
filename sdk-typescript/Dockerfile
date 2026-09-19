# syntax=docker/dockerfile:1

# This Dockerfile is intentionally package-only. scripts/run-docker-package-gate.mjs
# creates a private temporary context containing the exact reviewed npm archive and
# bounded consumer files; repository source is never part of the build context.
FROM --platform=linux/arm64 node:26.7.0-bookworm-slim@sha256:4db36457f406501e6f608802e5da617e5fbd0e80b75901b6a09de1ae5a667d32 AS package-check

ARG BUILDPLATFORM
ARG TARGETPLATFORM
ARG SDK_TARBALL_SHA256
ENV npm_config_audit=false \
    npm_config_engine_strict=true \
    npm_config_fund=false
WORKDIR /consumer

# Cross-architecture emulation is outside the supported lane. BUILDPLATFORM is
# supplied by BuildKit and must describe the native Linux ARM64 builder.
RUN test "$BUILDPLATFORM" = "linux/arm64" \
    && test "$TARGETPLATFORM" = "linux/arm64" \
    && test "$(uname -m)" = "aarch64" \
    && test "$(node --version)" = "v26.7.0" \
    && npm install --global npm@11.19.0 \
    && test "$(npm --version)" = "11.19.0"

COPY teslatlas-sdk.tgz /tmp/teslatlas-sdk.tgz
COPY package-smoke.mjs ./package-smoke.mjs
RUN test -n "$SDK_TARBALL_SHA256" \
    && printf '%s  %s\n' "$SDK_TARBALL_SHA256" /tmp/teslatlas-sdk.tgz | sha256sum --check --strict - \
    && npm install --omit=dev --ignore-scripts --no-audit --no-fund /tmp/teslatlas-sdk.tgz \
    && node package-smoke.mjs "$SDK_TARBALL_SHA256"

FROM package-check AS test

FROM package-check AS consumer
COPY consumer/package.json consumer/node.mjs consumer/index.html consumer/app.js consumer/serve.mjs ./
RUN rm -f /tmp/teslatlas-sdk.tgz package-smoke.mjs
USER 1000:1000
ENTRYPOINT ["node", "node.mjs"]
