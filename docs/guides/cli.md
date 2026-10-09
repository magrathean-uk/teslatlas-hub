# CLI reference

The authoritative command list is emitted by the exact binary:

```sh
teslatlas-hub --help
teslatlas-hub COMMAND --help
```

## Platform invocation

`teslatlas-hub` in the reference tables is shorthand, not a promise that every
package adds the binary to `PATH`. Use the installed platform form:

| Platform | Data/config commands | Service commands |
|---|---|---|
| Debian package | `sudo -u teslatlas -- /usr/bin/teslatlas-hub` | `sudo /usr/bin/teslatlas-hub` |
| macOS package | `"/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"` as the signed-in user | Same absolute path, or the app |
| Source checkout | `./target/release/teslatlas-hub` | Depends on the operator's supervisor |

Pass `--config ABSOLUTE_PATH` before or after the command. Commands that need
credentials accept private files or bounded standard input; secrets must never
appear in process arguments.

## Setup and service

| Command | Purpose |
|---|---|
| `init` | Initialize or migrate the local Hub database. |
| `bootstrap` | Create the configured packaged Linux store. |
| `setup` | Configure legacy credentials for one or every vehicle. |
| `setup-fleet` | Configure Fleet credentials from bounded JSON on standard input. |
| `configure-fleet-telemetry` | Install the fixed Fleet Telemetry policy on configured vehicles. |
| `serve` | Run the HTTP service in the foreground. |
| `service status\|start\|stop\|restart` | Control the installed service. |
| `install` | Install the minimal per-user macOS LaunchAgent. |

## Health and maintenance

| Command | Purpose |
|---|---|
| `status` | Print redacted local state. |
| `doctor` | Check database, credentials, TLS, and collector readiness. |
| `preflight` | Confirm one configured vehicle can be served. |
| `repair` | Validate integrity and remove orphaned packs. |
| `observe` | Observe one admitted vehicle for a bounded duration. |

## Data and recovery

| Command | Purpose |
|---|---|
| `backup` | Create a data-only backup generation. |
| `verify-backup` | Verify one immutable backup without mutation. |
| `restore-data` | Restore data into a new directory without credentials. |
| `export-recovery-credentials` | Export keys into a separately encrypted recovery file. |
| `restore-recovery-credentials` | Restore that key export into a matching data restore. |
| `pair` | Create a short-lived, single-use device pairing invitation. |

## Migration and interoperability

| Command | Purpose |
|---|---|
| `teslamate-check` | Read-only TeslaMate compatibility and inventory check. |
| `migrate` | Read-only TeslaMate history and credential transfer. |
| `write-back charge-cost` | Dry-run or explicitly apply one allow-listed charge-cost update. |
| `observation-watermark` | Capture a durable cutover watermark. |
| `verify-observation` | Prove a newer observation was committed. |

TeslaMate migration requires a running TeslaMate 4.2.0 or newer and the exact
reviewed v4.2-compatible database schema. Because the database cannot prove the
app version, `teslamate-check` reports confirmation required until the operator
passes `--acknowledge-v4-2-compatible-schema`; `migrate` always requires that
flag.

## Vehicle control

`control` manages collection settings, paired devices, geofences, GPX export,
and explicit vehicle actions. Vehicle actions require `--confirm`; multiple
vehicles also require `--vehicle-id UUID`.

```sh
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml control \
  --vehicle-id 11111111-1111-4111-8111-111111111111 wake --confirm
```

Do not run real vehicle commands during testing. Charging, climate, lock, wake,
light, and horn actions affect physical property and third-party services.

## Legal and source

```sh
teslatlas-hub legal
teslatlas-hub licence
teslatlas-hub source
```

These commands identify the exact version, licence, notices, and source route
for the running binary. The v1.0.0 build identifies its immutable tagged source;
development builds identify the public repository.
