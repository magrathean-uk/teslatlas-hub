# CLI reference

The authoritative command list is emitted by the exact binary:

```sh
tesatlas-hub --help
tesatlas-hub COMMAND --help
```

## Platform invocation

`tesatlas-hub` in the reference tables is shorthand, not a promise that every
package adds the binary to `PATH`. Use the installed platform form:

| Platform | Data/config commands | Service commands |
|---|---|---|
| Debian package | `sudo -u tesatlas -- /usr/bin/tesatlas-hub` | `sudo /usr/bin/tesatlas-hub` |
| macOS app | `"/Applications/Tesatlas Hub.app/Contents/Resources/tesatlas-hub"` as the signed-in user; use `~/Applications` if installed there | Start, Stop and Restart in the app |
| Source checkout | `./target/release/tesatlas-hub` | Depends on the operator's supervisor |

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
| `service status\|start\|stop\|restart` | Control the installed Linux service; Linux only. |

On macOS, `control sign-out` disables the per-user agent, stops Hub and removes
the persisted Tesla account credentials. It stays stopped across logins until
**Start Hub** in the app enables it again.

## Health and maintenance

| Command | Purpose |
|---|---|
| `status` | Print redacted local state: service readiness (`ready`, `readinessReason`), the one `collectionAttention`, healthy `readinessDetail`, `collectorRunning`, the redacted lease timing and, only while a cadence rule is broken, `collectorCadenceError`. Current location labels are omitted. |
| `doctor` | Check database, credentials, TLS, and collector readiness. Collection attention alone does not fail it; an independent integrity, credential, unresolved Legacy refresh, TLS, stale-lease or stream-recovery fault does. |
| `collector retry\|resume <UUID>\|gaps acknowledge` | Ask the running collector to retry after a refusal, clear one car's collector pause, or mark alert gaps as seen. They record a request the collector applies within seconds; they are not `control pause` and `control resume`. Output shapes: [Collector](configuration.md#when-collection-stops-or-a-car-is-held). |
| `preflight` | Confirm one configured vehicle can be served. |
| `repair` | Validate integrity, remove orphaned packs and clear ended rotation-grace successors. |
| `observe` | Observe one admitted vehicle for a bounded duration. It refuses at once, with the cadence message, when the collector cadence breaks a rule; unlike `serve` it never waits. |

## Data and recovery

| Command | Purpose |
|---|---|
| `backup` | Create a data-only backup generation. |
| `verify-backup` | Verify one immutable backup without mutation. |
| `restore-data` | Restore data into a new directory without credentials. |
| `export-recovery-credentials` | Export keys into a separately encrypted recovery file. |
| `restore-recovery-credentials` | Restore that key export into a matching data restore. |
| `pair` | Create a short-lived, single-use device pairing invitation (stop the Hub first; use `control pair` while it runs). `--vehicle UUID` (repeatable) grants only those vehicles, `--all-vehicles` or no flag grants every vehicle active when the invitation is claimed. |

## Migration and interoperability

| Command | Purpose |
|---|---|
| `source-check` | Read-only source application compatibility and inventory check. |
| `migrate` | Read-only source history and credential transfer. |
| `observation-watermark` | Capture a durable cutover watermark. |
| `verify-observation` | Prove a newer observation was committed. |

Import checks read the source schema and report whether this Hub recognises it.
The schema cannot establish the release of the running source application. After
checking the supported-release guidance, pass `--accept-schema-evidence` to
acknowledge that limit; `migrate` requires the flag. The earlier schema flag is
deprecated, remains a hidden alias, emits one warning
on stderr and can be removed in a later version. The earlier verb `teslamate-check`
is handled the same way: it still runs `source-check`, is absent from help, and
warns once on stderr. Both aliases are on the removal list for the next release.
They only map an earlier name to a command that exists; inputs that were removed
on purpose, such as source tokens, stay refused.

The import kind is `source_db`; a first keyless import uses `default`. On an
upgraded catalogue, reuse its stored key with `--source-key` or configure
`import_source.source_key`. A different key is refused unless
`--new-import-source` explicitly permits a second import source. `doctor` reports
an unmatched current-source key; its current-state worker stops while Hub keeps
serving. Old command/configuration names are refused with replacement guidance.

## Vehicle control

`control` manages collection settings, paired devices, geofences, GPX export,
and explicit vehicle actions.

### Vehicle actions

Three actions are sent: `wake`, `flash-lights` and `honk-horn`. Only the
command line sends them; the Mac app shows none. Each needs `--confirm`, and
with more than one car, `--vehicle-id UUID`. The other actions the command line
parses (`climate-start`, `climate-stop`, `charge-start`, `charge-stop`,
`set-charge-limit --percent N`, `lock` and `unlock`) are always refused, by the
command line and again by the running service, and never sent.

```sh
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml control \
  --vehicle-id 11111111-1111-4111-8111-111111111111 wake --confirm
```

An action is sent only by a Legacy Hub whose running service is collecting
inside an open collection window. The command hands it to that service, which
checks the window again, records it in the outbound audit as an explicit vehicle
command (a wake as a direct wake command) and sends it once through the Owner
route; it is never repeated. On success the command prints the service's report
as one JSON line; a wake's `state` is one of `online`, `asleep`, `offline` or
`waking`, or absent. A refusal prints nothing on standard output, exits 1 and
writes one of these lines to standard error, and nothing is sent:

| Line | When |
| --- | --- |
| `vehicle_commands_not_offered: ...` | The Hub is configured for Fleet. Fleet is polling only, with or without `--confirm`. |
| `vehicle_action_not_permitted: ...` | The action is not `wake`, `flash-lights` or `honk-horn`, with or without `--confirm`. |
| `vehicle_action_unconfirmed: ...` | `--confirm` is missing. |
| `collection_window_closed: ...` | No running Hub is collecting inside an open window, or its window has closed. |
| `vehicle_action_unknown: ...` | The running Hub does not know the action. |

An action name the command line does not know is a usage error (exit 2). When
Tesla explicitly rejects the command (a 4xx answer other than 408 or 429, or a
refusal in the answer), the line says the provider rejected it. When the action
was sent but its answer was lost, timed out, could not be read, or came back as a
server or gateway failure (5xx, 408, 429), the command exits 1
with `vehicle command outcome is unknown: it may have been sent (audit receipt
N); do not repeat it`: the car may have acted, and the audit receipt records the
failure rather than a rejection. Check the car
before repeating anything.

Do not run real vehicle commands during testing. Charging, climate, lock, wake,
light, and horn actions affect physical property and third-party services.

### Paired devices and their vehicles

Every paired device reads only the vehicles it is granted. `control pair`
creates an invitation while the Hub is serving (it takes short SQLite
transactions and no instance lock, like `paired-devices` and `revoke-device`),
and `control paired-devices` lists each device with its granted vehicle ids.

```sh
# One car only; repeat --vehicle for more. Unknown or retired ids are refused.
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml control pair \
  --vehicle 11111111-1111-4111-8111-111111111111
# Every vehicle that is active when the invitation is claimed (the default).
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml control pair --all-vehicles
```

The control-level `--vehicle-id` picks the car of a vehicle action and does not
apply to `control pair`: the command refuses it and names `control pair --vehicle`
as the way to scope an invitation. The terminal output names the scope (names
from the provider have control and invisible formatting characters replaced
with `?`). The invitation link and QR never carry
it, and a vehicle added to the Hub later is never granted to an existing
device. A device with no grant sees no vehicle: every vehicle route answers it
as it answers an unknown vehicle, and `GET /v1/vehicles` is empty.

## Legal and source

```sh
tesatlas-hub legal
tesatlas-hub licence
tesatlas-hub source
```

These commands identify the exact version, licence, notices, and source route
for the running binary. The v1.0.0 build identifies its immutable tagged source;
development builds identify the public repository.
