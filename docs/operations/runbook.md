# Operate Tesatlas Hub

## Daily health

For a Debian package installation, run data and configuration commands as the
dedicated service account:

```sh
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml status
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml doctor
```

`status` is a bounded redacted summary. `doctor` validates database integrity,
credentials, TLS, collector readiness, and configuration. Neither command sends
a vehicle action.

Also inspect the Debian supervisor:

```sh
sudo systemctl status tesatlas-hub.service --no-pager
sudo journalctl -u tesatlas-hub -n 200 --no-pager
```

On macOS, press **Command-L** for the bounded combined log and diagnostics view.
CLI use must run as the signed-in user with the absolute app-embedded binary path;
see [CLI reference](../guides/cli.md#platform-invocation).

## Start, stop, and restart

On Debian, service-management commands require administrator authority:

```sh
sudo /usr/bin/tesatlas-hub service status
sudo /usr/bin/tesatlas-hub service start
sudo /usr/bin/tesatlas-hub service stop
sudo /usr/bin/tesatlas-hub service restart
```

On macOS, use **Start Hub**, **Stop Hub** and **Restart Hub** in the app. Stop
persists across logins until Start Hub enables the agent again. Data commands
use the app-embedded binary as the signed-in user; see
[CLI reference](../guides/cli.md#platform-invocation).

An intentional stop is a stable stopped state. It stops new collection, closes
Tesla streaming, terminates supervised command-proxy and receiver connections,
stops the HTTP listener, and waits only for the bounded shutdown grace period.
It does not delete history.

On Debian, the Hub unit conditionally pulls in both optional sidecar units.
`PartOf` propagates explicit stop/restart operations. Unexpected exits restart
Hub directly without dropping healthy companions; after restart exhaustion, a
short-lived failure target conflicts with both companions and stops them. The
command proxy remains independently startable while Hub is stopped for Fleet
enrolment. The single Hub service command is otherwise the lifecycle control
for all network-owning processes.

## Upgrade

On macOS, close the collection window and wait for `window status` to report
`closed`, then export recovery credentials through the running Hub. Use
**Stop Hub** in the app, create and verify an offline data backup, then quit
the app. Retain both recovery artefacts privately; see
[Backup and recovery](backup-and-recovery.md). Replace
**Tesatlas Hub.app** in `/Applications` or `~/Applications` and open the new
copy. Run diagnostics before choosing **Start Hub**. The app replaces an older
per-user agent after preflight; see [Upgrade and rollback](../releases/upgrade.md)
for installer-era administrator cleanup.

For a Debian package installation:

1. Close the collection window, wait for `window status` to report `closed`,
   and export recovery credentials through the running Hub.
2. Verify the new source tag, exact version, notices, and locally built package.
3. Stop Hub, then create, verify and retain an offline data backup and the
   separately exported recovery credentials.
4. Install the new package.
5. Run `doctor` and `status` before resuming collection.

Package admission fails before mutation where possible. Once a forward database
migration begins, the new service remains stopped on failure instead of
restoring an older, schema-incompatible binary.

## Repair

For a Debian package installation:

```sh
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml repair
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml doctor
```

Repair validates the catalogue and packs, reports quarantined sessions,
removes orphaned packs and clears the sealed successors of ended bearer-rotation
grace windows. Back up before repair and preserve diagnostic output.

## Vehicle controls

Commands require explicit confirmation and are never health probes. Confirm the
selected vehicle before wake, climate, charging, lock, light, or horn actions.
Fleet actions require the local signed-command proxy; legacy UI command controls
remain unavailable.

## Incident response

If credentials or a paired bearer may be exposed:

1. stop Hub;
2. revoke the affected paired device or provider tokens;
3. preserve redacted logs and exact version information;
4. rotate credentials;
5. run `doctor` before restart;
6. report a product vulnerability privately under the [security policy](../../.github/SECURITY.md).
