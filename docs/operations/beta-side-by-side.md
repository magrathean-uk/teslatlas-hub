# Beta side-by-side runbook (draft)

Draft status: written from the token-guard and collection-window design and the
encrypted-backup decisions. Windows, scheduled backups, restore and every command or
control marked "planned" are not yet implemented. Their names and behaviour are fixed
when that work lands. The final pass must follow M02-T05 and M06 and replace every
"planned" with the shipped form.

Execution prerequisite: no live collection window runs until the M02 guards (token
guard, window grant and automatic stop) are implemented, tested and independently
approved. Until then only the install and stop steps apply.

Import prerequisite: the credential-free import (section 4) is planned. The current
Mac import flow reads the other logger's source encryption key and stores it locally,
which this runbook forbids. Do not run any import or history collection step until
the M02 import guards and the matching Mac-app guards are implemented, tested and
independently approved.

This runbook is for the owner's Mac beta. The Hub runs beside another vehicle logger
that stays in service. That logger is never stopped, restarted, reconfigured or
re-logged-in by anything in this runbook. If a step would need that, stop and do not
proceed.

## Principles

- The Hub uses only its own Tesla login. It never reads, stores, uses or refreshes
  any credential held by another application.
- The Hub contacts Tesla only inside a collection window: a signed, owner-approved,
  time-limited grant for this host. Outside a window it sends nothing to Tesla.
- Between windows the Hub is meant to keep its history current by a read-only import
  (planned, section 4) from the other logger's database that never touches that
  logger's credentials. Until the guards land, no import runs.
- One collector at a time across the Mac and any guest.

## 1. Install

1. Install the ad-hoc signed beta app built for this Mac (or the macOS 13 guest).
2. Open the app. Opening it does not start the Hub or write its LaunchAgent. Complete
   account setup and diagnostics, then choose **Start Hub**: the app then writes and
   loads its per-user LaunchAgent after configuration preflight. Collection stays off
   until a window is open (section 3, planned). Any beta-specific startup behaviour
   is planned.
3. Confirm the Hub is healthy: the app shows ready, or `/healthz` and `/readyz` answer
   on loopback.

## 2. Own Tesla login

Sign-in is Tesla traffic, so it is possible only while a window is open (section 3,
planned; it needs the approved M02 guards).

1. Open a window.
2. In the app, choose sign-in. Complete the Tesla page in your own browser session.
3. The app hands the Hub only the one-use authorisation code. The Hub exchanges it
   itself and stores the resulting own credential.
4. Check the credential audit shows exactly one grant, the Hub's own.

If the app or Hub reports "re-login needed", the window has closed. Open a new
window and sign in again.

## 3. Collection windows

Full open, stop, rollback and restart steps: `docs/operations/collection-windows.md` (written by the window rollback task).
Summary:

- Planned behaviour: each window needs a grant approved by the owner, naming this host, a start and a
  hard expiry within the code ceiling. Extension needs a new grant.
- Open (planned `window open --grant FILE`) and check (planned `window status`).
- Planned: the Hub stops collecting at expiry even if nobody is present, and returns to
  read-only import with a recorded handback.

Rollback, at any time or on any doubt:

1. Stop the Hub's collection (planned `window close`). Quitting the app does not stop
   the background Hub: choose **Stop Hub…** in the app, or unload the LaunchAgent, and
   confirm collection has ended (the app shows stopped and no Tesla traffic follows).
2. Revoke the Hub's own grant in the owner's Tesla account security settings.
3. Confirm the other logger's positions are still advancing.
4. Do not resume until the cause is understood; a new window needs a new grant.

## 4. Read-only import (planned)

Not to be executed until the M02 import guards and Mac-app guards are implemented,
tested and independently approved. The steps describe the intended behaviour.

1. Close or confirm no window is open; import is refused during a window.
2. In the app, point the import at the other logger's database with a read-only
   database role.
3. Run the import. Planned: credentials in the source are not read, and any request
   to do so is refused.
4. Check the import report: history is current up to the latest source position.

Repeat between windows to keep history current.

On a Hub that is already signed in and has collected in windows, the Mac app runs this
import from **File > Import History…**. It uses the history-only path: the source must be
a PostgreSQL database on this Mac (127.0.0.1 or ::1), read with a password file. The Hub's
own `config.toml` is never edited: the import runs against a private history-only copy
of it (collection interval 0, Legacy login path off, no Fleet Telemetry). The app checks
that copy before it touches the Hub, and refuses a configuration that writes collector
settings in a form it cannot rewrite safely (dotted keys or inline tables). It then pauses
the Hub (it stays enabled at login), imports, and starts the Hub again if it was running,
once the importer can no longer write. The import counts as finished only when the Hub's
process is confirmed serving its configuration again (`teslatlas-hub serving-check`,
which works between windows and never contacts Tesla). A Hub that answers but reports a
fault (credential store, catalogue, quarantine or history it cannot serve) is not
confirmed, and the app shows the reason. The data the Hub collected in its
windows is kept; history from before it is added beside it. Phones already paired
download the car's history again once.

- If the import fails, the Hub is still started again if it was running, and the app
  says so. If the restart also fails, the app shows both errors and asks you to choose
  **Start Hub**.
- If the app quits or the Mac loses power during the import, the app finishes the job
  at its next launch: it removes the private copy, starts the Hub if it was running and
  confirms it serving. Until that, or a confirmed **Start Hub**, a new import is
  refused. A successful **Stop Hub** cancels that pending start, so the Hub stays
  stopped at the next launch. A recovery record the app cannot trust starts nothing:
  the app reports it and leaves the Hub as it is.

## 5. Backups and restore

Planned behaviour (M06; not yet implemented): backups are encrypted, written to a
folder you choose, and kept as the last few generations.

- Back up now: use the app's "Back up now" (planned). Scheduled backups (planned)
  follow the schedule in settings.
- Keep the recovery password separately from the backup folder. Without it a backup
  cannot be restored.
- Backups exclude Tesla refresh tokens and virtual-key private keys, so a restore
  needs a fresh own login (new window) and re-pairing of devices.
- Restore (planned) on a replacement host from the bundle and password only. The restore
  regenerates the TLS identity, cursor key and bearers; access revoked, before or
  after the backup was taken, stays revoked: restoring an older backup never revives
  a later revocation.
- A failed or interrupted backup keeps the last good generation. A wrong password or
  a corrupt bundle is refused without changing the Hub.

## 6. Pair a phone

Pairing gives the Tesatlas App on a phone read access to this Hub over the local
network. The Hub needs a LAN TLS identity for that; the Mac app sets it up the first
time.

1. Start the Hub and wait until the app shows it running.
2. Open **Service Details** and choose **Pair a phone…** under Paired devices.
3. The app runs `teslatlas-hub lan-tls --host <name>.local` with this Mac's local
   hostname (System Settings, General, Sharing). The first time, it makes a
   self-signed P-256 certificate valid for 397 days in the Hub data folder's `tls/`
   (folder 0700, files 0600, named after the run), sets `bind = "0.0.0.0:8443"` and a
   `[tls]` table with `https://<name>.local:8443/`, and restarts the Hub. With `[tls]`
   already set it changes nothing. It refuses a configuration file the Hub itself would
   refuse, and reports a change made to that file meanwhile instead of overwriting it;
   a setup interrupted before it finished is cleaned up by the next one. Setups never
   overlap: each holds a lock beside the configuration file (`.config-writer.lock`)
   from its first read to publication, and a second waits up to 30 seconds for it.
   Before asking for a link the app confirms that the running Hub serves the configured
   identity (`teslatlas-hub serving-check`). If it still serves an older configuration,
   for example after a restart that failed, the app restarts it first.
4. The app shows a one-time pairing link with **Copy** and the time it expires
   (15 minutes). Copy it to the phone, for example with Universal Clipboard, and paste
   it in Tesatlas. The App pins the certificate named in the link.
5. Choose **Done**. The new phone appears under Paired devices once it has claimed the
   link. An unused link stops working at its expiry; choose **Pair a phone…** again
   for a new one.

The link is a single-use secret: the app never logs or stores it, and it is not
shown again after **Done**. From a terminal, `teslatlas-hub control pair` prints a
new link the same way, under "Paste this link in Tesatlas".

The phone and the Mac must be on the same local network, and the Mac firewall must
allow incoming connections to the Hub. To undo, stop the Hub, restore the previous
`config.toml` (its `bind` and no `[tls]`) and start the Hub again; revoke paired
phones under Paired devices.

## 7. Stopping or removing the Hub

Stop: choose **Stop Hub…** in the app (or unload its LaunchAgent) and confirm
collection has ended. Quitting the app alone leaves the background Hub running. The
other logger is unaffected.

Remove:

1. Close any window and take a final backup (section 5).
2. Revoke the Hub's own grant in the Tesla account security settings.
3. Choose **Stop Hub…**, confirm collection has ended, then quit the app and unload
   its LaunchAgent.
4. Move the app and its data folder to the Trash. Keep the backup bundle and
   recovery password until you are sure you no longer need them.
5. Confirm the other logger still collects.
