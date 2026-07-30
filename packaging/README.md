# Native Debian delivery

Status: package prototype. An earlier amd64 Debian 12 bench snapshot passed
installation and verification. Current committed-source, arm64, signed public
release, and stable one-command bootstrap proof remain open. See
[Current status](../docs/STATUS.md).

The Hub is installed as a normal Debian package and supervised by systemd. It
does not require Docker, a language toolchain, or a database daemon on the
target host.

## Build

Run on the matching target architecture:

```sh
cargo build --release --locked
packaging/build-deb.sh --version 0.1.0
```

This produces `dist/teslatlas-hub_0.1.0_ARCH.deb`. Build one artifact on amd64
and one on arm64. The package never starts or enables the service itself.

## Local bench install

Use this before any public release:

```sh
sudo scripts/install.sh --local-artifact ./dist/teslatlas-hub_0.1.0_amd64.deb
```

Then create the protected LAN identity, start Hub, and display the pairing QR:

```sh
sudo teslatlas-hub-setup
```

## Native Git bootstrap

For a reviewed immutable source commit, a single native command can fetch,
build, package, and install Hub without Docker. The bootstrap refuses moving
branch heads: supply both the release ref and its full reviewed object ID.
It can securely prompt for the token after the package is built.

```sh
curl -fsSLO https://YOUR-HUB-REPOSITORY.example/bootstrap-from-git.sh
sudo bash bootstrap-from-git.sh \
  --repo https://github.com/OWNER/REPOSITORY.git \
  --ref v0.1.0 --commit FULL_REVIEWED_COMMIT_SHA --prompt-token
```

Fleet authorization is deliberately later. The first native bootstrap accepts
only the token-safe path, leaving no plaintext token in its arguments or
configuration.

`scripts/install.sh --dry-run` and `scripts/bootstrap-from-git.sh --dry-run`
perform no package, filesystem, service, credential, or network change. The
bootstrap dry-run still validates `--repo`, `--ref`, and `--commit` before
printing the planned actions. `--no-start` leaves the package installed but
inactive.

## Signed release install

Public release mode needs two explicit trust anchors: a GitHub repository and a
Minisign public key that was distributed independently of the release asset.

```sh
sudo scripts/install.sh \
  --repo OWNER/REPOSITORY \
  --release-key /path/to/teslatlas-hub.minisign.pub \
  --version v0.1.0
```

The script downloads a detached-signed manifest, verifies it with the pinned
key, then downloads and SHA-256 verifies the architecture-specific `.deb`.
There is intentionally no insecure default repository or key. Once project
identity is fixed, the public key may be embedded in the published bootstrap
script to make the production command fully parameter-free.

Release artifacts must include:

```text
teslatlas-hub_VERSION_amd64.deb
teslatlas-hub_VERSION_arm64.deb
teslatlas-hub.manifest
teslatlas-hub.manifest.minisig
```

Create the final two with a signing key held outside this repository:

```sh
scripts/write-release-manifest.sh \
  --version VERSION \
  --artifacts dist \
  --secret-key /secure/path/teslatlas-hub.minisign.key \
  --out release
```

## Credentials

Import a legacy owner token from a protected file, or use the system password
agent:

```sh
sudo scripts/install.sh --local-artifact ./dist/teslatlas-hub_0.1.0_amd64.deb \
  --token-file /secure/path/owner-token

sudo scripts/install.sh --local-artifact ./dist/teslatlas-hub_0.1.0_amd64.deb \
  --prompt-token
```

The file input must be a regular non-symlink file with no group or world
permissions. `--prompt-token` uses `systemd-ask-password` with echo disabled.
Both paths stream directly into `systemd-creds encrypt --with-key=host`; the
only staged file is encrypted ciphertext at
`/etc/teslatlas/credentials/owner-token`. The installer also creates one
random 32-byte binary cursor-signing key, pipes it directly from
`/dev/urandom` into `systemd-creds`, and retains its encrypted ciphertext at
`/etc/teslatlas/credentials/cursor-key`. No plaintext key file is created.

Both the main and explicit collection units always receive the cursor key as
`$CREDENTIALS_DIRECTORY/cursor-key`. If an owner token was imported, those
same units also receive `$CREDENTIALS_DIRECTORY/owner-token`. Reinstalling
retains the existing cursor key so previously issued cursors remain valid.

A token never appears in a command argument, environment variable, log line,
configuration file, or plaintext temporary file. Omitting both token options
still creates the cursor key, but no owner token credential or owner-token
drop-in, so the Hub remains usable without a token.

## Explicit compatibility collection

The packaged collector is intentionally a manual systemd unit. It has no
default Tesla URL, no schedule, no command endpoints, and never wakes a
vehicle. Set an explicit public HTTPS compatibility API base in the
`[collector]` section of `/etc/teslatlas/config.toml`, then run:

```sh
sudo systemctl start teslatlas-hub-collect.service
```

It discovers legacy owner-token vehicles through `/api/1/products`, then reads
`vehicle_data` only for vehicles already reported online. Ongoing Tesla data
will move to Fleet Telemetry rather than regular `vehicle_data` polling.
Every successful compatibility collection publishes a signed car-only Hub
snapshot. This gives the iPhone a real paired source immediately, but never
fabricates a completed trip, position, or charge from a single present-state
response.

## Explicit TeslaMate history migration

This path is manual, TLS-only, and source read-only. First add a credential-
free source URL and durable source label to `/etc/teslatlas/config.toml`:

```toml
[teslamate]
source_url = "postgresql://teslamate_reader@db.internal/teslamate"
source_key = "home-teslamate"
```

Create the encrypted PostgreSQL password credential with a protected input
file, then attach it to the installed import template:

```sh
sudo install -d -m 0700 /etc/teslatlas/credentials
sudo systemd-creds encrypt --with-key=host --name=teslamate-postgres-password \
  /secure/path/teslamate-reader-password \
  /etc/teslatlas/credentials/teslamate-postgres-password
sudo chmod 0600 /etc/teslatlas/credentials/teslamate-postgres-password
sudo install -d -m 0755 /etc/systemd/system/teslatlas-hub-import@.service.d
printf '[Service]\nLoadCredentialEncrypted=teslamate-postgres-password:/etc/teslatlas/credentials/teslamate-postgres-password\n' | \
  sudo tee /etc/systemd/system/teslatlas-hub-import@.service.d/10-teslamate-postgres-password.conf >/dev/null
sudo systemctl daemon-reload
sudo systemctl start teslatlas-hub-import@CAR_ID.service
```

The installer attaches an existing encrypted migration password automatically
on upgrade. The template also receives the cursor signing key. The first
release imports full snapshots only; it rejects a selected-car history above
one million rows rather than overcommitting small hosts.

## Bench validation

On the Debian bench VM after a local package install, run:

```sh
sudo teslatlas-hub-verify
```

This performs `systemd-analyze verify` against the installed unit, then checks
the active service, Hub database and readiness endpoint without changing state.

## Removal

`dpkg --remove` and `dpkg --purge` intentionally preserve Hub data and
credentials. A separate explicit data-destruction tool will be required before
this project ever removes user telemetry.
