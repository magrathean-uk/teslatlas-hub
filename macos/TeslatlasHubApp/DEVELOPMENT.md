# Source-run control app

The AppKit control app keeps its installed-service behavior by default. An unsigned
source build can instead control a source-built Hub only when all of these environment
variables are supplied and the opt-in value is exactly `1`:

```text
TESLATLAS_HUB_DEVELOPMENT=1
TESLATLAS_HUB_DEVELOPMENT_BINARY=/absolute/path/to/teslatlas-hub
TESLATLAS_HUB_DEVELOPMENT_CONFIG=/absolute/path/to/config.toml
TESLATLAS_HUB_DEVELOPMENT_STATE_DIRECTORY=/absolute/path/to/private-state
TESLATLAS_HUB_DEVELOPMENT_LOG_DIRECTORY=/absolute/path/to/private-logs
TESLATLAS_HUB_DEVELOPMENT_MODE=standalone
```

`TESLATLAS_HUB_DEVELOPMENT_MODE` selects one exact source-run contract:

- `standalone` runs an empty or import-only Hub with every collector disabled.
- `edge` runs only the explicitly configured local `collector.edge` composition.
- `fixture` retains the historical seeded-fixture checks. It is the compatibility
  default when the mode variable is absent, but it is not the ordinary fresh setup.

All three modes require a literal loopback listener, matching HTTPS public URL, and
a valid owner-controlled TLS identity. Standalone rejects any collector; Edge rejects
periodic/Fleet Telemetry collection and passes the normal production Edge binding
preflight. No mode bypasses configuration, data ownership, TLS, or credential checks.

Create the config parent, state, and log directories as the current user. State and
log directories must use mode `0700`. If the config already exists it must be a
regular owner-only file such as mode `0600`. The binary must be a regular executable
owned by the current user and must not be writable by group or others. None of the
paths or their existing components may be symbolic links or writable by group or
others.

Set these variables in the Xcode scheme, or run the built app executable directly
from the configured shell. Do not rely on Finder to inherit shell variables.

On **Start Hub**, the app writes an owner-only LaunchAgent property list inside the
state directory. Its label is distinct from `com.teslatlas.hub`, and it runs exactly:

```text
<development binary> --config <development config> serve
```

Before writing that property list or invoking launchd for Start or Restart, the app
runs the same binary's non-listening validation path:

```text
<development binary> --config <development config> serve-preflight --mode <selected mode>
```

This loads the exact configuration and calls the same source-run Serve admission
used by the long-lived process. A mode mismatch, unsafe listener/TLS identity, empty
fixture, or invalid Edge binding is returned directly to the app; no listener,
collector, property-list replacement, bootstrap, or kickstart occurs. Stop remains
available without preflight so a damaged or obsolete job can always be booted out.

The dashboard's Start, Stop, and Restart actions control only that development label.
Status, diagnostics, setup, migration, and account commands use the same source binary
and config. Hub stdout and stderr appear as `hub.out.log` and `hub.err.log` in the
specified log directory. The diagnostics view reports the development label and all
three owned locations.

The development LaunchAgent sets its Hub process to
the selected `TESLATLAS_HUB_DEVELOPMENT_MODE` and
`RUST_LOG=info,tower_http=debug`. The existing trace layer therefore writes the HTTP
method, URI, response status, and request latency to the owner-only `hub.out.log`;
`hub.err.log` remains available for process stderr. URIs can contain synthetic
identifiers; redact or hash identifier values before retaining or sharing evidence
from either log.

The app revalidates the paths before each command, Start, Restart, or status action
and rejects a missing, replaced, symlinked, misowned, or over-permissive path. Stop
is the deliberate recovery exception: it validates the current UID, normalized
configured paths, and the derived development-only label, then boots out that exact
label even when a runtime path has been removed or damaged. Production package
installation, update, uninstall, signing, and trust checks are not reused or weakened;
those controls are unavailable in development mode.
