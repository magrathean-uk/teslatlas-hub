# Getting started

Teslatlas Hub collects vehicle history on your own host. The native Mac app
controls the background service; the separately distributed Teslatlas client
connects to Hub to synchronise history.

## Choose your host

| Host | Interface | Local build |
|---|---|---|
| Apple-silicon Mac, macOS 13+ | Native app and CLI | Combined installer with Fleet companions |
| Debian 13, amd64 or arm64 | CLI and systemd | Core/Legacy package; no Fleet companions |

Start with [Mac setup and everyday use](install-macos.md) or
[Debian installation](install-debian.md), after [building from source](build-from-source.md).
No prebuilt releases are provided. Local Mac installers are unsigned and
unnotarised by default and may be blocked by macOS.

## Choose a setup path

- **New installation:** configure Fleet or Legacy credentials, run diagnostics
  and start collecting. You do not need TeslaMate.
- **Migrate from TeslaMate:** bring supported history from your own server.
  Read the [migration checklist](../releases/migration.md#before-you-start)
  before connecting. Keep the source and a backup until you verify the result.

| Connection | What you need | Important boundary |
|---|---|---|
| Legacy | Existing Owner API access/refresh tokens, or the explicitly selected migration credential path | Only one service may refresh the token pair |
| Fleet | A Tesla Developer application, account authorisation and configured Fleet companions | Not included in a core-only Debian build |

Follow [Fleet setup](fleet-setup.md) for the prerequisites. The Mac wizard does
not remove the developer-application and receiver requirements.

## Confirm collection

Complete setup and diagnostics before starting Hub. On Mac, check the
dashboard's service state, expected vehicles and recent activity. On Debian,
use the installed status and doctor commands in the installation guide.
Confirm fresh data appropriate to the vehicle's state; an asleep car does not
need to be woken merely to test setup.

If checks fail, use [Troubleshooting](troubleshooting.md). Do not run a second
collector or let TeslaMate and Hub refresh the same Legacy credentials.

## Pair your client

Pairing is separate from connecting Hub to Tesla. It authorises a Teslatlas
client to access this Hub's history.

1. Choose how the client will reach the host. Plaintext HTTP must remain on
   loopback. A client on another device requires a reachable Hub address,
   Hub TLS and paired-device authentication. Follow
   [Configuration](configuration.md) before exposing a listener.
2. Use the `pair` command from the [CLI reference](cli.md#platform-invocation)
   with the same configuration and operating-system user as the deployment.
   It creates a short-lived, single-use invitation. This is a CLI step, not a
   dashboard button.
3. Complete pairing in the separately distributed Teslatlas client using its
   connection flow. Treat the invitation as a secret; do not paste it into
   issues or screenshots. Create a new invitation if the old one expires.
4. Check that history synchronises. Revoke retired or unknown devices using
   Hub's device controls described in the CLI reference.

Do not publish the internal Fleet Telemetry ingestion route as a client endpoint.

## Keep a recovery copy

Before relying on the installation, create and verify a data backup, export
credentials into a separately encrypted recovery file, store them separately,
and test restoration into a new directory. A data backup alone does not restore
provider credentials. Follow [Backup and recovery](../operations/backup-and-recovery.md).

For routine use, see [Mac controls](install-macos.md#everyday-use) or the
[operations runbook](../operations/runbook.md). Before replacing installed
software, read [Upgrade and rollback](../releases/upgrade.md).
