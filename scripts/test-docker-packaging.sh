#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

test -s Dockerfile
test -s .dockerignore
test -s compose.yaml
test -s packaging/docker/config.toml
test -s packaging/docker/config.toml.example
test -s docs/guides/install-docker.md
test -x packaging/docker/prepare-volumes.sh

# Compose is optional on source-build hosts; when present, validate the
# resolved service definition without creating containers or touching volumes.
if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    TESLATLAS_HUB_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567 \
        docker compose -f compose.yaml config --quiet
elif command -v docker-compose >/dev/null 2>&1 && docker-compose version >/dev/null 2>&1; then
    TESLATLAS_HUB_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567 \
        docker-compose -f compose.yaml config --quiet
fi

grep -F 'COPY Cargo.toml Cargo.lock build.rs source_identity.rs ./' Dockerfile >/dev/null
grep -F 'TESLATLAS_HUB_SOURCE_COMMIT="${TESLATLAS_HUB_SOURCE_COMMIT}"' Dockerfile >/dev/null
grep -F 'cargo build --locked --release --bin teslatlas-hub' Dockerfile >/dev/null
grep -F 'TESLATLAS_HUB_SOURCE_COMMIT: ${TESLATLAS_HUB_SOURCE_COMMIT:?' compose.yaml >/dev/null
grep -F 'USER ${HUB_UID}:${HUB_GID}' Dockerfile >/dev/null
grep -F 'no-new-privileges:true' compose.yaml >/dev/null
grep -F '127.0.0.1:8443:8443' compose.yaml >/dev/null
grep -F './config.toml:/etc/teslatlas-hub/config.toml:ro' compose.yaml >/dev/null
grep -F '/healthz' compose.yaml >/dev/null
grep -F '0.0.0.0:8443' packaging/docker/config.toml >/dev/null
grep -F 'setup --tokens-stdin' docs/guides/install-docker.md >/dev/null
grep -F '/config.toml' .gitignore >/dev/null
grep -F '/tls/' .gitignore >/dev/null
printf '%s\n' 'docker packaging static checks passed'
