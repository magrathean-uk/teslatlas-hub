# Companion source bootstrap

`scripts/bootstrap-companions.py` installs the five supported companion
components from fixed, allowlisted source recipes: protocol, TypeScript SDK,
Swift SDK, Home Assistant, and Edge. It accepts catalog records and immutable
source or output digests; catalog data cannot supply executable commands.

Production mode admits published cohorts. Local candidate mode requires a
byte-complete source manifest and labels receipts `local-unpublished`. The
manifest covers installable source files and executable bits while excluding
Git metadata, dependency trees, virtual environments, build output, and caches.
Symlinks and other nonregular source files are rejected.

## Commands

Run these commands from the Hub repository root. Replace all `/private/...`
paths with fresh absolute locations you own. Create a local manifest:

```sh
scripts/bootstrap-companions.py manifest \
  --components protocol,sdk-typescript,sdk-swift,home-assistant,edge \
  --source protocol=/absolute/path/teslatlas-protocol \
  --source sdk-typescript=/absolute/path/teslatlas-sdk-typescript \
  --source sdk-swift=/absolute/path/teslatlas-sdk-swift \
  --source home-assistant=/absolute/path/teslatlas-home-assistant \
  --source edge=/absolute/path/teslatlas-edge \
  --output /private/local-sources.json
```

Run a candidate dry run or installation with an explicit catalog and prefix:

```sh
scripts/bootstrap-companions.py dry-run \
  --mode local-candidate --components protocol,sdk-typescript,sdk-swift,home-assistant,edge \
  --hub-version 2026.36.2 --catalog /private/catalog.json \
  --local-sources /private/local-sources.json --node-bin /private/node/bin \
  --prefix /private/companions
```

Replace `dry-run` with `install` to install. For Edge on Linux, provide
`--edge-target local-linux`, `--edge-go-binary` and `--edge-tool-root`.

`update` may choose a later cohort only when the catalog admits it for the
installed Hub version. `status` recovers an interrupted transaction before
reading its receipt. `rollback` selects the latest fully verified retained
release. `remove` changes only installer-owned component links and retained
releases, preserving `PREFIX/data` and `PREFIX/config`.

All replaceable inputs and outputs live under `PREFIX/releases`, with
`PREFIX/active` as the atomic activation link. Transactions record and validate
the prior and intended state. Failed operations remove staging data and leave
the previous active link in place. The bootstrap prepares code only: it does
not start services, register telemetry, or handle Tesla credentials.

The historical `d1-plan` and `packaging/components.json` records are evidence,
not current selectors. A one-component `d1-install` record is not a valid
five-companion cohort. Installed output manifests hash generated paths when
they are part of retained output, even if their names are normally excluded
from source manifests.
