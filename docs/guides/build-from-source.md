# Build from source

Hub is distributed as source. There are no current prebuilt GitHub installers.
Existing tags and release notes describe historical source states. The current
manifest is `2026.36.2`, with Rust edition 2024 and a minimum Rust version of 1.98.
A successful build does not establish live collection or platform acceptance.

## Develop the Rust service

Use rustup and a C toolchain suitable for the host. Fetch the repository and run
commands from its root:

```sh
git clone https://github.com/magrathean-uk/teslatlas-hub.git
cd teslatlas-hub
cargo check --locked
cargo test --locked --lib TEST_FILTER
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
```

Replace `TEST_FILTER` with the affected test name. Run broader tests when a
change affects shared behaviour. The maintained Teslatlas workspace has its own
runner and heavy-build coordination; use those existing controls when working
there instead of these standalone commands. The workspace pins Rust 1.98.1.

## Teslatlas Compute

Hub uses the Teslatlas Compute library (`teslatlas-compute`, Apache-2.0) from
<https://github.com/magrathean-uk/teslatlas-compute>. `Cargo.toml` pins the
corresponding published Compute source to one full commit; `Cargo.lock` records
the same commit. That source includes the bounded paged-generation API and
algorithm identity 0.1.1, while its package version remains 0.1.0. Cargo fetches
the library with the other locked dependencies, so no sibling checkout is
required. `cargo fetch --locked` also fetches it for offline builds.

For an ordinary local library check, an explicit Cargo path override may select
the sibling checkout without changing the manifest or lockfile:

```sh
cargo check --locked --config 'paths=["../teslatlas-compute"]'
```

Do not use that override for a source-bound or distributed build. The existing
source-evidence gates remain enabled. A source commit and dependency binding
do not establish package, installed, live or platform acceptance.

## Source identity

An ordinary development build has no distributable source identity. Without
`TESLATLAS_HUB_SOURCE_COMMIT`, discovery identifies the source repository but
`teslatlas-hub source` fails. Do not present that binary as a source-bound release.

## Bind distributable builds to source

Start from a tracked-clean checkout at the exact official pushed commit you
intend to reproduce. These commands only inspect local and remote source state:

```sh
HUB_SOURCE_COMMIT=$(git rev-parse HEAD)
SOURCE_DATE_EPOCH=$(git show -s --format=%ct "$HUB_SOURCE_COMMIT")
export SOURCE_DATE_EPOCH
test -z "$(git status --short)"
git cat-file -e "${HUB_SOURCE_COMMIT}^{commit}"
git ls-remote origin | awk -v commit="$HUB_SOURCE_COMMIT" '$1 == commit { found=1 } END { exit !found }'
```

The Hub commit's `Cargo.lock` fixes the Compute source commit. Keep the commit,
toolchain versions and artifact checksums with your build record.
Build identity does not confer signing, notarisation or support.

## macOS app and combined installer

The source includes an AppKit control app and a combined app/service packager
for Apple-silicon macOS, with a macOS 13 deployment target. The current
`scripts/build-macos-app.sh` is workspace-specific: it requires the parent
workspace runner, external output locations and a reviewed Go host identity.
A standalone clone does not contain that runner. There is no complete portable
Mac installer recipe in this checkout.

The packager checks Xcode, XcodeGen, rustup and exactly Go 1.27.1. It reads the
current absolute `buildRoot` from the JSON emitted by
`clean-development status --json` and accepts Cargo output only under that
root's managed Hub directory.
Do not hard-code a cache path or set Cargo's target directory around the runner.

`scripts/tesla-proxy-lock.json` records the Go 1.27.1 command-proxy subjects
and the reviewed build host: the selected Go executable and its binary, Xcode
27.0, its Apple Clang toolchain and the macOS 27.0 SDK. `TESLATLAS_GO` may
select only a Go executable recorded there; another Go 1.27.1 installation is
insufficient. The lock covers the proxy component only. It does not establish a
working combined package or installed Hub.

The packaging source checks select exactly the maintained Rust 1.98.1 toolchain
for `Cargo.toml`'s `1.98` minimum and reject incompatible minimum changes. That
check proves the source-level toolchain selection only; a successful local
package build and ordinary-user install or upgrade acceptance remain required.

Within the maintained workspace, the entry point is:

```sh
TESLATLAS_GO=/absolute/reviewed/path/to/go \
  TESLATLAS_HUB_SOURCE_COMMIT="$HUB_SOURCE_COMMIT" \
  ../scripts/dev/run.sh hub ./scripts/build-macos-app.sh
```

The script prints the app and package paths on success. Its output root follows
the workspace's configured lab and managed build roots, so inspect the printed
paths instead of assuming a location under the source checkout. Mac builds are not
automatically Developer ID signed or notarised. Do not disable macOS security
controls to install one. Once you have a validated package, follow
[Mac setup](install-macos.md). The
[source-run control app](../../macos/TeslatlasHubApp/DEVELOPMENT.md) describes
unsigned development mode separately.

## Mac to Debian ARM64 development build

The maintained workspace can cross-build the Rust service on Apple silicon with
Rust 1.98.1, cargo-zigbuild 0.23.4 and Zig 0.16.0. Install the official Rust target
`aarch64-unknown-linux-gnu` on the build host; no guest toolchain is needed.
The `.2.28` suffix below selects the glibc ABI baseline.

Run from the workspace root after loading its Debian ARM64 tool and cache setup
in `docs/development/ENVIRONMENT.md`. That parent-workspace document and its runners
are not part of a standalone Hub clone. Keep Cargo output and Zig caches outside
the source tree, and invoke Cargo directly through the managed runner:

```sh
source ~/dev/env.zsh
scripts/dev/with-heavy-build-lock.sh scripts/dev/run.sh hub \
  cargo zigbuild --locked --release --target aarch64-unknown-linux-gnu.2.28 \
  --bin teslatlas-hub --message-format json-render-diagnostics
scripts/dev/with-heavy-build-lock.sh scripts/dev/run.sh hub \
  cargo zigbuild --locked --release --target aarch64-unknown-linux-gnu.2.28 \
  --test tls_import_e2e --message-format json-render-diagnostics
```

The second command builds the existing TLS test executable without running it.
Resolve each executable from Cargo's JSON artifact record; do not assume a managed
target path or wrap Cargo inside another shell command. Copy the regular executable
files to the Debian guest. On the guest, select the copied CLI explicitly and run
the copied test harness directly:

```sh
TESLATLAS_HUB_SMOKE_BINARY=/absolute/path/to/teslatlas-hub \
  /absolute/path/to/tls_import_e2e-TEST_ARTIFACT --test-threads=1
```

The local 2026-10-01 check ran the exact 20,188,176-byte unbound development CLI on
Debian 13.7 ARM64 with glibc 2.41. Its isolated, credential-free service passed
start/restart/stop, certificate and hostname validation, config/PID-bound HTTPS
readiness, all eight doctor checks, persistent identity across restart and data-only
backup/restore. Both signed TLS tests passed under the guest's normal `0002` umask.
This establishes the bounded development route, not an official package,
companion-service installation, live collection or Mac App-to-Debian acceptance.

Linux runs ordinary `serve` with CLI/systemd controls; the macOS source-run control
app and its development-mode admission remain separate. See
[Debian service operation](install-debian.md#start-and-inspect).

## Debian core package

`scripts/build-deb.sh` accepts a prebuilt service binary, legal bundle, exact
source commit, version, architecture and output path. It supports `amd64` and
`arm64` package metadata. Package construction does not prove the resulting
binary works on Debian 13 or on either architecture.

The cross-built development ELF above is unbound and cannot be presented as an
official package. The existing builder executes the supplied binary to validate
version and source identity, and requires Linux ELF and Debian dependency tools;
it does not package a Linux executable directly on macOS. Keep its source and
provenance checks intact.

The following recipe is for a standalone native Debian build host with Python 3,
Rust, a C toolchain and Debian packaging tools. The owner's test VMs run host-built
artifacts and are not build hosts. Use a new, absolute directory outside the
checkout for `HUB_BUILD`; legal generation refuses to overwrite an existing bundle.

```sh
HUB_BUILD=/absolute/path/to/new-hub-build
HUB_SOURCE_ROOT=$(pwd -P)
mkdir -p "$HUB_BUILD"
cargo fetch --locked
TESLATLAS_HUB_SOURCE_COMMIT="$HUB_SOURCE_COMMIT" \
  RUSTFLAGS="--remap-path-prefix=${HUB_SOURCE_ROOT}=/usr/src/teslatlas-hub" \
  cargo build --locked --release --bin teslatlas-hub --target-dir "$HUB_BUILD/target"
python3 scripts/legal-bundle.py --repo . --output-dir "$HUB_BUILD/dependency-legal"
HUB_VERSION=$("$HUB_BUILD/target/release/teslatlas-hub" --version | awk '{print $2}')
HUB_ARCH=$(dpkg --print-architecture)
scripts/build-deb.sh \
  --binary "$HUB_BUILD/target/release/teslatlas-hub" \
  --legal-bundle "$HUB_BUILD/dependency-legal" \
  --version "$HUB_VERSION" --architecture "$HUB_ARCH" \
  --source-commit "$HUB_SOURCE_COMMIT" \
  --output "$HUB_BUILD/teslatlas-hub_${HUB_VERSION}_${HUB_ARCH}.deb"
```

`cargo fetch --locked` supplies all locked dependency metadata for the offline
legal gate, including target-specific dependencies. Keep the fixed source-path
remap and the source commit timestamp for reproducibility. This package is
core/Legacy only. Fleet also requires compatible companions and evidence;
see [Fleet setup](fleet-setup.md). Do not replace a Fleet deployment with a
core-only package. See [Debian installation](install-debian.md) for local use.

## Container candidate

`scripts/build-container-image.sh` builds a Linux ARM64 candidate image archive.
It takes `--tag REPOSITORY:TAG` and `--output PATH`, checks the official source
identity, and uses a fresh Git archive context. It requires Docker and Buildx;
its archive validation is specific to the supported Docker save format.

Follow the [Docker guide](install-docker.md) for the exact requirements and
candidate checks. Use an output path outside the checkout. A candidate archive
is not evidence of a working installation or reproducibility; that requires
independent builds and the documented runtime checks.

## Validate and redistribute

Test installation, start/stop, pairing, collection and backup recovery for the
exact artifact and platform before relying on it. Back up before replacing an
installation. Include the required licence texts, notices and
[Corresponding Source](../legal/source-availability.md) when redistributing.
