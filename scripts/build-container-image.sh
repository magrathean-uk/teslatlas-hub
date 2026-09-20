#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

usage() {
    echo "usage: $0 --output PATH --tag REPOSITORY:TAG" >&2
    exit 64
}

output=
tag=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output) [ "$#" -ge 2 ] || usage; output=$2; shift 2 ;;
        --tag) [ "$#" -ge 2 ] || usage; tag=$2; shift 2 ;;
        *) usage ;;
    esac
done
[ -n "$output" ] && [ -n "$tag" ] || usage

root=$(CDPATH='' cd -P -- "$(dirname -- "$0")/.." && pwd -P)
cd "$root"

command -v git >/dev/null 2>&1 || {
    echo "git is required" >&2
    exit 69
}

for git_environment in \
    GIT_DIR \
    GIT_WORK_TREE \
    GIT_COMMON_DIR \
    GIT_OBJECT_DIRECTORY \
    GIT_ALTERNATE_OBJECT_DIRECTORIES \
    GIT_INDEX_FILE \
    GIT_NAMESPACE \
    GIT_SHALLOW_FILE \
    GIT_EXEC_PATH \
    GIT_REPLACE_REF_BASE \
    GIT_CONFIG_PARAMETERS \
    GIT_CEILING_DIRECTORIES \
    GIT_DISCOVERY_ACROSS_FILESYSTEM
do
    if env | grep -Eq "^${git_environment}="; then
        echo "Git repository selection environment is not allowed: $git_environment" >&2
        exit 65
    fi
done
if env | grep -Eq '^GIT_CONFIG_[A-Za-z0-9_]*='; then
    echo "Git configuration override environment is not allowed" >&2
    exit 65
fi
repository_root=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "container artifact builder must run from its Hub Git checkout" >&2
    exit 65
}
repository_root=$(CDPATH='' cd -P -- "$repository_root" && pwd -P)
[ "$repository_root" = "$root" ] || {
    echo "container artifact builder resolved a different Git checkout" >&2
    exit 65
}

case "$output" in
    /*) ;;
    *) output="$root/$output" ;;
esac
[ ! -e "$output" ] && [ ! -L "$output" ] || {
    echo "container artifact output already exists" >&2
    exit 65
}
[ -d "$(dirname -- "$output")" ] && [ ! -L "$(dirname -- "$output")" ] || {
    echo "container artifact output parent must be a real directory" >&2
    exit 65
}
printf '%s\n' "$tag" | grep -Eq '^[a-z0-9]+([._/-][a-z0-9]+)*:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$' || {
    echo "container artifact tag must be an explicit local repository:tag" >&2
    exit 65
}

command -v docker >/dev/null 2>&1 || {
    echo "docker is required" >&2
    exit 69
}
command -v python3 >/dev/null 2>&1 || {
    echo "python3 is required" >&2
    exit 69
}
command -v tar >/dev/null 2>&1 || {
    echo "tar is required" >&2
    exit 69
}

official_repository=https://github.com/magrathean-uk/teslatlas-hub.git
origin_url=$(git config --get remote.origin.url || true)
[ "$origin_url" = "$official_repository" ] || {
    echo "origin must be the official Teslatlas Hub GitHub repository" >&2
    exit 65
}
if git config --get-regexp '^url\..*\.insteadof$' >/dev/null 2>&1; then
    echo "Git URL rewrite rules are not allowed for a distributable build" >&2
    exit 65
fi
grafts=$(git rev-parse --git-path info/grafts)
[ ! -e "$grafts" ] && [ ! -L "$grafts" ] || {
    echo "Git grafts are not allowed for a distributable build" >&2
    exit 65
}
[ -z "$(env GIT_NO_REPLACE_OBJECTS= git for-each-ref --format='%(refname)' refs/replace/)" ] || {
    echo "Git replace refs are not allowed for a distributable build" >&2
    exit 65
}
[ -z "$(git config --get core.replaceRefs || true)" ] || {
    echo "core.replaceRefs overrides are not allowed for a distributable build" >&2
    exit 65
}
export GIT_NO_REPLACE_OBJECTS=1
unset GIT_REPLACE_REF_BASE

source_commit=$(git rev-parse --verify HEAD)
printf '%s\n' "$source_commit" | grep -Eq '^[0-9a-f]{40}$' || {
    echo "current Git commit is invalid" >&2
    exit 65
}
[ -z "$(git status --porcelain=v1 --untracked-files=no)" ] || {
    echo "tracked source tree must be clean" >&2
    exit 65
}
git cat-file -e "${source_commit}^{commit}"
source_date_epoch=$(git show -s --format=%ct "$source_commit")
case "$source_date_epoch" in
    ''|*[!0-9]*) echo "selected commit has an invalid timestamp" >&2; exit 65 ;;
esac
[ "${#source_date_epoch}" -le 10 ] || {
    echo "selected commit has an invalid timestamp" >&2
    exit 65
}
remote_main=$(
    git ls-remote --exit-code "$official_repository" refs/heads/main \
        | awk '$2 == "refs/heads/main" && $1 ~ /^[0-9a-f]{40}$/ { print $1; found++ }
               END { if (found != 1) exit 1 }'
) || {
    echo "cannot resolve exactly one official GitHub main tip" >&2
    exit 65
}
git cat-file -e "${remote_main}^{commit}" 2>/dev/null || {
    echo "official GitHub main tip is not present locally; run git fetch origin main" >&2
    exit 65
}
git merge-base --is-ancestor "$source_commit" "$remote_main" || {
    echo "selected commit is not contained by official GitHub main" >&2
    exit 65
}

version=$(sed -nE 's/^version = "([^"]+)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$version" ] || {
    echo "Cargo package version is missing" >&2
    exit 65
}

work=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-container-build.XXXXXX")
cohort_tag="teslatlas-hub-build-cohort:${source_commit}-$(python3 -c 'import secrets; print(secrets.token_hex(16))')"
owned_image_id=
remove_owned_cohort() {
    current_image_id=$(docker image inspect --format '{{.Id}}' "$cohort_tag" 2>/dev/null) || {
        echo "cannot verify the recorded private cohort tag for cleanup: $cohort_tag" >&2
        return 1
    }
    [ "$current_image_id" = "$owned_image_id" ] || {
        echo "refusing to remove a foreign image at private cohort tag: $cohort_tag" >&2
        return 1
    }
    docker image rm "$owned_image_id" >/dev/null 2>&1 || {
        echo "failed to remove the recorded private cohort image: $owned_image_id" >&2
        return 1
    }
    if docker image inspect "$cohort_tag" >/dev/null 2>&1; then
        echo "private cohort tag was replaced during cleanup and was preserved: $cohort_tag" >&2
        return 1
    fi
    return 0
}
cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    if [ -n "$owned_image_id" ]; then
        if ! remove_owned_cohort; then
            [ "$status" -ne 0 ] || status=70
        fi
    fi
    find "$work" -depth -delete 2>/dev/null || true
    exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
iidfile="$work/image-id"
raw_archive="$work/docker-save.tar"
report="$work/artifact.json"
source_root="$work/source"
mkdir -m 700 "$source_root"

# Never expose the live checkout to Docker. Even ignored or untracked files
# below a COPY root could otherwise enter the image while status appears clean.
git archive --format=tar "$source_commit" | tar -xf - -C "$source_root"
python3 "$source_root/scripts/normalize-tree-mtimes.py" \
    --root "$source_root" --epoch "$source_date_epoch" >/dev/null

server_version=$(docker info --format '{{.ServerVersion}}')
case "$server_version" in
    26.*) ;;
    *) echo "the reproducible artifact path requires Docker Engine 26" >&2; exit 65 ;;
esac
docker buildx version >/dev/null 2>&1 || {
    echo "the reproducible artifact path requires the Docker Buildx CLI component" >&2
    exit 69
}
driver_status=$(docker info --format '{{json .DriverStatus}}')
case "$driver_status" in
    *io.containerd.snapshotter.v1*)
        echo "the reproducible artifact path requires Docker's classic image store" >&2
        exit 65
        ;;
esac
if docker image inspect "$cohort_tag" >/dev/null 2>&1; then
    echo "random private cohort tag already exists in the Docker daemon" >&2
    exit 65
fi

# Dockerfile's RUN --mount gate also rejects any accidental legacy-builder
# fallback. --load is required for the validated classic docker-save path.
DOCKER_BUILDKIT=1 docker buildx build \
    --load \
    --no-cache \
    --platform linux/arm64 \
    --build-arg "TESLATLAS_HUB_SOURCE_COMMIT=$source_commit" \
    --build-arg "SOURCE_DATE_EPOCH=$source_date_epoch" \
    --iidfile "$iidfile" \
    --tag "$cohort_tag" \
    "$source_root"

[ -s "$iidfile" ] || {
    echo "BuildKit did not write an image ID" >&2
    exit 65
}
image_id=$(tr -d '\n' < "$iidfile")
printf '%s\n' "$image_id" | grep -Eq '^sha256:[0-9a-f]{64}$' || {
    echo "BuildKit returned an invalid image ID" >&2
    exit 65
}
owned_image_id=$image_id
current_image_id=$(docker image inspect --format '{{.Id}}' "$cohort_tag" 2>/dev/null || true)
[ "$current_image_id" = "$owned_image_id" ] || {
    echo "private cohort tag does not resolve to the recorded image ID" >&2
    exit 70
}

docker image save --output "$raw_archive" "$cohort_tag"
remove_owned_cohort || {
    exit 70
}
owned_image_id=

python3 "$source_root/scripts/canonicalize-docker-archive.py" \
    --input "$raw_archive" \
    --output "$output" \
    --base-lock "$source_root/packaging/docker/base-images.json" \
    --source-commit "$source_commit" \
    --source-date-epoch "$source_date_epoch" \
    --input-repository-tag "$cohort_tag" \
    --output-repository-tag "$tag" \
    --version "$version" \
    --image-id "$image_id" > "$report"
cat "$report"
