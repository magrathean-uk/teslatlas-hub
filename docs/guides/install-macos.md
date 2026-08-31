# Install on macOS

Teslatlas Hub v1.0.0-beta.2 supports Apple-silicon Macs running macOS 13 or
later.

## Verify the download

No binary release is published yet. The installation instructions below apply
only after the complete signed prerelease is published and passes
[release verification](../releases/verification.md).

## Install

1. Expand `Teslatlas Hub.zip`.
2. Move **Teslatlas Hub.app** to `/Applications`.
3. Open `/Applications/Teslatlas Hub.app`.
4. Choose Fleet API, legacy login, or TeslaMate migration.
5. Complete account setup and diagnostics.
6. Approve the embedded privileged service installation when macOS asks.
7. Confirm the dashboard reports **Running** and **Ready**.

The app installs its embedded, independently signed service package at
`/Library/Application Support/Teslatlas Hub`. A per-user LaunchAgent owns the
running Hub service for the signed-in user. The separately downloadable
`TeslatlasHubService.pkg` installs only this service payload; it does not install
or move **Teslatlas Hub.app**.

The app keeps Hub stopped when setup, version admission, package verification,
or diagnostics fail. It never silently starts an unconfigured collector.

## Service controls

Use the app, or run the installed binary by its absolute path. The package does
not add a shell command to `PATH`:

```sh
HUB='/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub'
CONFIG="$HOME/Library/Application Support/Teslatlas Hub/config.toml"
"$HUB" service status
"$HUB" --config "$CONFIG" service start
"$HUB" --config "$CONFIG" service restart
"$HUB" service stop
```

Stopping Hub pauses collection and closes streaming, HTTP, command-proxy, and
supervised companion connections. Stored history remains intact.

## Logs and diagnostics

Press **Command-L** in the app to open bounded, redacted app and service logs.
**Run Diagnostics** performs `doctor`, `preflight`, `status`, database,
credential, connection, and recent-log checks.

Copy and Save use the same redaction pass. Review every report before sharing
it; precise telemetry is sensitive even when ordinary identifiers are removed.

## Uninstall

Open **Service Details → Uninstall Hub…**. The default uninstall removes the
current user's LaunchAgent, service payload, and logs but preserves the Hub
database and configuration. Permanent data deletion is a separate confirmed
choice.

The uninstaller refuses to remove a shared service payload while another local
user still has a Hub LaunchAgent.
