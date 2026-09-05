# Install on macOS

Teslatlas Hub 2026.36.1 targets Apple-silicon Macs running macOS 13 or
later.

Download `TeslatlasHub-2026.36.1-arm64.pkg` from the matching release and
[verify it](../releases/verification.md). Read the
[release notes](../releases/release-notes-2026.36.1.md): the app is ad-hoc signed
and the combined installer is unsigned and unnotarised. Use the combined
installer for installation and upgrades; in-app service installation,
reinstallation, and update are unavailable in this distribution. The embedded
installer requires official-release metadata and Gatekeeper trust, which this
build does not provide. The unsigned installer can be blocked by macOS or
organisation policy. Do not disable system-wide security controls to install it.
For an existing installation, follow [Upgrade and rollback](../releases/upgrade.md).

Alternatively, build the combined installer from the matching source tag:

```sh
git checkout --detach v2026.36.1
./scripts/build-macos-app.sh
```

## Install

1. Open the verified `TeslatlasHub-2026.36.1-arm64.pkg` (or
   `dist/TeslatlasHub.pkg` for a local build).
2. Complete the macOS Installer flow. It installs **Teslatlas Hub.app** in
   `/Applications` and the service payload in
   `/Library/Application Support/Teslatlas Hub`.
3. Open `/Applications/Teslatlas Hub.app`.
4. Choose Fleet API, legacy login, or TeslaMate migration.
5. Complete account setup and diagnostics.
6. Confirm the dashboard reports **Running** and **Ready**.

The product installer includes both the app and a service-only component. The
app also carries `Contents/Resources/TeslatlasHubService.pkg`, but this
distribution cannot install that embedded package through the app. Use the
combined installer to install or update the service. A
per-user LaunchAgent owns the running Hub service for the signed-in user.

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
