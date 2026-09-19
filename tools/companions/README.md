# Companion source bootstrap

`hub/scripts/bootstrap-companions.py` is the noninteractive source installer for
the five active Teslatlas companions. It accepts only fixed recipes and the
five allowlisted GitHub origins. Catalog data selects immutable source inputs; it
cannot provide commands.

Production mode accepts published cohorts only. It fetches each full Git commit
without tags or prompts, checks the detached commit and complete source digest,
verifies product/profile metadata and dependency locks, and then runs the fixed
recipe. Local candidate mode requires a separately generated, byte-complete
source manifest and labels receipts `local-unpublished` with public availability
pending.

The manifest covers every installable source file and its executable bit. Git
and agent-workspace metadata, dependency trees, virtual environments, compiler
outputs and caches are intentionally excluded; dependencies and outputs are
rebuilt by the fixed recipes. Source symlinks and nonregular files are rejected.

Create a local manifest without modifying repository refs:

```sh
hub/scripts/bootstrap-companions.py manifest \
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
hub/scripts/bootstrap-companions.py dry-run \
  --mode local-candidate \
  --components protocol,sdk-typescript,sdk-swift,home-assistant,edge \
  --hub-version 2026.36.2 \
  --catalog /private/catalog.json \
  --local-sources /private/local-sources.json \
  --node-bin /private/node-v26.7.0/bin \
  --prefix /private/companions
```

For an isolated Edge toolchain, pass both verified paths:

```sh
hub/scripts/bootstrap-companions.py install \
  --mode local-candidate \
  --components edge \
  --hub-version 2026.36.2 \
  --catalog /private/catalog.json \
  --local-sources /private/local-sources.json \
  --edge-target local-linux \
  --edge-go-binary /private/toolchains/go/bin/go \
  --edge-tool-root /private/toolchains \
  --prefix /private/companions
```

`install` selects the Hub's same cohort by default. `update` may select a later
cohort only when that catalog record explicitly admits the installed Hub
version. SDK and Home Assistant catalog records bind reviewed output hashes.
Catalogs still cannot provide commands. `status` recovers an interrupted
transaction before reading the active receipt and therefore requires an
unprivileged caller with access to the installation. `rollback` selects the most recent fully
verified retained release. Install and update acquire the operation lock,
recover, and compare the exact input before Git transport or build-tool probes.
Identical input is a locked no-op only after every retained source and installed
output byte has been reverified. Receipts from the earlier incomplete
output-manifest schema are rejected for status, no-op, reuse, and rollback.

The retained `d1-plan` input is historical source-selection evidence only.
`d1-install` fails closed because that one-component record is not an exact
five-companion F6 catalog and cannot establish current cohort identity.
`packaging/components.json` is the immutable historical D1 aggregate record;
its deferred Viewer entry is preserved as history and is not an active selector
or input to `catalog-current.json`.

The TypeScript SDK output is its verified package archive. Protocol retains its
verified source snapshot beside a `source-path.txt`
pointer because it is source tooling rather than a service. Swift retains the
verified source plus the complete SwiftPM release product directory. Installed
output manifests use a separate traversal with no source-cache exclusions, so
paths named `node_modules`, `dist`, `.build`, `.venv`, or `target` are hashed
when they are part of a retained output. Output symlinks are rejected.

All replaceable source and outputs live under `PREFIX/releases`; `PREFIX/active`
is the atomic activation link. A durable `transaction.json` records the prior
and intended history, active release, release marker, and installer-owned HA
links before any of them changes. The next locked install, status, target
repair, or rollback completes a committed transition or restores a pre-commit
transition. `dry-run` never takes this repair path and does not mutate the
prefix, its absent parent, or targets; source checking uses an existing private
temporary area. Recovery validates its selected release, history shape, marker
state, and HA link plan before its first mutation. If the selected retained
payload is missing or corrupt, recovery leaves the current state and journal
intact for diagnosis. An uncommitted rollback may restore the recorded missing
HA link at the receipt's configured path; committed recovery still requires the
exact receipt-bound immutable-release link. `PREFIX/data` and `PREFIX/config`
are never removed or rolled back. Home Assistant requires `--ha-config`; Edge requires the
bootstrap to run on the selected Linux host with `--edge-target local-linux`.
An isolated Go installation can be bound with both `--edge-go-binary` and
`--edge-tool-root`; the executable must be canonical, non-symlinked, and below
that root. Its path and SHA-256 are part of the installation identity and
receipt. HA links point to one immutable release and are removed, moved, or
restored when component sets, config targets, or rollback selections change.
Only links whose target shape proves ownership by this prefix are changed; HA
registry and configuration files are not touched. The bootstrap prepares code
only and never starts services, registers telemetry, or handles Tesla
credentials.

Failed operations remove their staging tree, leave the previous active link in
place, and retain only a private `PREFIX/failures/.../failure.json` plus any
component build logs needed to diagnose the fixed command that failed.
