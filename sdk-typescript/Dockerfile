# syntax=docker/dockerfile:1

# Keep the image identity explicit.  The build installs the lock-pinned npm
# release because the upstream Node image's bundled npm can change.
FROM node:26.7.0-bookworm-slim AS toolchain

ENV NODE_ENV=development \
    npm_config_audit=false \
    npm_config_fund=false \
    npm_config_engine_strict=true
WORKDIR /workspace

RUN npm install --global npm@11.19.0 \
    && test "$(node --version)" = "v26.7.0" \
    && test "$(npm --version)" = "11.19.0"

COPY package.json package-lock.json .npmrc tsconfig.json tsconfig.build.json biome.json vitest.config.ts ./
RUN npm ci

FROM toolchain AS source
COPY protocol ./protocol
COPY src ./src
COPY scripts ./scripts
COPY tests ./tests
COPY examples/node.mjs ./examples/node.mjs
COPY examples/browser ./examples/browser
COPY examples/hub ./examples/hub
COPY docs ./docs
COPY README.md LICENSE ./
RUN npm run build \
    && mkdir -p /tmp/teslatlas-sdk-package \
    && npm pack --pack-destination /tmp/teslatlas-sdk-package >/tmp/teslatlas-sdk-pack.log \
    && set -- /tmp/teslatlas-sdk-package/*.tgz \
    && test "$#" -eq 1 \
    && mv "$1" /tmp/teslatlas-sdk.tgz

FROM source AS test
RUN npx playwright install --with-deps chromium \
    && npm run verify

FROM node:26.7.0-bookworm-slim AS consumer
ENV npm_config_audit=false \
    npm_config_fund=false \
    npm_config_engine_strict=true
WORKDIR /consumer
RUN npm install --global npm@11.19.0 \
    && test "$(node --version)" = "v26.7.0" \
    && test "$(npm --version)" = "11.19.0"
COPY --from=source /tmp/teslatlas-sdk.tgz /tmp/teslatlas-sdk.tgz
COPY examples/hub/package.json examples/hub/node.mjs examples/hub/index.html examples/hub/app.js examples/hub/serve.mjs ./
RUN npm install --omit=dev --ignore-scripts --no-audit --no-fund /tmp/teslatlas-sdk.tgz
USER 1000:1000
ENTRYPOINT ["node", "node.mjs"]
