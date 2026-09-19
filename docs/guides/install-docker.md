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
export TESLATLAS_HUB_TLS_SERVER_NAME=hub.example.invalid # replace with the DNS name in server.pem and public_url
test -z "$(git status --short)"
git ls-remote origin | awk -v commit="$TESLATLAS_HUB_SOURCE_COMMIT" '$1 == commit { found=1 } END { exit !found }'
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

Compose requires that explicit exact pushed commit. The Docker build fails
closed when it is absent or not 40 lowercase hexadecimal characters; it never
infers source identity from the build directory. The `source` readback above is
the required runtime binding check for an exported-source image; do not treat a
successful build alone as source-provenance evidence. The build context also
excludes Git metadata, local configuration, TLS material and the Cargo target
cache; only the Dockerfile's explicit source and legal `COPY` inputs enter the
build.

The one-shot volume initializer runs only after the image exists. It drops all
capabilities and adds back only `CHOWN`, `FOWNER`, and `DAC_OVERRIDE`, changes
only `/var/lib/teslatlas-hub` in the named volume, and fails unless the result
is exactly UID/GID `10001:10001` with mode `0700`. The normal Hub service still
drops every capability and never recursively changes ownership. Bind-mounted
data must already have the private ownership and modes required by Hub.

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

## Current acceptance boundary

The checked-in packaging script validates immutable base references, absence of
runtime package-manager inputs, source binding, the health command, and Compose
security shape without creating containers. A Docker build, health response or
local synthetic test does not establish native Linux ARM64/AMD64 support,
Fleet telemetry, public companion availability, or live Tesla collection.
Those claims require the isolated runtime, upgrade, recovery and consumer
acceptance records described by the compatibility plan.
