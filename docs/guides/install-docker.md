# Run Hub with Docker Compose

Teslatlas Hub can be built locally from this repository and run as a non-root
Compose service. This is a source-build workflow; the project does not publish
an image registry or prebuilt image. Native macOS and Debian service installs
remain separate workflows.

Both official base-image tags are bound to immutable multi-platform index
digests in the Dockerfile. Their resolved index identities and Linux ARM64
child manifests are recorded in `packaging/docker/base-images.json`. The index
form preserves ordinary Docker platform selection, but this project currently
claims container acceptance only for Linux ARM64. The runtime stage does not
contact a package repository: it copies the public CA root bundle from the
digest-pinned Rust builder and uses the Hub binary itself for the private TLS
health probe.

The current Compose file is a core Hub candidate. Optional Fleet, Home
Assistant, and Edge services require their own reviewed source cohort and
acceptance evidence. An empty public companion catalog does not make those
services available automatically.

The Dockerfile deliberately avoids `COPY --chmod`, but the candidate
distributable container path requires Docker 26, its Buildx CLI component, and
the classic image store. Docker 26's optional containerd image store emits a
different OCI save layout and is intentionally rejected. The wrapper uses
`docker buildx build --load`; the BuildKit-only validation step prevents a
legacy-builder fallback, and the selected pushed commit's exact timestamp is
passed as `SOURCE_DATE_EPOCH`. The ordinary Compose runtime evidence remains
bounded separately; this build path does not add lifecycle or F6 acceptance by
itself.

## Prepare a private configuration

Install Docker Engine or Docker Desktop with Compose v2, then copy the example
configuration and create a trusted certificate for the hostname that browsers
will use:

```sh
cp packaging/docker/config.toml.example config.toml
mkdir -m 700 tls
```

Set `tls/server.pem`, `tls/server-key.pem`, and `tls/ca.pem` with ownership and
modes accepted by the host. `tls/ca.pem` is used only by the container health
probe; it must validate `server.pem`. Configure `public_url` to the same
trusted origin that clients will use. Do not place Tesla or pairing secrets in
the image, Compose file, environment, build arguments, or Git.

## Build and start

Build from the locked Rust source, initialise the store while the service is
stopped, then start the foreground Hub process:

```sh
export TESLATLAS_HUB_SOURCE_COMMIT=$(git rev-parse HEAD)
export SOURCE_DATE_EPOCH=$(git show -s --format=%ct "$TESLATLAS_HUB_SOURCE_COMMIT")
export TESLATLAS_HUB_TLS_SERVER_NAME=hub.example.invalid # replace with the DNS name in server.pem and public_url
test -z "$(git status --short)"
test "$(git config --get remote.origin.url)" = \
  https://github.com/magrathean-uk/teslatlas-hub.git
test -z "$(git replace -l)"
test ! -e "$(git rev-parse --git-path info/grafts)"
! git config --get-regexp '^url\..*\.insteadof$'
OFFICIAL_MAIN=$(git ls-remote \
  https://github.com/magrathean-uk/teslatlas-hub.git refs/heads/main | awk '{print $1}')
git cat-file -e "${OFFICIAL_MAIN}^{commit}"
GIT_NO_REPLACE_OBJECTS=1 git merge-base --is-ancestor \
  "$TESLATLAS_HUB_SOURCE_COMMIT" "$OFFICIAL_MAIN"
docker compose build hub
test "$(docker compose run --rm hub source)" = \
  "https://github.com/magrathean-uk/teslatlas-hub/tree/$TESLATLAS_HUB_SOURCE_COMMIT"
./packaging/docker/prepare-volumes.sh
docker compose run --rm hub --config /etc/teslatlas-hub/config.toml bootstrap
# Keep the token JSON on a private pipe; `--tokens-stdin` is the bounded setup input.
printf '%s\n' '{"accessToken":"<access-token>","refreshToken":"<refresh-token>"}' \
  | docker compose run --rm -T hub --config /etc/teslatlas-hub/config.toml setup --tokens-stdin
docker compose up -d hub
docker compose exec hub teslatlas-hub --config /etc/teslatlas-hub/config.toml status
docker compose logs --tail 100 hub
```

Compose requires that explicit exact pushed commit and its commit timestamp.
The Docker build fails closed when either is absent or malformed; it never
infers source identity from the build directory. The official-remote ancestry
check and `source` readback above are required because the image labels and
binary source route name that repository. Do not treat a successful build alone
as source-provenance evidence. The build context also excludes Git metadata,
local configuration, TLS material and the Cargo target cache; only the
Dockerfile's explicit source and legal `COPY` inputs enter the build. Use the
artifact wrapper—not this live Compose context—for distribution evidence.

The one-shot volume initializer runs only after the image exists. It drops all
capabilities and adds back only `CHOWN`, `FOWNER`, and `DAC_OVERRIDE`, changes
only `/var/lib/teslatlas-hub` in the named volume, and fails unless the result
is exactly UID/GID `10001:10001` with mode `0700`. It atomically publishes an
empty regular `.teslatlas-volume-initialized` sentinel with UID/GID
`10001:10001` and mode `0600`, so Docker does not treat the volume as empty and
replace those root-directory permissions from the image on the next mount. A
pre-existing symlink, non-regular entry, non-empty file, or wrong sentinel
metadata/link count fails without following or rewriting that entry. Compose completion-
orders this initializer before Hub, including a generic `docker compose up`, so
the privileged one-shot service cannot race normal startup. The normal Hub
service still drops every capability and never recursively changes ownership.
Bind-mounted data must already have the private ownership and modes required by
Hub.

## Create a reproducibility-candidate local image archive

Run the artifact builder from a tracked-clean checkout whose exact `HEAD` is
contained by the advertised `main` tip of the fixed official GitHub repository.
Docker 26's Buildx CLI component must already be installed and visible as
`docker buildx version`; the wrapper does not install it. The wrapper resolves
its checkout physically and rejects a different origin, URL rewrites, replace
refs, grafts, replace-object configuration, and Git environment that can select
another repository, work tree, object store, index, namespace, shallow file, or
configuration source. It requires the official remote tip locally,
derives `SOURCE_DATE_EPOCH` from `HEAD`, materializes a fresh `git archive`
context, and normalizes every regular, directory and symlink context mtime to
that epoch before Docker sees it. It performs a no-cache Linux ARM64
`docker buildx build --load` and records the content-addressed image ID. This
keeps ignored and untracked checkout files outside every `COPY`. Missing Buildx,
missing BuildKit, or a legacy fallback fails closed.

```sh
mkdir -p dist
./scripts/build-container-image.sh \
  --tag teslatlas-hub:2026.36.2-arm64 \
  --output dist/teslatlas-hub_2026.36.2_linux-arm64.docker.tar
```

The final validator requires exactly one tagged Docker image; Linux ARM64; the
exact source, version and title labels; the pinned Debian base rootfs prefix;
matching config, image ID, ordered layer diffIDs and legacy parent graph; and
the selected commit epoch on the image and Hub application-history suffix. It
also binds the runtime user, environment, entrypoint, command and working
directory, and requires every regular/directory member in every Hub application
layer to have the exact commit mtime. Links, special members, PAX metadata,
duplicates, unsafe paths, unreferenced payloads and an existing output are
refused. The daemon build uses a cryptographically random private cohort tag;
the requested archive tag is never inspected, created, or removed in the
daemon. The validator requires that exact input cohort tag, then deterministically
emits the requested tag in `manifest.json` and `repositories`. The finalizer
writes and fsyncs a private temporary archive, publishes with the platform's
atomic exclusive rename, fsyncs the directory, and normalizes only outer
transport metadata plus those two tag records. A published output is never
rolled back or deleted. A pre-publication interruption can leave a randomized
hidden partial beside the requested output; this avoids any checked-path unlink
race and is not a valid artifact. Image config and `layer.tar` bytes are never
rewritten. Cleanup removes the private cohort before publication and only while
it still resolves to the recorded owned image ID; a foreign replacement is
reported and preserved, and no final artifact is published on cleanup failure.

For reproducibility evidence, repeat the command from a second independent
tracked-clean checkout of the same commit with a fresh output path, then require
both the printed image ID and complete archive SHA-256 to match byte-for-byte.
Load and exercise only the verified archive:

```sh
cmp first/teslatlas-hub.docker.tar second/teslatlas-hub.docker.tar
sha256sum first/teslatlas-hub.docker.tar second/teslatlas-hub.docker.tar
docker image load --input first/teslatlas-hub.docker.tar
```

Only matching bytes establish reproducibility for the recorded toolchain and
inputs. A single candidate archive does not. Matching bytes prove only
deterministic construction from those recorded inputs.
Source readback, Compose health/security/persistence and the remaining upgrade,
rollback, backup/restore, failed-candidate and removal gates still need retained
runtime evidence.

Replace the placeholders from a protected local secret source. Do not put the
tokens in Compose environment values, build arguments, command history, the
image, or a checked-in file. The `setup` command takes the exclusive store lock,
so run it before starting the resident service.

The published port defaults to `127.0.0.1:8443`. Change the explicit host bind
only when the certificate, `public_url`, firewall and trusted hostname agree.
The private Fleet ingestion and command-proxy ports are not published by the
core service. Direct Fleet and Edge delivery are separate collection choices.

`/healthz` reports that the process responds. `/readyz` reports the service's
actual serving conditions and may remain unavailable while setup or collection
is incomplete. Neither endpoint proves that a vehicle recently sent telemetry.
The health probe is an exec-form invocation of this image's Hub binary. It
requires the absolute mounted CA path, disables redirects, trusts only that PEM
bundle, and requires `TESLATLAS_HUB_TLS_SERVER_NAME` to be the lowercase DNS
name present in `server.pem` and the configured `public_url`. The client
resolves only that validation name to `127.0.0.1:8443` for the private probe,
bounds the response, and requires `status=ok` plus the same Hub product version.
It never disables TLS or hostname verification and does not require curl or a
shell.

Stop the service before commands that take the exclusive data-store lock:

```sh
docker compose stop hub
docker compose run --rm hub --config /etc/teslatlas-hub/config.toml verify-backup --source /backup/generation
docker compose start hub
```

Use Hub's supported `backup`, `verify-backup`, `restore-data`,
`export-recovery-credentials`, and `restore-recovery-credentials` commands.
Restore data into a new volume, keep the original volume until verification,
and re-pair clients after a data restore. Removing containers is not data
deletion; removing the named volume is destructive.

## Retain runtime evidence before cleanup

The next exact-export runtime must stage only the redacted files required by
`scripts/finalize-container-runtime-evidence.py`. Its `prepare` phase rejects a
missing or extra file, symlink, empty/oversized result, known secret-shaped
content, source mismatch, non-ARM64 image, unequal host/guest source manifest,
manifest digest/member-count mismatch, unsafe manifest path, or Compose model
that does not completion-order `volume-init`. The input must include the exact
uncompressed `source-archive.tar`; the harness reads its PAX commit, derives its
Git tree and file manifest, and compares its actual bytes and SHA-256 with
`source-archive.json`. Host and guest manifests use sorted
`<sha256>  <path>` lines relative to `source/` for every non-directory archive
member (a symlink hashes its link text). It copies only
that allowlist into a new owner-only bundle and records every relative path,
byte count and SHA-256 plus an aggregate manifest hash:

```sh
python3 scripts/finalize-container-runtime-evidence.py prepare \
  --input /absolute/redacted-pre-cleanup \
  --output /absolute/retained-evidence-bundle \
  --source-commit "$TESLATLAS_HUB_SOURCE_COMMIT"
jq -e '.state == "READY_FOR_CLEANUP"' \
  /absolute/retained-evidence-bundle/manifest.json
```

The prepare manifest generates an `evidence_run_id` and copies the exact
`source_commit` and `cohort` object from the validated build summary. The cohort
identifies the Compose project, image ID, container names, network, volume, and
guest/host runtime roots through the exact keys `compose_project`, `image_id`,
`container_names`, `network_name`, `volume_name`, `guest_runtime_root`, and
`host_runtime_root`. Do not remove the image, cohort, guest root, or lock
unless the ready check passes. After cleanup, write the separate redacted
`cleanup.json` with the same `evidence_run_id`, `source_commit`, and complete
`cohort` object plus every required resource assertion true. Foreign-cohort or
partial cleanup cannot finalize the bundle. Retain the result with the review
receipt:

```sh
python3 scripts/finalize-container-runtime-evidence.py finalize \
  --bundle /absolute/retained-evidence-bundle \
  --cleanup /absolute/cleanup.json
jq -e '.state == "COMPLETE" and .contains_secrets == false' \
  /absolute/retained-evidence-bundle/manifest.json
```

The allowlist covers the exact source tar and matching host/guest manifests,
build/tooling summary, image inspect, binary/source/version readbacks, rendered
Compose model, volume permissions, process security, TLS positive/negative
results, pre/post-restart health/doctor/status, restart, database hash, and
cleanup. Case-insensitive structured credential keys, token assignments,
Bearer credentials, and private-key PEM headers are rejected. Private keys,
tokens, invitations, private configuration, raw
credentials, and unredacted logs must never enter its input.

## Current acceptance boundary

The checked-in packaging script validates immutable base references, absence of
runtime package-manager inputs, explicit copy/permission steps, mandatory
BuildKit epoch/source binding, the health command, adversarial sentinel
preservation,
and the rendered Compose dependency/security topology without creating
containers. The first exact-export ARM64 runtime attempt was rejected after
cleanup because its raw evidence was not retained for independent inspection
and its original empty-volume handoff was unsafe. A Docker build, health
response or local synthetic test does not establish native Linux ARM64/AMD64
support, Fleet telemetry, public companion availability, or live Tesla
collection. Those claims require the isolated runtime, upgrade, recovery and
consumer acceptance records described by the compatibility plan.
