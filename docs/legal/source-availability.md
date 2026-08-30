# Corresponding Source availability

For every binary/package, publish:

- exact source archive;
- commit and signed tag;
- build/install scripts;
- lockfiles and vendored source;
- generated-source inputs;
- interface definitions;
- dependency notices and SBOM;
- checksums/signatures;
- installation information where GNU AGPL section 6 requires it.

Keep source access available for the period required by the distribution method.

A modified network deployment must offer the source of the version actually running, not stale upstream source.

## Official v1 source assets

The version-bound GitHub release page is the prominent source landing page. Its
complete Corresponding Source offer comprises both the exact tagged workspace
archive `teslatlas-hub-v1.0.0-beta.1-source.tar.gz` and the source components
inside `teslatlas-hub-v1.0.0-beta.1-evidence.tar.gz`; neither asset alone is
represented as complete. Locked Rust registry source is a deterministic
component of the latter. Generate and verify it with the actual helper contract:

```sh
RUST_CARGO=$(rustup which --toolchain 1.98.0 cargo)
PATH="$(dirname "$RUST_CARGO"):$PATH"
export PATH
python3 scripts/rust-source-evidence.py \
  --repo . \
  --cargo "$RUST_CARGO" \
  --cargo-home "${CARGO_HOME:-$HOME/.cargo}" \
  --bin teslatlas-hub \
  --output-dir dist/release/rust-source-evidence
python3 scripts/rust-source-evidence.py \
  --repo . \
  --verify-dir dist/release/rust-source-evidence \
  --rebuild
```

The output directory contains only `rust-vendored-sources.tar.gz`,
`rust-source-inventory.json`, and `rust-source-evidence-manifest.json`. The
archive carries the Cargo offline source-replacement configuration, exact
Cargo.lock registry `.crate` archives, and independently reconstructed package
trees; the tagged workspace source remains in the separate workspace archive.
`release-evidence.py --rust-source-evidence` stages
all three files inside the deterministic detailed evidence tarball; they are
not separate top-level publication assets.

The complete Fleet Telemetry evidence directory must also be included inside
the detailed evidence tarball. Its `fleet-telemetry-upstream-source.tar.gz`
file is the exact pinned upstream source used by the bridge build.
`fleet-telemetry-go-module-sources.tar.gz` contains the exact source ZIP and
`go.mod` for every one of the 45 locked runtime modules, including the Eclipse
Paho EPL-2.0 source. The generated Fleet notice embedded in the app and Debian
packages points to that archive in detailed release evidence; the archive is
not part of the smaller dependency legal bundle installed with the package.
This Fleet Go source/legal corpus is platform-invariant and does not by itself
prove a native Linux rebuild.
The Go command-proxy evidence similarly contains
`tesla-http-proxy-go-sources.tar.gz`, including the exact upstream module
archives and the tracked, dated `go.mod` overlay applied to the private build
copy. Publish the actual detailed archive, not
only source URLs, locks, manifests, or hashes.

The command-proxy evidence helper supports `darwin-arm64`, `linux-amd64`, and
`linux-arm64`. Its v2 manifest binds the selected target and binary subject,
captures the 20-module cross-platform source lock and 21 source packages, and
records a byte-identical clean target rebuild performed during generation on
the locked Apple-silicon macOS host. `--verify-dir` validates that record and
the complete evidence but does not rerun the rebuild. Official Debian evidence must include the
matching per-architecture Go and Fleet directories as well as the native
receipt that binds the tagged `packaging/linux/sidecar-sha256.lock` and packaged
`SIDECAR_SHA256SUMS`. Darwin evidence is never a substitute for Linux evidence.

The preferred editable source for the raster application icon is the tracked
`macos/TeslatlasHubApp/Artwork/AppIcon.iconset/`. On macOS,
`scripts/build-app-icon.sh` regenerates the distributed `AppIcon.icns` and the
README preview from those inputs. Private creator/assignment and brand-clearance
records remain a separate release-authority gate; their absence is not repaired
by source availability.

Recommended CLI:

```text
teslatlas-hub legal
teslatlas-hub licence
teslatlas-hub source
```

The official binary prints the version-bound GitHub release-page URL that lists
both required source assets. `/.well-known/teslatlas-hub` exposes that same
immutable landing page. The macOS app provides a Corresponding Source menu item
bound to its embedded Hub version. Release notes must identify the workspace
archive and detailed evidence archive as the complete two-part source set.

An operator distributing a modified build must replace that route with the
complete Corresponding Source for the version actually served.
