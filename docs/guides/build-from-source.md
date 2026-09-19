# Build from source

Hub packages include only the audited companion bootstrap modules and their
small schemas/catalog. The five active companion repositories, dependency trees, and
built outputs remain outside the Hub payload and are created only by the
unprivileged command described in [Companion source setup](companion-setup.md).

Hub is source-only: no prebuilt GitHub releases or installer downloads are
provided. Existing tags remain historical source snapshots. Distributable
builds must receive the exact pushed Hub commit explicitly; the build never
infers identity from Git metadata or the current directory.

```sh
git clone https://github.com/magrathean-uk/teslatlas-hub.git
cd teslatlas-hub
HUB_SOURCE_COMMIT=$(git rev-parse HEAD)
SOURCE_DATE_EPOCH=$(git show -s --format=%ct "$HUB_SOURCE_COMMIT")
export SOURCE_DATE_EPOCH
test -z "$(git status --short)"
git cat-file -e "${HUB_SOURCE_COMMIT}^{commit}"
git ls-remote origin | awk -v commit="$HUB_SOURCE_COMMIT" '$1 == commit { found=1 } END { exit !found }'
```

## Apple-silicon Mac

Install Xcode and its command-line tools, XcodeGen, Rust through rustup, and Go.
The packaging helpers enforce the Rust version in `Cargo.toml` and companion
toolchain requirements. The previously verified toolchains were Rust 1.98,
Go 1.27.0 and Xcode 27. Building downloads locked dependency source material.

```sh
TESLATLAS_HUB_SOURCE_COMMIT="$HUB_SOURCE_COMMIT" ./scripts/build-macos-app.sh
codesign --verify --deep --strict "dist/Teslatlas Hub.app"
pkgutil --payload-files dist/TeslatlasHub.pkg
```

If Go 1.27.0 is installed outside the default `PATH`, select its executable
explicitly. `TESLATLAS_GO` must be an absolute path to an executable file; the
packaging parent, both Go companion builds and their evidence generators use
the same selection and still reject any version other than exactly Go 1.27.0.
Quote the path when it contains spaces. Go proxy evidence generation also
requires the selected executable's resolved path, SHA-256 and reported GOROOT
to match one complete reviewed host identity in `scripts/tesla-proxy-lock.json`;
an arbitrary Go 1.27.0 installation is not sufficient.

```sh
TESLATLAS_GO="/absolute/path/to/go" \
  TESLATLAS_HUB_SOURCE_COMMIT="$HUB_SOURCE_COMMIT" ./scripts/build-macos-app.sh
```

Install `dist/TeslatlasHub.pkg`, then follow [Mac setup](install-macos.md).
The combined package installs both the control app and background service.
An ad-hoc build is not Developer ID signed or notarised; macOS or organisation
policy may block it. Do not disable system-wide security controls. In-app
installation or version-changing service updates require trusted release
metadata; use your locally built combined package instead. Reconnecting an
account can reuse an already installed matching service.

## Debian 13 core/Legacy package

Build natively on amd64 or arm64. Install the Rust toolchain required by
`Cargo.toml`, a C build toolchain, Python 3 and Debian packaging tools.
Use a fresh output directory; legal-bundle generation refuses to overwrite one.

```sh
cargo fetch --locked
HUB_SOURCE_ROOT=$(pwd -P)
TESLATLAS_HUB_SOURCE_COMMIT="$HUB_SOURCE_COMMIT" \
  RUSTFLAGS="--remap-path-prefix=${HUB_SOURCE_ROOT}=/usr/src/teslatlas-hub" \
  cargo build --locked --release --bin teslatlas-hub
mkdir -p dist
python3 scripts/legal-bundle.py --repo . --output-dir dist/dependency-legal
HUB_VERSION=$(target/release/teslatlas-hub --version | awk '{print $2}')
HUB_ARCH=$(dpkg --print-architecture)
scripts/build-deb.sh \
  --binary target/release/teslatlas-hub \
  --legal-bundle dist/dependency-legal \
  --version "$HUB_VERSION" --architecture "$HUB_ARCH" \
  --source-commit "$HUB_SOURCE_COMMIT" \
  --output "dist/teslatlas-hub_${HUB_VERSION}_${HUB_ARCH}.deb"
```

`cargo fetch --locked` is required before legal-bundle generation. The legal
gate reads all locked target-specific package metadata offline, including
dependencies that are not compiled for Linux; an ordinary native build alone
does not populate that complete cache. The source-path remap prevents the
checkout's absolute path from changing otherwise identical release binaries;
keep the fixed destination exactly as shown. `SOURCE_DATE_EPOCH` is required by
the Debian packager and must remain the selected pushed commit's timestamp so
archive member metadata is reproducible across fresh source exports.

Follow [Debian installation](install-debian.md) using that local package.
This command builds core/Legacy functionality only. Fleet requires both
compatible companions and their evidence bundles; see
[Fleet setup](fleet-setup.md) and the packaging script's options. Do not replace
an existing Fleet deployment with a core-only package.

## Keep your build identifiable

An ordinary `cargo build` without `TESLATLAS_HUB_SOURCE_COMMIT` remains useful
for development: discovery reports the repository root, while `teslatlas-hub
source` fails and the legal notice identifies the build as unbound and
non-distributable. Retain the source commit, toolchain versions and local package checksum.
Back up before replacing an installed version. Source builds are not proof of
successful live collection or backup recovery. If you redistribute binaries,
include the corresponding source and required legal material described in
[source availability](../legal/source-availability.md).
