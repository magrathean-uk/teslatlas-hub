# Collection windows

Hub sends Tesla traffic only on the host a signed grant names and only while
a collection window is open. Its login comes from a Hub sign-in or a migration
from the source application: it must be stopped before take-over and stay stopped,
and the Hub becomes the only program that refreshes that login. Outside a
window Hub sends nothing to Tesla (no sign-in exchange, vehicle discovery,
telemetry configuration or token refresh).

The separate `take-over-source-token` step reads the source database
read-only, with the source application stopped, and stores the login locally, without contacting Tesla. Run it with
the Hub service stopped, the window Closed and its handback recorded. The
first refresh waits for the next collection window. The history import must
run first and sets up the selected car; with no car, take-over refuses with
`history_import_required` and asks for the import first. No later car discovery
or new sign-in is needed for take-over. Follow the
[migration guide](../releases/migration.md) for the key, retries and returning
to the source application. After take-over, leave the source application stopped between windows too.
History-only imports remain read-only and do not read credentials.

This guide covers issuing a grant, trusting the issuer, issuer key custody,
opening, stopping, rolling back, restarting and removing refused credentials. `tesatlas-hub window --help`
lists the verbs.

`window status`, `window trust`, `window open`, `window close`, `setup`, the
credential-recovery commands, `purge-refused-credentials`, `backup-now` and
`backup-status` are forwarded to
the running Hub service, which
holds the window authority. With no service running they are refused with
`service_not_running` before they read any file, standard input or the
network. The issuer verbs (`window issuer-init`, `window issuer-trust`,
`window issue`, `window void`) and `window host-id` run on the issuer Mac
without a service.

## Migration and first refresh

The app discovers a source deployment that is already stopped. Its application
containers must not be running, dashboards and other database clients must be
disconnected, and automatic restart must not bring them back. Keep PostgreSQL
running. Closing a web page or pausing collection does not stop the application.
Keep the source stopped for as long as Hub owns that login.

A repeat take-over for the same login reports `already_taken_over` and writes
nothing; a different login in the slot is refused. After an app restart or a
lost connection, setup resumes take-over without importing again. Reconnect
and repeat the stop check; a server sign-in password is asked for again and
is not kept.

If take-over fails or is declined, **Sign in with Tesla Instead** starts the
ordinary sign-in route. With the imported car and no login, Hub runs with
collection off until sign-in inside an open window, then collects without a
restart.

The app distinguishes four refresh states. `none_sent` means the source login
is untouched and the first refresh waits for a window; sign out in Hub before
returning to the source. `attempt_recorded` means a refresh is in progress:
keep the source stopped until its outcome is known, and use a new sign-in if
returning to it. `completed` means Hub holds the login and the source copy no
longer works. `sign_in_required` asks for a new Hub sign-in and also requires
a new sign-in if returning to the source. For either of those last two states,
sign out in Hub before restarting the source application. See the
[migration guide](../releases/migration.md#going-back-to-the-source-application).

## How a window works

- A *grant* is a small signed file naming one host, a start, a hard expiry, the
  approver (exactly the owner) and a reason. The issuer signs the canonical
  body; Hub verifies the signature, the approver, its own host, the sequence
  number against its consumption anchor and the length against the code ceiling
  (four hours). A grant cannot be extended; a longer window needs a new grant.
- States: Closed, Pending (before the start time), Open, Closing, Closed.
  Close reasons are `expired`, `operator_close`, `relogin_needed`,
  `owner_api_refused`, `clock_invalid`, `reboot` and `state_unverifiable`.
- Inside a window the service's supervisor starts the collector, restarts it
  with backoff after a failure (the gap is recorded) and stops it at the hard
  expiry without an operator. The local API keeps serving throughout.
- Only one collector holds a window at a time: the process lifetime lock, the
  collector lease and a per-host claim all apply, and the issuer refuses
  overlapping grants for any host, with a gap of at least 15 minutes between
  hosts.
- Hub checks its clock against an NTP server when you request a window and
  every five minutes while it is Pending or Open. A measured offset of more
  than 30 seconds in either direction refuses the open with `clock_offset`,
  or closes an active window with `clock_invalid`. After a missed probe, Hub retries after 30 seconds. It
  closes on the third consecutive miss, or on a miss at least five minutes
  after the first. A measurement within the limit clears the miss count.
  At service restart, Hub requires a measurement within the limit before it
  resumes a window.
- On close Hub writes a handback record: grant id, opened and closed times,
  reason, coverage (the open interval minus recorded gaps) and per-table
  high-water marks for drives, charges and positions. The same write closes a
  drive or charge still open at its last evidence: a drive ends at its last
  recorded movement and a charge at its newest sample, never at the handback.
  The session then reaches the local API, sync and the next import once, even
  if Hub never collects again. This applies at expiry, after `window close`,
  when import admission records a handback the store lacks, and when a
  restored store whose window was left open is sealed with no window open on
  this host. A window that is still open closes nothing. A local write already
  under way may finish after the handback (see the sign-in notes below); a
  session it opens is taken up when the next window opens.

## Issuer key custody

- The issuer runs on the owner's Mac only. Its Ed25519 private key lives in
  1Password and is read through `op-cached` at each issuance as a 0600 file
  passed by path. It is never stored in a repository, the data directory, a
  backup, the lab or the login keychain.
- Create the key pair and the issuer ledger once with
  `tesatlas-hub window issuer-init --key-out KEY --trust-out TRUST`. Store the
  private key in 1Password, then discard the local copy. The trust document
  (the public key, its id and the trust epoch) goes to each Hub with
  `window trust`. `window issuer-trust --key-file KEY --trust-out TRUST` writes
  it again; add `--new-epoch` to start a new trust epoch.
- The ledger is a hash-chained file in
  `~/dev/machine/records/tesatlas/hub-evidence/window-issuer/` (directory mode
  0700, internal drive). After each issuance the ledger head digest and
  sequence are pinned in the login keychain; a ledger that neither matches nor
  extends the pin refuses issuance, which detects a restored older ledger.
- Issuance holds one exclusive lock from the overlap check through signing and
  publication. A reservation entry is written and synced before signing; if
  publication fails, the reservation keeps blocking overlaps until it is voided
  with `window void --key-file KEY --seq N --reason TEXT`.
- The issuer refuses to write a grant if the key file is missing, unreadable,
  wider than 0600 or the wrong length; the key id differs from the ledger; the
  ledger is missing or its chain is broken; the approver is not the owner or the
  approval reference is empty; the expiry is not after the start, is already
  past or exceeds the ceiling; or the interval overlaps another grant.

## First start on a fresh Mac

On a Mac with no Hub service yet, the owner sign-in starts the service, so the
window verbs below have something to talk to:

1. Open the app. In setup choose **Get Started**, **New installation**,
   **Continue**, **Owner sign-in**, **Continue**, then **Connect Tesla**.
2. The app confirms that no Hub LaunchAgent file exists and no Hub job is
   loaded, writes the default configuration, installs and starts the Hub
   service and waits until it answers (the page shows **Setting up Hub…**).
   Nothing is sent to Tesla, nothing is imported and no account is created.
3. A Hub that was never configured (every collector path off, a loopback
   bind, no car, no credential and no imported history) starts idle: its
   start check prints `"state": "idle"`, `window status`, `window trust`,
   `window open` and `window close` answer on the local control channel, and
   the collector stays off. Any other state is checked as before, so a Hub
   that is configured but broken still refuses to start.
4. With no Open window the page shows "Hub is set up and running on this
   Mac" as a notice, not an error: the service stays running and idle, both
   Tesla sign-in actions are unavailable and the bottom button is **Done**,
   which closes set-up without touching the service. The page asks the
   service again, read-only, whenever it shows and whenever you return to the
   set-up window, so it follows the window: with a window open it is the
   ordinary page with **Connect Tesla**. A faulted window (its state needs a
   new trust document), an answer the app cannot read, a pending credential
   recovery or a service that does not answer are never shown as the healthy
   notice: each has its own line. The window's close button works once the
   service answers, also after **Back**, and closing leaves Hub running. If
   the start fails, the page shows why and the app removes the service it
   installed (only that one); press **Connect Tesla** to retry. A removal that
   fails is reported and finished by the next press.
5. Trust the issuer, issue a grant and open the window as described below.
6. Return to the set-up window (or open set-up again from the app's Tesla
   account menu): the page shows **Connect Tesla** and the Tesla sign-in page
   opens when you press it. The sign-in runs in the running service, inside
   the window; the service does not restart.
7. Once a car is configured, the sign-in sets the collection interval (300
   seconds) in the configuration and reloads the collector before it
   reports completion: the collector runs inside the Open window at once and
   stops when the window closes. After a stop or a restart the Hub passes the
   usual checks (a car and the Hub's own login) and collects only inside an
   Open window. The Hub records that it was set up in `hub-setup.json` beside
   its catalogue; a set-up Hub whose `hub.sqlite` is missing or empty refuses
   to start instead of starting as new.

## Trust the issuer on a Hub

1. Copy the issuer trust document to the Hub host.
2. With the Hub service running and the window Closed, run
   `tesatlas-hub window trust FILE`. Hub stores the key and key id in its data
   directory and starts a fresh window ledger with the document's trust epoch.
   The epoch must be newer than the one Hub last trusted; a document from
   `window issuer-trust --new-epoch` invalidates every earlier grant.

A Hub whose window state or ledger is missing, corrupt or chain-broken refuses
every open until `window trust` is run again.

## Issue a grant

1. Agree the window with the owner and record the approval reference. Read the
   target Hub's host id with `tesatlas-hub window host-id` on that host.
2. On the issuer Mac, run `tesatlas-hub window issue --key-file KEY
   --host-id HOST --starts-at START --expires-at EXPIRY --approver G
   --reason TEXT --approval-ref REF --out GRANT`. Times are UTC, as RFC 3339
   (`2026-11-12T09:30:00Z`) or Unix seconds. The key is read from 1Password for
   this run.
3. Copy the signed grant file to the Hub host (to a guest, over SSH).

## Open

1. Confirm the state is Closed with `tesatlas-hub window status`, and that no
   import is running.
2. Run `tesatlas-hub window open --grant FILE`. A refusal
   returns a stable reason code and changes no state. Typical codes:
   `host_mismatch`, `import_running`, `collector_busy`, a clock refusal, or a
   grant at or below the consumption anchor.
3. If the grant starts in the future the window is Pending and opens at the
   start time.
4. Sign in with the Hub app while `window status` reports Open. The app passes
   the authorisation code, its PKCE verifier and the redirect URI to
   `tesatlas-hub setup` on standard input; `setup` forwards them to the
   running service, which performs the exchange and vehicle discovery. Outside
   an Open window the service refuses with `window_closed`. Collection starts
   once Hub holds its own login. Confirm the latest observation advances.

   Fleet polling with your own application uses the same window: its
   registration, sign-in and car discovery each need the window Open and are
   refused with `window_closed` otherwise. Polling then runs inside the window
   only and stops at its deadline. See the [Fleet guide](../guides/fleet-setup.md).

   The exchange uses Tesla's standard public owner client, the same values the
   app's sign-in window sends: client `ownerapi`, redirect
   `tesla://auth/callback`, scope `openid email offline_access`, and the global
   owner token endpoint `https://auth.tesla.com/oauth2/v3/token`. The refresh
   of the Hub's own pair uses the same client, under the one refresh fence.
   The service checks the material before anything is sent: any other
   redirect, a malformed verifier or code (`sign_in_material_invalid`) and a
   code it has already tried (`sign_in_code_reused`) are refused locally. It
   then asks the egress gate, sends one request and stores only a complete,
   valid answer, as the Hub's own login (origin `hub_login`). An issuer
   refusal (`sign_in_refused`), an unusable or oversized answer
   (`sign_in_answer_invalid`) or an unreachable issuer (`sign_in_unavailable`)
   stores nothing; start the sign-in again for a new code. Refusals name a
   fixed code only, never the code, verifier or a token.

   Vehicle discovery follows in the same request, inside the same Open window,
   with the pair just stored: one bounded products list over the Owner API,
   through the egress gate with a permit of its own, and no vehicle-data read,
   wake or command. `setup --vehicle-id ID` configures that listed car,
   `setup --all-vehicles` every listed car, and plain `setup` the one listed
   car (or the car this Hub already holds). The configured cars let collection
   start when the collector restarts. The report then reads
   `"vehicleDiscovery": "configured"` with `"vehiclesConfigured"`, and names
   no VIN, car name or token. If discovery fails after the sign-in is stored,
   `setup` still reports `"status": "signed_in"`, with
   `"vehicleDiscovery": "failed"` and a fixed `discoveryCode`: for example
   `discovery_vehicle_not_listed`, `discovery_selection_required`,
   `discovery_answer_invalid`, `discovery_refused` or `window_closed`. The
   stored login stays and no car is configured. To finish, run the same
   `setup` again with the same sign-in material (and a corrected choice)
   within 10 minutes, before the service restarts and while that login is
   still stored: the service recognises the sign-in it stored, sends nothing
   to Tesla's token endpoint (`"signIn": "resumed"`) and only lists the cars
   again. After that, a new sign-in is needed. These `setup` instructions
   describe Legacy Owner sign-in. A new installation offers Fleet or Legacy;
   use the [Fleet guide](../guides/fleet-setup.md) for Fleet onboarding.
   This Legacy login is only sent to Tesla's Owner API: an
   `owner_api_base_url` naming another host is refused
   (`discovery_settings_invalid`).

   While the window's collector runs it holds the Hub's credential store, so
   `setup` first asks the service to stop it; collection pauses for the sign-in
   and restarts on the new login once it ends. If the collector does not stop
   within 30 seconds the sign-in is refused (`collector_still_running`); a
   second sign-in while one runs is refused (`sign_in_busy`). A refusal before
   the write leaves the stored login as it was. If the disk does not confirm
   the write, `setup` reports `sign_in_stored_unconfirmed`: check
   `tesatlas-hub status` before signing in again. A stored sign-in whose
   follow-up cleanup failed reports `"aftercare": "pending"`; the service
   finishes it when it next opens the credential store, as the collector's
   restart does. A window closed during a sign-in stays Closing while the
   sign-in may still finish. The supervisor checks two thresholds, 40 and 55
   seconds after the window began closing, and acts on the first pass at or
   after each. It polls, and its passes are not timed to those moments, so a
   pass can come later than the threshold; the handback also does its own I/O.
   On the first pass at or after 40 seconds, new sign-in work is refused and
   nothing new starts. On the first pass at or after 55 seconds, the window is
   handed back whatever the sign-in is doing, unless it was handed back earlier
   because nothing was exchanging or writing. No Tesla traffic follows the
   handback. A local write that had already begun and then stalls is not cut
   off: it may still finish after the handback. It sends nothing to Tesla, and
   lands whole or not at all. The
   `setup` reply waits for that write, so it can arrive after the window has
   closed and report the discovery as failed. A later window, sign-in or
   credential store open picks up whatever was stored. A sign-in cut off before
   its login was committed refuses `window_closed` and leaves at most an unused
   binding, which expires; one cut off after the commit is stored with
   `"aftercare": "pending"`.

## Stop

- *At expiry:* nothing to do. At the deadline Hub stops issuing Tesla traffic,
  cancels streams, lets a token exchange already begun finish (up to 60
  seconds; a local write already under way may finish later, see the sign-in
  notes above), releases the collector lease and host claim, syncs the handback
  record and moves to Closed.
- *Early:* run `tesatlas-hub window close`. The same closing sequence
  applies with reason `operator_close`.
- Hub also closes itself on `relogin_needed`, `owner_api_refused`,
  `clock_invalid`, `reboot` and `state_unverifiable`. After `relogin_needed`
  the account stays signed out, and no retry happens until a new window and a
  fresh sign-in.

## Roll back to the other logger

Use this to return collection to the other logger after a window, or at any
time you want Hub to stop collecting.

1. **Stop.** Close the window (above) and confirm `window status` reports
   Closed with the handback record written.
2. **Revoke Hub's own grant.** Hub's login belongs to Hub alone, so revoking
   does not touch any other application:
   - sign Hub out (`tesatlas-hub control sign-out`) to remove its stored
     Tesla credential generations locally;
   - optionally revoke Hub's application from Tesla Account Security;
   - on the issuer Mac write a new-epoch trust document
     (`window issuer-trust --key-file KEY --trust-out TRUST --new-epoch`) and
     run `window trust TRUST` on the Hub, so no earlier grant can open a
     window.
   Hub's consumption anchor stays in the login keychain, so restoring an older
   data directory does not make a used grant valid again. The anchor also pins
   a close before its handback is confirmed, so restoring an Open copy cannot
   undo a completed close. If that pin cannot be saved, the close stays pending.
   When the login keychain is locked while Hub reads an anchor, macOS may ask to unlock it. For the read of an anchor saved by an earlier build, Hub waits five seconds, never stops the system helper, and treats the anchor as unavailable until a later load.
3. **Confirm the other logger resumed.** In the other logger's database, check
   that the newest position timestamp for each vehicle advances over several
   minutes while the car is driving or awake. Hub is not involved in this
   check and sends nothing to Tesla.
4. **Import the gap.** With the window Closed and the handback record written,
   run the read-only history import. Positions inside the covered interval keep
   Hub's rows; the rest come from the other logger. Import is refused while a
   window is Pending, Open or Closing.

## Restart

- Restarting the service never moves the deadline and never reopens a consumed
  grant. There is no extend command.
- If the service restarts while a window is Open or Pending, on the same boot,
  with a sound clock and before the deadline, it resumes until the original
  deadline, retakes the lease and claim, and records a `resumed` entry.
- An active snapshot whose older consumption anchor did not record whether
  the grant was open refuses resume. Trust a newer issuer trust document and
  issue a new grant; an old snapshot cannot establish that it was never closed.
- After a reboot the window closes with reason `reboot`; a new grant is needed.
- At or after the deadline it closes once with reason `expired`, timed at the
  deadline.
- If the state is unreadable it closes with `state_unverifiable`. A crash during
  Closing completes the close at the next start.
- Backups and restores never carry the window directory.
  `tesatlas-hub backup-now` runs one encrypted backup through the running
  service and `tesatlas-hub backup-status` shows the schedule; neither sends
  anything to Tesla.

## Remove refused credentials

An upgraded Hub keeps the credentials it refuses until you remove them: a
sign-in stored by an earlier version (the `pre_upgrade` origin), or other
credential material not admitted by its origin and refresh owner. A login
admitted through the source application take-over (`source_token` on `owner_api`,
refresh owner `hub`) is usable and is not refused material. Nothing is deleted
on upgrade. While such a credential is kept, `status` and `doctor` show a
`refusedCredentialPurge` entry naming the command, and while a refused
pre-upgrade credential blocks backups, `tesatlas-hub backup` is refused with
`backup_blocked_by_refused_credentials`, naming the same command. Use the
purge command to remove refused material; a new sign-in does not make that
material trusted.

1. With the Hub service running, list what would be removed:

   ```sh
   tesatlas-hub purge-refused-credentials
   ```

   The listing names each refused slot, its origin and the table that holds it,
   the old key files in the Hub's `secrets` folder and the old tables. It never
   prints a secret value, and it deletes and writes nothing.

2. Remove them:

   ```sh
   tesatlas-hub purge-refused-credentials --confirm
   ```

   The service deletes the refused rows, their encrypted values and the old
   tables in one database transaction, and removes the old key files only after
   that transaction is safely on disk. It then reclaims the freed space and
   completes the credential cleanup; the report says whether cleanup is
   complete. Admitted Hub sign-ins and taken-over logins are never touched.

The command is refused with `window_active` while a window is Pending, Open or
Closing, `collector_running` while a collector holds its lease, and
`refresh_active` while a refresh of the Hub's own sign-in is unresolved. It goes
only through the running service and makes no network connection. If cleanup
needs more free space, the report says it is incomplete, and the Hub finishes it
the next time it opens its credential store. Afterwards the store reports no refused slot and
backups are no longer blocked.
