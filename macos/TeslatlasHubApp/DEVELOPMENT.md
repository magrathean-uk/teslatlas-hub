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
TESLATLAS_HUB_DEVELOPMENT_CONTROL_DIRECTORY=/Users/owner/dev/runtime
TESLATLAS_HUB_DEVELOPMENT_MODE=standalone
```

`TESLATLAS_HUB_DEVELOPMENT_MODE` selects one exact source-run contract:

- `standalone` runs an empty or import-only Hub with every collector disabled.
- `edge` runs only the explicitly configured local `collector.edge` composition.
- `fixture` retains the historical seeded-fixture checks. It is the compatibility
  default when the mode variable is absent, but it is not the ordinary fresh setup.
- `private-lan` admits an explicitly selected RFC1918 IPv4 listener with strict
  TLS and an exact matching HTTPS public URL. It can serve an existing valid
  Hub store with collection disabled, or use the normal native Legacy provider
  preflight when that provider is enabled. It does not adopt Edge or Fleet
  Telemetry ingress.

The original three modes require a literal loopback listener, matching HTTPS public URL, and
a valid owner-controlled TLS identity. Standalone rejects any collector; Edge rejects
periodic/Fleet Telemetry collection and passes the normal production Edge binding
preflight. No mode bypasses configuration, data ownership, TLS, or credential checks.

Create the config parent, state, and log directories as the current user. State and
log directories must use mode `0700`. If the config already exists it must be a
regular owner-only file such as mode `0600`. The binary must be a regular executable
owned by the current user and must not be writable by group or others. None of the
paths or their existing components may be symbolic links or world-writable.
Private leaves remain owner-only. Executable and configuration paths require every
ancestor to disallow group and world writes, because launchd later resolves these
absolute paths. On this Mac use private runtime executable/configuration copies
under the existing `~/dev/runtime`, following the accepted source-run runtime pattern;
the original build artifacts and data remain external. State and journal ancestors
may be group-writable only when their UID and primary GID are the current owner's.

Set these variables in the Xcode scheme, or run the built app executable directly
from the configured shell. Do not rely on Finder to inherit shell variables.

On **Start Hub**, the app writes a mode-0600 LaunchAgent property list inside the
existing mode-0700 control directory (`~/dev/runtime` by default). That directory
and all ancestors must have no group or world write permission; it is opened through
descriptor-anchored `openat` calls without following symlinks. The directory is not
created automatically. This keeps launchd control files away from the external
volume's group-writable ancestors. The runtime executable/configuration copies are
private internal files; state and journals remain at their selected external locations.
Its label is distinct from `com.teslatlas.hub`, and it runs exactly:

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
and config. The diagnostics view reports the development label and configured
locations. New development services record the typed journals described below;
existing `hub.out.log` and `hub.err.log` remain historical evidence.

The development LaunchAgent passes the selected mode and private log directory to
the Hub. The new source-run journal uses closed typed JSONL events in
`hub-events.0.jsonl` and the AppKit controller uses `appkit-events.0.jsonl`.
Events contain UTC milliseconds, generated process/session/request or operation
correlation, static endpoint/action labels, status, numeric heads/counts and duration.
They never copy headers, raw URIs/queries, command arguments/output, error text,
tokens, cursors, manifests, source/vehicle identifiers or location values.
CLI stdout JSON remains the command protocol and is not copied into the journal.

Each journal uses a 64-event asynchronous queue, 0600 files in the validated 0700
log directory, ten-MiB segments, at most five segments and a seven-day age limit
applied on the next write. Rotation closes the current file before renaming under
an interprocess lock. Only its exact journal names are pruned; old logs and receipts
are preserved. The two journals have separate bounds. Queue/write losses appear in
`dropped_total` on later successful events; CLI final drain is bounded to 500ms and
AppKit termination drains each journal for at most 250ms. Abrupt termination can
lose queued events and an idle journal is not age-pruned until its next write.
Both journal writers open each directory component relative to a held descriptor
with `O_DIRECTORY | O_NOFOLLOW`, then validate that descriptor before proceeding.
A group-writable ancestor cannot redirect journal writes through a substituted symlink.

For newly controlled development services, launchd stdout/stderr go to `/dev/null`:
the typed journal replaces unrestricted debug output and avoids unbounded open
stdio files. Existing `hub.out.log`/`hub.err.log` are preserved as prior evidence.
Raw panic/uncaught diagnostic text is intentionally unavailable in these new runs;
missing completion and the controller's typed exit/readiness events identify that
limit. Production service behaviour and its logging configuration are unchanged.

The app revalidates the paths before each command, Start, Restart, or status action
and rejects a missing, replaced, symlinked, misowned, or over-permissive path. Stop
is the deliberate recovery exception: it validates the current UID, normalized
configured paths, and the derived development-only label, then boots out that exact
label even when a runtime path has been removed or damaged. Production package
installation, update, uninstall, signing, and trust checks are not reused or weakened;
those controls are unavailable in development mode.

## Optional read-only TeslaMate current telemetry

The producer is disabled unless this complete table is present in the protected Hub
configuration. It operates independently of the Legacy/Fleet collector cadence:

```toml
[teslamate.current]
source_url = "postgresql://reader@127.0.0.1:PORT/DATABASE"
source_key = "EXISTING_TESLAMATE_SOURCE_KEY"
vehicle_id = "EXISTING_HUB_VEHICLE_UUID"
car_id = 1
password_file = "/Users/owner/dev/creds/teslatlas/current-postgres-password"
```

The endpoint cannot contain a password. The password-only file must be a current-user
regular single-link file with mode `0600` in a validated mode-`0700` directory; it is
read through descriptor-anchored no-follow opens and held in zeroizing storage.
Literal loopback retains the existing local PostgreSQL transport; other endpoints
require the existing certificate-validated TLS path. A private bridge, if used, is
an owner-configured runtime dependency, never created by the producer.

Each attempt opens a read-only repeatable-read transaction, applies a five-second
statement timeout and selects only the latest position for the configured car.
No coordinates, charge/state timestamps, tokens, or Tesla commands are read or sent.
The returned EID/VIN must resolve to the existing TeslaMate source key and exact active
Hub vehicle alias. This path never registers or remaps vehicles.

Battery, ideal/estimated/rated range and odometer come from that single position.
Each field is independently validated; unusable fields are omitted, and at least
one usable supported field is required. Ranges and odometer remain in kilometres.
The source position timestamp is preserved; rows older than five minutes, future
rows and source/Hub clock skew over two minutes are rejected. Import/poll time is
never substituted. Newer TeslaMate telemetry uses a sparse summary so older provider
fields do not inherit its timestamp. Existing current endpoint authorization and
App freshness checks remain in place.

The producer polls after 30 seconds on success, bounds each attempt to 15 seconds,
and backs off failures to at most five minutes. Failures preserve the current cache;
unchanged source rows do not advance timestamps or allocate repeated cache records.
Schema 67 adds truthful internal `teslamate_position_v1` provenance and preserves
existing current records. It does not publish a new signed history checkpoint.
