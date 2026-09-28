# Corresponding Source availability

Teslatlas Hub 2026.36.1 is distributed under `AGPL-3.0-only`. Its corresponding
source boundary is the annotated Git tag `v2026.36.1`:

```sh
git clone https://github.com/magrathean-uk/teslatlas-hub.git
cd teslatlas-hub
git checkout --detach v2026.36.1
git status --short
```

The final command should print nothing. The tag contains the Hub source,
platform packaging, lockfiles, interface definitions, licence texts, notices,
and the inputs needed by the documented build helpers.

## Teslatlas Compute

Current `main` links the Teslatlas Compute library (`teslatlas-compute`),
licensed under Apache-2.0 and published at
<https://github.com/magrathean-uk/teslatlas-compute>. `Cargo.toml` names it as
a Git dependency at one full commit, and `Cargo.lock` records that commit in the
package source. A Hub commit therefore identifies the exact library source, in
the same way as every other locked dependency.

The Corresponding Source of a Hub build that links the library includes the
library source at the commit locked in that build's `Cargo.lock`. Keep the
library's `LICENSE` and `NOTICE` with any distribution. Tag `v2026.36.1`
predates this dependency.

## Distribution status

Hub is now source-only. GitHub release pages and binary assets were withdrawn
on 5 September 2026; source tags remain available for previously distributed
versions. Current fixes are on `main`. Users can [build locally](../guides/build-from-source.md).

For previously downloaded 2026.36.1 packages, the sanitised `BUILD-INFO.md` records the exact source
commit and package scope. Compare it to the tag and use `SHA256SUMS` to verify
distributed bytes. Packages must retain their applicable project and dependency
legal material. Debian core-only packages omit Fleet companions; source and
evidence for components that are distributed must match those exact components.

### Historical v1.0.0

There is no GitHub Release page and no downloadable GitHub release asset for
v1.0.0. The repository distributes source. A combined macOS package can be
built locally as `dist/TeslatlasHub.pkg`; Debian packages can be built locally
for their target architecture.

Anyone who distributes those packages or another object-code build must make
the complete corresponding source for the exact distributed version available
under the GNU AGPL. That offer must include the build and installation material
required by the chosen distribution method.

## Dependency source material

The build helpers generate exact dependency inventories and source evidence
from the locked inputs:

```sh
python3 scripts/go-proxy-evidence.py --repo . \
  --verify-dir dist/go-proxy-evidence
python3 scripts/fleet-telemetry-evidence.py --repo . \
  --verify-dir dist/fleet-telemetry-evidence
python3 scripts/legal-bundle.py --repo . \
  --go-proxy-evidence dist/go-proxy-evidence \
  --fleet-telemetry-evidence dist/fleet-telemetry-evidence \
  --verify-dir dist/dependency-legal
```

Fleet evidence includes the pinned upstream source and the source ZIP plus
`go.mod` for each locked runtime module. Go command-proxy evidence includes the
locked upstream module sources and tracked overlay. Rust dependency evidence is
generated with `scripts/rust-source-evidence.py` from `Cargo.lock`. It holds
every locked crate archive and, for a Git dependency such as Teslatlas Compute,
the locked commit object and the source tree of that commit. Verification checks
the tree against the commit without network access.

```sh
cargo fetch --locked
python3 scripts/rust-source-evidence.py --repo . \
  --output-dir dist/rust-source-evidence
python3 scripts/rust-source-evidence.py --repo . \
  --verify-dir dist/rust-source-evidence
```

## Runtime source route

The CLI exposes the licence and source information used by the running build:

```text
teslatlas-hub legal
teslatlas-hub licence
teslatlas-hub source
```

For a bound build, `source` and the macOS menu identify the exact
`/tree/<40-hex-commit>` URL embedded by the build. The `Cargo.lock` in that
tree pins the Teslatlas Compute commit the build uses. An unbound developer build
keeps the discovery schema valid by reporting the repository root, but the
`source` command fails and the legal notice marks the build non-distributable.
The macOS app omits its Corresponding Source menu item when unbound.

An operator who modifies or hosts Hub must offer the source of the version
actually running, not an unrelated tag or a newer `main` checkout.

## If you distribute or host a modified Hub

- Give recipients the complete Corresponding Source for the exact version you
  distribute, including build and installation scripts, lockfiles and
  interface definitions. A link to a moving branch is not enough.
- If people use your modified Hub over a network, offer them the Corresponding
  Source of the version actually running (section 13 of the licence). The
  `legal` and `source` commands or an About screen can do this.
- Keep the licence, notices and attribution, and mark your changes with the
  date.
