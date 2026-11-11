# Keep the build/runtime toolchain aligned with the locked jsdom and TypeScript
# requirements. Update this patch tag only with a fresh verification run.
FROM node:26.10.0-bookworm-slim@sha256:662933cf47f013bc8e4beb31a6116448427a82057ba7c42c97e4c5ba766504c2 AS build

WORKDIR /build
RUN apt-get update \
  && apt-get install -y --no-install-recommends tar \
  && rm -rf /var/lib/apt/lists/*

RUN npm install --global npm@12.1.0 \
  && test "$(node --version)" = "v26.10.0" \
  && test "$(npm --version)" = "12.1.0"

COPY package.json package-lock.json ./
COPY bin ./bin
COPY scripts ./scripts
COPY artifacts ./artifacts
COPY src ./src
COPY public ./public
COPY index.html tsconfig.json tsconfig.app.json tsconfig.node.json vite.config.ts ./

RUN npm ci --include=dev
RUN npm run build

FROM node:26.10.0-bookworm-slim@sha256:662933cf47f013bc8e4beb31a6116448427a82057ba7c42c97e4c5ba766504c2 AS runtime

WORKDIR /app
ENV NODE_ENV=production
COPY --from=build --chown=node:node /build/dist ./dist
COPY --from=build --chown=node:node /build/bin/teslatlas-viewer.mjs ./bin/teslatlas-viewer.mjs
COPY --from=build --chown=node:node /build/bin/healthcheck.mjs ./bin/healthcheck.mjs
COPY --from=build --chown=node:node /build/package.json ./package.json

USER node
EXPOSE 4173
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 CMD ["node", "bin/healthcheck.mjs"]
ENTRYPOINT ["node", "bin/teslatlas-viewer.mjs", "--host", "0.0.0.0", "--port", "4173"]
