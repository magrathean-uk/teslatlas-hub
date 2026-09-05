# Build from source

Hub is source-only: no prebuilt GitHub releases or installer downloads are
provided. Existing tags remain historical source snapshots. Use `main` for the
latest fixes and record the exact commit used for your build.

```sh
git clone https://github.com/magrathean-uk/teslatlas-hub.git
cd teslatlas-hub
git rev-parse HEAD
```

## Apple-silicon Mac

Install Xcode and its command-line tools, XcodeGen, Rust through rustup, and Go.
The packaging helpers enforce the Rust version in `Cargo.toml` and companion
toolchain requirements. The previously verified toolchains were Rust 1.98,
Go 1.27.0 and Xcode 27. Building downloads locked dependency source material.

```sh
./scripts/build-macos-app.sh
codesign --verify --deep --strict "dist/Teslatlas Hub.app"
pkgutil --payload-files dist/TeslatlasHub.pkg
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
cargo build --locked --release --bin teslatlas-hub
mkdir -p dist
python3 scripts/legal-bundle.py --repo . --output-dir dist/dependency-legal
HUB_VERSION=$(target/release/teslatlas-hub --version | awk '{print $2}')
HUB_ARCH=$(dpkg --print-architecture)
scripts/build-deb.sh \
  --binary target/release/teslatlas-hub \
  --legal-bundle dist/dependency-legal \
  --version "$HUB_VERSION" --architecture "$HUB_ARCH" \
  --output "dist/teslatlas-hub_${HUB_VERSION}_${HUB_ARCH}.deb"
```

Follow [Debian installation](install-debian.md) using that local package.
This command builds core/Legacy functionality only. Fleet requires both
compatible companions and their evidence bundles; see
[Fleet setup](fleet-setup.md) and the packaging script's options. Do not replace
an existing Fleet deployment with a core-only package.

## Keep your build identifiable

Retain the source commit, toolchain versions and local package checksum.
Back up before replacing an installed version. Source builds are not proof of
successful live collection or backup recovery. If you redistribute binaries,
include the corresponding source and required legal material described in
[source availability](../legal/source-availability.md).
