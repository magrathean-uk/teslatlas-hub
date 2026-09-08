# Keep the build/runtime toolchain aligned with the locked jsdom and TypeScript
# requirements. Update this patch tag only with a fresh verification run.
FROM node:26.8.1-bookworm-slim AS build

WORKDIR /build
RUN apt-get update \
  && apt-get install -y --no-install-recommends tar \
  && rm -rf /var/lib/apt/lists/*

COPY package.json package-lock.json ./
COPY bin ./bin
COPY scripts ./scripts
COPY artifacts ./artifacts
COPY src ./src
COPY public ./public
COPY index.html tsconfig.json tsconfig.app.json tsconfig.node.json vite.config.ts ./

RUN npm ci --include=dev
RUN npm run build

FROM node:26.8.1-bookworm-slim AS runtime

WORKDIR /app
ENV NODE_ENV=production
COPY --from=build --chown=node:node /build/dist ./dist
COPY --from=build --chown=node:node /build/bin/teslatlas-viewer.mjs ./bin/teslatlas-viewer.mjs
COPY --from=build --chown=node:node /build/package.json ./package.json

USER node
EXPOSE 4173
ENTRYPOINT ["node", "bin/teslatlas-viewer.mjs", "--host", "0.0.0.0", "--port", "4173"]
