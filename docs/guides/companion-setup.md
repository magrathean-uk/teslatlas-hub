# Companion source setup

Teslatlas Hub can install the six independent companion projects from verified
source. The Hub package contains the audited bootstrap code, schemas, and an
initial catalog. It does not contain companion source trees, dependencies,
Viewer assets, Home Assistant code, Edge binaries, or SDK build products.

The bootstrap requires Python 3.10 or newer. Each selected recipe also checks
its own locked toolchain before changing the active installation. Run companion
commands as the intended unprivileged user. Hub package post-install scripts do
not run them.

## Published source

`install` uses a previously verified cached catalog when present and otherwise
uses the catalog shipped with the installed Hub. It does not contact the
network to refresh catalog selection:

```sh
teslatlas-hub companions install \
  --components protocol,sdk-typescript,viewer \
  --prefix "$HOME/.local/share/teslatlas/companions" \
  --node-bin /absolute/path/to/pinned-node/bin
```

`update` is the explicit catalog-refresh operation. It downloads only
`tools/companions/catalog-current.json` from the fixed Teslatlas Hub `main`
source origin over HTTPS, enforces a 15-second and 1 MiB limit, records the
catalog digest, and then resolves full allowlisted companion commits:

```sh
teslatlas-hub companions update \
  --components protocol,sdk-typescript,viewer \
  --prefix "$HOME/.local/share/teslatlas/companions" \
  --node-bin /absolute/path/to/pinned-node/bin
```

The shipped catalog is deliberately empty until an exact compatible cohort is
reachable from the public Git repositories. The current `2026.36.2` working
candidate is unpublished; production install/update must therefore report that
no admitted reachable cohort exists. It never substitutes `main` for an
immutable recorded commit.

`status` and `rollback` do not read or refresh a catalog and do not need Hub
configuration or Tesla credentials:

```sh
teslatlas-hub companions status \
  --prefix "$HOME/.local/share/teslatlas/companions"
teslatlas-hub companions rollback \
  --prefix "$HOME/.local/share/teslatlas/companions"
```

`setup-companions` is a visible alias for `companions`. Both routes execute the
same packaged helper and always bind selection to the version embedded in the
running Hub binary. There is no command-line Hub-version override.

## Local candidate verification

An unpublished candidate requires an explicit content-bound catalog and local
source manifest. Generate the manifest with the standalone helper, then use the
Hub route:

```sh
teslatlas-hub setup-companions dry-run \
  --mode local-candidate \
  --components protocol,sdk-typescript,viewer \
  --prefix /absolute/private/companions \
  --catalog /absolute/private/catalog.json \
  --local-sources /absolute/private/local-sources.json \
  --node-bin /absolute/path/to/pinned-node/bin
```

The result and errors are one JSON value on standard output. A missing helper,
unsupported Python, invalid source/catalog, missing tool, build failure, or
unsupported target returns nonzero before an unverified release is activated.
An identical verified install is an offline no-op.

## Component targets

- `protocol` installs source-neutral schema and conformance tooling.
- `sdk-typescript` retains the verified npm package for consumer projects.
- `viewer` requires `sdk-typescript` in the same selection and retains its CLI
  runtime under the activated release.
- `sdk-swift` retains the verified Swift package source and release products.
- `home-assistant` requires `--ha-config /absolute/config`; the installer links
  only the integration directory and preserves HA configuration and registry
  data.
- `edge` requires a Linux host and `--edge-target local-linux`. An isolated Go
  toolchain can be bound with both `--edge-go-binary` and `--edge-tool-root`.

Source, dependencies, and outputs remain under the selected companion prefix,
outside the Hub package and Hub database. Pairing, Tesla credentials, telemetry
registration, service start, and Home Assistant reload remain separate explicit
operations.
