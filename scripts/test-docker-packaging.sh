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
test -x packaging/docker/initialize-volume.sh
test -x scripts/finalize-container-runtime-evidence.py
test -x scripts/build-container-image.sh
test -x scripts/canonicalize-docker-archive.py
test -x scripts/normalize-tree-mtimes.py

# Compose is optional on source-build hosts; when present, validate the
# resolved service definition without creating containers or touching volumes.
if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    TESLATLAS_HUB_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567 \
    SOURCE_DATE_EPOCH=1700000000 \
    TESLATLAS_HUB_TLS_SERVER_NAME=hub.example.invalid \
        docker compose -f compose.yaml config --quiet
    TESLATLAS_HUB_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567 \
    SOURCE_DATE_EPOCH=1700000000 \
    TESLATLAS_HUB_TLS_SERVER_NAME=hub.example.invalid \
        docker compose -f compose.yaml config --format json \
        | python3 scripts/check-docker-compose-render.py
elif command -v docker-compose >/dev/null 2>&1 && docker-compose version >/dev/null 2>&1; then
    TESLATLAS_HUB_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567 \
    SOURCE_DATE_EPOCH=1700000000 \
    TESLATLAS_HUB_TLS_SERVER_NAME=hub.example.invalid \
        docker-compose -f compose.yaml config --quiet
    TESLATLAS_HUB_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567 \
    SOURCE_DATE_EPOCH=1700000000 \
    TESLATLAS_HUB_TLS_SERVER_NAME=hub.example.invalid \
        docker-compose -f compose.yaml config --format json \
        | python3 scripts/check-docker-compose-render.py
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

runtime_diff_ids = locked["images"]["runtime"]["linux_arm64_rootfs_diff_ids"]
assert runtime_diff_ids == [
    "sha256:8227a1264c7ff2f8bd125b585e62c1cec77dd2099c985c33e08d34292d06144f"
]

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
mkdir -p "$context_root/fixtures/teslamate-corpus/v1" "$context_root/packaging/docker"
cp Cargo.toml Cargo.lock build.rs source_identity.rs "$context_root/"
cp LICENSE NOTICE "$context_root/"
cp -R src examples tests "$context_root/"
cp fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql \
    "$context_root/fixtures/teslamate-corpus/v1/"
cp packaging/com.teslatlas.hub.plist.in "$context_root/packaging/"
cp packaging/docker/initialize-volume.sh "$context_root/packaging/docker/"
TESLATLAS_HUB_SOURCE_COMMIT=0123456789abcdef0123456789abcdef01234567 \
    CARGO_TARGET_DIR="$root/target/docker-context-check" \
    cargo check --locked --offline --manifest-path "$context_root/Cargo.toml" \
    --bin teslatlas-hub --quiet

# Repository-selection variables must not redirect the wrapper into an
# alternate checkout even when that checkout advertises the official URL.
alternate_repository="$context_root/alternate-repository"
mkdir "$alternate_repository"
git -C "$alternate_repository" init -q
git -C "$alternate_repository" remote add origin \
    https://github.com/magrathean-uk/teslatlas-hub.git
if GIT_DIR="$alternate_repository/.git" GIT_WORK_TREE="$alternate_repository" \
    ./scripts/build-container-image.sh \
        --output "$context_root/redirected.tar" \
        --tag teslatlas-hub:redirected \
        2>"$context_root/redirected.err"; then
    printf '%s\n' 'container artifact builder accepted redirected Git repository state' >&2
    exit 1
fi
grep -F 'Git repository selection environment is not allowed: GIT_DIR' \
    "$context_root/redirected.err" >/dev/null
test ! -e "$context_root/redirected.tar"
if GIT_EXEC_PATH="$alternate_repository" \
    ./scripts/build-container-image.sh \
        --output "$context_root/fabricated-remote.tar" \
        --tag teslatlas-hub:fabricated-remote \
        2>"$context_root/fabricated-remote.err"; then
    printf '%s\n' 'container artifact builder accepted redirected Git helper path' >&2
    exit 1
fi
grep -F 'Git repository selection environment is not allowed: GIT_EXEC_PATH' \
    "$context_root/fabricated-remote.err" >/dev/null
test ! -e "$context_root/fabricated-remote.tar"

grep -F 'COPY Cargo.toml Cargo.lock build.rs source_identity.rs ./' Dockerfile >/dev/null
grep -F 'COPY LICENSE NOTICE ./' Dockerfile >/dev/null
grep -F 'COPY fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql ./fixtures/teslamate-corpus/v1/updates-lossless-selected-car.sql' Dockerfile >/dev/null
grep -F 'COPY packaging/com.teslatlas.hub.plist.in ./packaging/com.teslatlas.hub.plist.in' Dockerfile >/dev/null
grep -F 'COPY packaging/docker/initialize-volume.sh ./packaging/docker/initialize-volume.sh' Dockerfile >/dev/null
grep -F 'TESLATLAS_HUB_SOURCE_COMMIT="${TESLATLAS_HUB_SOURCE_COMMIT}"' Dockerfile >/dev/null
grep -F 'ARG SOURCE_DATE_EPOCH' Dockerfile >/dev/null
grep -F -- 'RUN --mount=type=tmpfs,target=/tmp/teslatlas-buildkit-required' Dockerfile >/dev/null
grep -F 'SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH}"' Dockerfile >/dev/null
grep -F 'org.opencontainers.image.revision="${TESLATLAS_HUB_SOURCE_COMMIT}"' Dockerfile >/dev/null
grep -F 'cargo build --locked --release --bin teslatlas-hub' Dockerfile >/dev/null
grep -F 'official_repository=https://github.com/magrathean-uk/teslatlas-hub.git' scripts/build-container-image.sh >/dev/null
grep -F 'cd -P -- "$(dirname -- "$0")/.." && pwd -P' scripts/build-container-image.sh >/dev/null
grep -F 'git rev-parse --show-toplevel' scripts/build-container-image.sh >/dev/null
grep -F 'GIT_ALTERNATE_OBJECT_DIRECTORIES' scripts/build-container-image.sh >/dev/null
grep -F 'GIT_EXEC_PATH' scripts/build-container-image.sh >/dev/null
grep -F 'GIT_CONFIG_PARAMETERS' scripts/build-container-image.sh >/dev/null
grep -F "'^GIT_CONFIG_[A-Za-z0-9_]*='" scripts/build-container-image.sh >/dev/null
grep -F 'git ls-remote --exit-code "$official_repository" refs/heads/main' scripts/build-container-image.sh >/dev/null
grep -F 'export GIT_NO_REPLACE_OBJECTS=1' scripts/build-container-image.sh >/dev/null
grep -F "refs/replace/" scripts/build-container-image.sh >/dev/null
grep -F 'info/grafts' scripts/build-container-image.sh >/dev/null
grep -F 'git merge-base --is-ancestor "$source_commit" "$remote_main"' scripts/build-container-image.sh >/dev/null
grep -F 'git archive --format=tar "$source_commit" | tar -xf - -C "$source_root"' scripts/build-container-image.sh >/dev/null
grep -F 'normalize-tree-mtimes.py' scripts/build-container-image.sh >/dev/null
grep -F '    "$source_root"' scripts/build-container-image.sh >/dev/null
if grep -Fx '    .' scripts/build-container-image.sh >/dev/null; then
    printf '%s\n' 'container artifact builder must not expose the live checkout to Docker' >&2
    exit 1
fi
grep -F 'find /image-root -exec touch -h -d "@${SOURCE_DATE_EPOCH}" {} +' Dockerfile >/dev/null
grep -F 'COPY --from=builder /image-root/ /' Dockerfile >/dev/null
if grep -F -- '--chmod=' Dockerfile >/dev/null; then
    printf '%s\n' 'Dockerfile must keep explicit reviewed runtime ownership and mode steps' >&2
    exit 1
fi
grep -F 'install -d -o 10001 -g 10001 -m 0755 /image-root/var/lib/teslatlas-hub' Dockerfile >/dev/null
grep -F 'the reproducible artifact path requires Docker Engine 26' scripts/build-container-image.sh >/dev/null
grep -F 'docker buildx version' scripts/build-container-image.sh >/dev/null
grep -F 'DOCKER_BUILDKIT=1 docker buildx build' scripts/build-container-image.sh >/dev/null
grep -F '    --load' scripts/build-container-image.sh >/dev/null
grep -F "io.containerd.snapshotter.v1" scripts/build-container-image.sh >/dev/null
grep -F 'secrets.token_hex(16)' scripts/build-container-image.sh >/dev/null
grep -F 'random private cohort tag already exists in the Docker daemon' scripts/build-container-image.sh >/dev/null
grep -F 'docker image save --output "$raw_archive" "$cohort_tag"' scripts/build-container-image.sh >/dev/null
grep -F -- '--input-repository-tag "$cohort_tag"' scripts/build-container-image.sh >/dev/null
grep -F -- '--output-repository-tag "$tag"' scripts/build-container-image.sh >/dev/null
grep -F '[ "$current_image_id" = "$owned_image_id" ]' scripts/build-container-image.sh >/dev/null
grep -F 'docker image rm "$owned_image_id"' scripts/build-container-image.sh >/dev/null
grep -F 'private cohort tag was replaced during cleanup and was preserved' scripts/build-container-image.sh >/dev/null
grep -F 'docker image save --output "$raw_archive" "$cohort_tag"' scripts/build-container-image.sh >/dev/null
test "$(grep -nE 'remove_owned_cohort|canonicalize-docker-archive.py' scripts/build-container-image.sh \
    | tail -n 2 | sed -n '1s/:.*//p')" -lt \
    "$(grep -n 'canonicalize-docker-archive.py' scripts/build-container-image.sh | cut -d: -f1)"
grep -F 'atomic_publish_noreplace(parent_descriptor, temporary_name, output_name)' \
    scripts/canonicalize-docker-archive.py >/dev/null
grep -F 'LayerSources' scripts/canonicalize-docker-archive.py >/dev/null
grep -F 'OCI_MANIFEST_MEDIA_TYPE' scripts/canonicalize-docker-archive.py >/dev/null
grep -F 'EMPTY_LAYER_DIGEST' scripts/canonicalize-docker-archive.py >/dev/null
grep -F 'validate_legacy_metadata_chain' scripts/canonicalize-docker-archive.py >/dev/null
if grep -F 'unlink_if_identity' scripts/canonicalize-docker-archive.py >/dev/null; then
    printf '%s\n' 'container archive publication must not use checked-path unlink cleanup' >&2
    exit 1
fi
if grep -E 'docker image (inspect|rm|save).*(^|[^a-z_])"?\$tag"?' scripts/build-container-image.sh >/dev/null; then
    printf '%s\n' 'container artifact builder must not touch the requested tag in Docker' >&2
    exit 1
fi
grep -F 'TESLATLAS_HUB_SOURCE_COMMIT: ${TESLATLAS_HUB_SOURCE_COMMIT:?' compose.yaml >/dev/null
grep -F 'SOURCE_DATE_EPOCH: ${SOURCE_DATE_EPOCH:?' compose.yaml >/dev/null
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
grep -F 'condition: service_completed_successfully' compose.yaml >/dev/null
grep -F '/usr/local/libexec/teslatlas-hub-initialize-volume' compose.yaml >/dev/null
test "$(grep -Fc 'cap_drop: ["ALL"]' compose.yaml)" -eq 2

sh scripts/test-docker-volume-init.sh
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
python3 -m unittest scripts/test_finalize_container_runtime_evidence.py
python3 -m unittest scripts/test_canonicalize_docker_archive.py
python3 -m unittest scripts/test_normalize_tree_mtimes.py
printf '%s\n' 'docker packaging static checks passed'
