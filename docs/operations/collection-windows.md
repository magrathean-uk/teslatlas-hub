# Collection windows

Hub sends Tesla traffic only with its own login, only on the host a signed grant
names, and only while a collection window is open. Outside a window it sends
nothing to Tesla (no sign-in exchange, vehicle discovery, telemetry
configuration or token refresh) and keeps history current by read-only import
from the other logger's database. Hub never reads, stores, uses or refreshes a
token held by another application, and no setting or flag changes that.

This guide covers issuing a grant, trusting the issuer, issuer key custody,
opening, stopping, rolling back and restarting. Command names marked
*(name may change)* are provisional until the operator commands land with
M02-T10; check `teslatlas-hub window --help` for the shipped spelling.

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
- Hub checks its clock against an NTP server (not a Tesla endpoint) when a
  window opens and every five minutes inside it, and closes if the offset
  exceeds 30 seconds or cannot be measured.
- On close Hub writes a handback record: grant id, opened and closed times,
  reason, coverage (the open interval minus recorded gaps) and per-table
  high-water marks for drives, charges and positions.

## Issuer key custody

- The issuer runs on the owner's Mac only. Its Ed25519 private key lives in
  1Password and is read through `op-cached` at each issuance as a 0600 file
  passed by path. It is never stored in a repository, the data directory, a
  backup, the lab or the login keychain.
- Create the key pair and the issuer ledger once with `window issuer-init`
  *(name may change)*. Store the private key in 1Password, then discard the
  local copy. The public key goes to each Hub with `window trust`.
- The ledger is a hash-chained file in
  `~/dev/machine/records/teslatlas/hub-evidence/window-issuer/` (directory mode
  0700, internal drive). After each issuance the ledger head digest and
  sequence are pinned in the login keychain; a ledger that neither matches nor
  extends the pin refuses issuance, which detects a restored older ledger.
- Issuance holds one exclusive lock from the overlap check through signing and
  publication. A reservation entry is written and synced before signing; if
  publication fails, the reservation keeps blocking overlaps until it is voided
  with an explicit entry.
- The issuer refuses to write a grant if the key file is missing, unreadable,
  wider than 0600 or the wrong length; the key id differs from the ledger; the
  ledger is missing or its chain is broken; the approver is not the owner or the
  approval reference is empty; the expiry is not after the start, is already
  past or exceeds the ceiling; or the interval overlaps another grant.

## Trust the issuer on a Hub

1. Copy the issuer public key file to the Hub host.
2. With the window Closed, run `teslatlas-hub window trust FILE` *(name may
   change)*. Hub stores the key and key id in its data directory and starts a
   fresh window ledger with a new trust epoch. Running it again sets a new
   epoch, which invalidates every earlier grant.

A Hub whose window state or ledger is missing, corrupt or chain-broken refuses
every open until `window trust` is run again.

## Issue a grant

1. Agree the window with the owner and record the approval reference.
2. On the issuer Mac, run `teslatlas-hub window issue` *(name may change)*
   with the host id of the target Hub, the start, the expiry, the approver, the
   reason and the approval reference. The key is read from 1Password for this
   run.
3. Copy the signed grant file to the Hub host (to a guest, over SSH).

## Open

1. Confirm the state is Closed with `teslatlas-hub window status` *(name may
   change)*, and that no import is running.
2. Run `teslatlas-hub window open --grant FILE` *(name may change)*. A refusal
   returns a stable reason code and changes no state. Typical codes:
   `host_mismatch`, `import_running`, `collector_busy`, a clock refusal, or a
   grant at or below the consumption anchor.
3. If the grant starts in the future the window is Pending and opens at the
   start time.
4. Sign in with the Hub app while `window status` reports Open. The app passes
   the authorisation code to the running service, which performs the exchange.
   Collection starts once Hub holds its own login. Confirm the latest
   observation advances.

## Stop

- *At expiry:* nothing to do. At the deadline Hub stops issuing Tesla traffic,
  cancels streams, lets a token exchange already begun finish (up to 60
  seconds), releases the collector lease and host claim, syncs the handback
  record and moves to Closed.
- *Early:* run `teslatlas-hub window close` *(name may change)*. The same
  closing sequence applies with reason `operator_close`.
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
   - sign Hub out (`teslatlas-hub sign-out` *(name may change)*) to remove its
     stored Tesla credential generations locally;
   - optionally revoke Hub's application from Tesla Account Security;
   - run `window trust` again with the same issuer key to set a new trust
     epoch, so no earlier grant can open a window.
   Hub's consumption anchor stays in the login keychain, so restoring an older
   data directory does not make a used grant valid again.
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
- After a reboot the window closes with reason `reboot`; a new grant is needed.
- At or after the deadline it closes once with reason `expired`, timed at the
  deadline.
- If the state is unreadable it closes with `state_unverifiable`. A crash during
  Closing completes the close at the next start.
- Backups and restores never carry the window directory.
