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
test -s packaging/docker/base-images.json
test -s docs/guides/install-docker.md
test -x packaging/docker/prepare-volumes.sh

# Compose is optional on source-build hosts; when present, validate the
# resolved service definition without creating containers or touching volumes.
if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    TESLATLAS_HUB_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567 \
    TESLATLAS_HUB_TLS_SERVER_NAME=hub.example.invalid \
        docker compose -f compose.yaml config --quiet
elif command -v docker-compose >/dev/null 2>&1 && docker-compose version >/dev/null 2>&1; then
    TESLATLAS_HUB_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567 \
    TESLATLAS_HUB_TLS_SERVER_NAME=hub.example.invalid \
        docker-compose -f compose.yaml config --quiet
fi

python3 - <<'PY'
import json
import hashlib
import re
from pathlib import Path

dockerfile = Path("Dockerfile").read_text(encoding="utf-8")
locked = json.loads(Path("packaging/docker/base-images.json").read_text(encoding="utf-8"))
assert locked["schema_version"] == 1
assert locked["claim_scope"].startswith("linux/arm64 only")
expected = {
    "builder": "rust",
    "runtime": "debian",
}
for stage, image in expected.items():
    record = locked["images"][stage]
    assert record["official_repository"] == f"library/{image}"
    for field in ("index_digest", "linux_arm64_manifest_digest", "linux_arm64_config_digest"):
        assert re.fullmatch(r"sha256:[0-9a-f]{64}", record[field]), (stage, field)
    assert record["linux_arm64_config_created"].endswith("Z")
    reference = f'{image}:{record["tag"]}@{record["index_digest"]}'
    assert f"FROM {reference} AS {stage}" in dockerfile

from_lines = [line for line in dockerfile.splitlines() if line.startswith("FROM ")]
assert len(from_lines) == 2
assert all(re.fullmatch(r"FROM [^ ]+@sha256:[0-9a-f]{64} AS (builder|runtime)", line) for line in from_lines)
assert not re.search(r"\b(apt|apt-get|apk|dnf|yum|curl|wget)\b", dockerfile)
fixture_path = Path("fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql")
assert hashlib.sha256(fixture_path.read_bytes()).hexdigest() == "d8eebcbbb2f7e2039caa5cc509b0ff76b4aea56e4db1631d16f27aabf86d23db"
delivery = Path("src/sync/updates_delivery.rs").read_text(encoding="utf-8")
assert 'include_bytes!("../../fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql")' in delivery
PY

# Recreate only the builder-stage COPY inputs and compile the production binary.
# This catches release-time include_bytes/include_str dependencies that are
# present in the worktree but absent from a clean Docker build context.
context_root=$(mktemp -d)
trap 'find "$context_root" -depth -delete' EXIT HUP INT TERM
mkdir -p "$context_root/fixtures/teslamate-corpus/v1" "$context_root/packaging"
cp Cargo.toml Cargo.lock build.rs source_identity.rs "$context_root/"
cp -R src examples tests "$context_root/"
cp fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql \
    "$context_root/fixtures/teslamate-corpus/v1/"
cp packaging/com.teslatlas.hub.plist.in "$context_root/packaging/"
TESLATLAS_HUB_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567 \
    CARGO_TARGET_DIR="$root/target/docker-context-check" \
    cargo check --locked --offline --manifest-path "$context_root/Cargo.toml" \
    --bin teslatlas-hub --quiet

grep -F 'COPY Cargo.toml Cargo.lock build.rs source_identity.rs ./' Dockerfile >/dev/null
grep -F 'COPY fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql ./fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql' Dockerfile >/dev/null
grep -F 'COPY packaging/com.teslatlas.hub.plist.in ./packaging/com.teslatlas.hub.plist.in' Dockerfile >/dev/null
grep -F 'TESLATLAS_HUB_SOURCE_COMMIT="${TESLATLAS_HUB_SOURCE_COMMIT}"' Dockerfile >/dev/null
grep -F 'cargo build --locked --release --bin teslatlas-hub' Dockerfile >/dev/null
grep -F 'COPY --from=builder --chown=0:0 --chmod=0644 /etc/ssl/certs/ca-certificates.crt' Dockerfile >/dev/null
grep -F 'TESLATLAS_HUB_SOURCE_COMMIT: ${TESLATLAS_HUB_SOURCE_COMMIT:?' compose.yaml >/dev/null
grep -F 'USER ${HUB_UID}:${HUB_GID}' Dockerfile >/dev/null
grep -F 'read_only: true' compose.yaml >/dev/null
grep -F 'cap_drop: ["ALL"]' compose.yaml >/dev/null
grep -F 'no-new-privileges:true' compose.yaml >/dev/null
grep -F '127.0.0.1:8443:8443' compose.yaml >/dev/null
grep -F './config.toml:/etc/teslatlas-hub/config.toml:ro' compose.yaml >/dev/null
grep -F './tls:/etc/teslatlas-hub/tls:ro' compose.yaml >/dev/null
grep -F 'hub-data:/var/lib/teslatlas-hub' compose.yaml >/dev/null
grep -F -- '- CMD' compose.yaml >/dev/null
grep -F -- '- /usr/local/bin/teslatlas-hub' compose.yaml >/dev/null
grep -F -- '- healthcheck' compose.yaml >/dev/null
grep -F -- '- --ca-file' compose.yaml >/dev/null
grep -F -- '- --server-name' compose.yaml >/dev/null
grep -F 'TESLATLAS_HUB_TLS_SERVER_NAME:?' compose.yaml >/dev/null
grep -F '127, 0, 0, 1' src/application/main/healthcheck.rs >/dev/null
if grep -Eq 'CMD-SHELL|\bcurl\b' compose.yaml; then
    printf '%s\n' 'compose healthcheck must use the Hub binary without a shell or curl' >&2
    exit 1
fi
grep -F '0.0.0.0:8443' packaging/docker/config.toml >/dev/null
grep -F 'volume-init' packaging/docker/prepare-volumes.sh >/dev/null
grep -F 'cap_add: ["CHOWN", "FOWNER", "DAC_OVERRIDE"]' compose.yaml >/dev/null
grep -F 'stat -c' compose.yaml >/dev/null
grep -F '10001:10001:700' compose.yaml >/dev/null
test "$(grep -Fc 'cap_drop: ["ALL"]' compose.yaml)" -eq 2
grep -F 'setup --tokens-stdin' docs/guides/install-docker.md >/dev/null
grep -F 'docker compose run --rm hub source' docs/guides/install-docker.md >/dev/null
grep -F 'packaging/docker/base-images.json' docs/guides/install-docker.md >/dev/null
grep -F '/config.toml' .gitignore >/dev/null
grep -F '/tls/' .gitignore >/dev/null
grep -Fx '.git' .dockerignore >/dev/null
grep -Fx 'config.toml' .dockerignore >/dev/null
grep -Fx 'tls/' .dockerignore >/dev/null
grep -Fx '*.key' .dockerignore >/dev/null
grep -Fx '*.pem' .dockerignore >/dev/null
grep -Fx '*.p12' .dockerignore >/dev/null
grep -Fx '*.pfx' .dockerignore >/dev/null
grep -Fx 'target/' .dockerignore >/dev/null
printf '%s\n' 'docker packaging static checks passed'
