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

## Distribution status

For 2026.36.1, the release's sanitised `BUILD-INFO.md` records the exact source
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
generated with `scripts/rust-source-evidence.py` from `Cargo.lock`.

## Runtime source route

The CLI exposes the licence and source information used by the running build:

```text
teslatlas-hub legal
teslatlas-hub licence
teslatlas-hub source
```

The macOS app exposes the same source information through its application menu,
and `/.well-known/teslatlas-hub` includes the source route for paired clients.

An operator who modifies or hosts Hub must offer the source of the version
actually running, not an unrelated tag or a newer `main` checkout.
