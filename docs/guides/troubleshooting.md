# Troubleshooting

Start with the message shown by Hub. Preserve your data and configuration;
uninstalling, deleting a database or repeatedly reconnecting an account is not
a first diagnostic step. Review logs before sharing them.

## An older Mac app opens after installation

Quit the control app, then open **Teslatlas Hub.app** directly from
`/Applications` in Finder. Do not open a copy from Downloads, a development
build folder or an old Dock shortcut. Check **About Teslatlas Hub** for the app
version and **Service Details** for service information. Opening the intended
app allows its per-user agent to be reconciled with the embedded Hub.

If the service is older, follow [Upgrade and rollback](../releases/upgrade.md)
and replace the app in `/Applications` or `~/Applications`. Keep your data
folder; Start validates the configuration before loading the current agent.

## Quitting the app does not stop collection

This is intentional: the background service is separate from the control app.
Use **Stop Hub…** to pause collection. Command-Q exits the control app; an
active protected import/setup operation may require completion first. Read
the displayed message and allow the operation to finish. Avoid force-quitting
an import as a routine workaround.

## SSH authentication or key selection fails

Check the server, SSH port and user together. With **SSH key** selected, use
**Choose Key…** to select the private key, not its `.pub` file. The file must
be readable by your Mac account. In the native chooser, Command-Shift-G lets
you enter the key's directory if it is hidden. Leaving the field empty uses
the SSH agent or default keys; it does not guarantee a usable key is available.

Keep any passphrase or password private. Read the specific error rather than
changing server permissions indiscriminately. See
[Mac migration setup](install-macos.md#connect-or-migrate).

## Migration connects but cannot access Docker

SSH authentication succeeded, but the remote account cannot access the
deployment. If that account normally uses sudo for Docker, select **This user
needs sudo to read the source database** and retry. The importer requires
the supported non-interactive access path; an interactive sudo password prompt
may not satisfy it. Ask the server administrator to verify the account's access
if the error persists. Do not make the Docker socket world-writable.

## Setup passes but new data is missing

Confirm that the service is running, the expected vehicle is configured and
diagnostics are current. Check logs for authentication, connectivity or provider
errors. An asleep vehicle or a vehicle without connectivity may legitimately
have no new samples. Do not wake it merely to prove the dashboard works.
Imported history is not evidence of fresh collection. See the stream and
Fleet checks below before changing credentials.

## Hub will not start

Run the read-only checks first:

```sh
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml doctor
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml preflight
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml status
```

Those are Debian package commands. On macOS, use the control app or run its
embedded binary as the signed-in user from `/Applications` or `~/Applications`; see
[CLI reference](cli.md#platform-invocation).

Check absolute paths, private-file ownership and mode, loopback/TLS binding,
database admission, and whether another Hub process holds the user lock.

## Driving data stops

Check vehicle connectivity before changing Hub. A car without cellular or Wi-Fi
signal cannot deliver live data. Legacy streaming reconnects with bounded
recovery; Fleet Telemetry resumes when the vehicle and public receiver can
communicate. A genuine no-signal interval may remain a route gap.

Confirm only one service owns the provider token pair. Never run the source application and
Hub concurrently with the same legacy refresh token.

## Service stopped but a port looks open

Check the owning process instead of assuming it is Hub:

```sh
sudo lsof -nP -iTCP:8080 -sTCP:LISTEN
sudo /usr/bin/teslatlas-hub service status
```

The second command is for Debian. On macOS, check service state in the control
app and use its embedded binary for data commands as described in the CLI reference.

A normal Hub stop closes collection, streaming, supervised children, active
connections after the bounded grace period, and the listener.

## Fleet commands fail

Confirm Fleet provider mode, virtual-key registration, the loopback command
proxy, its CA path, and command-proxy service state. Wake uses the direct Fleet
endpoint; other signed commands require the proxy.

## Source application import fails

Run `source-check`. The running app must be the source application at a supported release, and
Hub admits only an exact recognised schema set. A newer or
modified schema is rejected rather than guessed.
Passwords must not appear in the PostgreSQL URL.

## Diagnostic sharing

Use Command-L on macOS or bounded journald output on Debian. Review redaction
before sharing. Do not publish tokens, pairing payloads, VINs, coordinates,
private addresses, production databases, or immutable packs.

For reproducible non-security defects, use the GitHub bug template. Report
vulnerabilities privately under the [security policy](../../.github/SECURITY.md).
