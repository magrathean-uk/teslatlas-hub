# Troubleshooting

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

Those are Debian package commands. On macOS, use the control app or run the
packaged binary as the signed-in user; see
[CLI reference](CLI.md#platform-invocation).

Check absolute paths, private-file ownership and mode, loopback/TLS binding,
database admission, and whether another Hub process holds the user lock.

## Driving data stops

Check vehicle connectivity before changing Hub. A car without cellular or Wi-Fi
signal cannot deliver live data. Legacy streaming reconnects with bounded
recovery; Fleet Telemetry resumes when the vehicle and public receiver can
communicate. A genuine no-signal interval may remain a route gap.

Confirm only one service owns the provider token pair. Never run TeslaMate and
Hub concurrently with the same legacy refresh token.

## Service stopped but a port looks open

Check the owning process instead of assuming it is Hub:

```sh
sudo lsof -nP -iTCP:8080 -sTCP:LISTEN
sudo /usr/bin/teslatlas-hub service status
```

The second command is for Debian. On macOS, check service state in the control
app or use the packaged binary path in the CLI reference.

A normal Hub stop closes collection, streaming, supervised children, active
connections after the bounded grace period, and the listener.

## Fleet commands fail

Confirm Fleet provider mode, virtual-key registration, the loopback command
proxy, its CA path, and command-proxy service state. Wake uses the direct Fleet
endpoint; other signed commands require the proxy.

## TeslaMate import fails

Run `teslamate-check`. Hub admits only the exact validated TeslaMate 4.1.1
migration set. A newer or modified schema is rejected rather than guessed.
Passwords must not appear in the PostgreSQL URL.

## Diagnostic sharing

Use Command-L on macOS or bounded journald output on Debian. Review redaction
before sharing. Do not publish tokens, pairing payloads, VINs, coordinates,
private addresses, production databases, or immutable packs.

For reproducible non-security defects, use the GitHub bug template. Report
vulnerabilities privately under [SECURITY.md](../SECURITY.md).
