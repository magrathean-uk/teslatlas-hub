# Security model

## Protected assets

- Hub's own Tesla access and refresh tokens;
- the window issuer key and its ledger;
- Fleet virtual-key and command-proxy material;
- device pairing bearers and cursor-signing keys;
- exact vehicle identity, location, and travel history; and
- backup and recovery keys.

## Trust boundaries

| Boundary | Control |
|---|---|
| Local plaintext HTTP | Loopback only. Protected routes require the private local bearer and one matching `Host` header. Health, readiness, discovery, and Fleet ingress have explicit route handling; Fleet ingress has its own bearer check. |
| Remote sync | TLS plus paired-device bearer authentication. |
| Pairing | Short-lived, single-use invitation; claim is the only unauthenticated mutation. |
| Fleet Telemetry ingress | Private bearer on supervised loopback route. |
| Tesla credentials | Hub uses a login from its own sign-in or takes over the source application's login during migration, with the source application stopped and kept stopped. Hub becomes its only refresher. Only the resident service, inside an open collection window, sends Tesla traffic and refreshes that login; one refresh runs at a time. |
| Collection window | A grant signed by the Mac's window issuer, naming this host, approved by the owner and ending at a hard expiry. Outside a window Hub sends nothing to Tesla. History-only imports remain available; after take-over the source application stays stopped. See [Collection windows](../operations/collection-windows.md). |
| history import | Read-only PostgreSQL transaction and exact schema admission. |
| Vehicle command | Explicit action, confirmation, selected vehicle, bounded proxy. |
| Source release | Immutable annotated tag, exact version, clean tree, provenance classification, and licence material. |

## Source login take-over

Take-over belongs to a migration: history imports first and sets up the car.
With no configured car, the command refuses with `history_import_required`,
tells the user to import history first and writes nothing.
History import remains credential-free. The separate `take-over-source-token`
verb reads the encrypted `access` and `refresh` values from exactly one row in
`private.tokens`. It opens its own PostgreSQL connection using the source
application's database role, with a bounded, read-only, repeatable-read
transaction, separate from the history import connection. Before reading the
row it checks for other client sessions on that database and refuses if any
exist, including dashboards. It never writes to the source database or sends
anything to Tesla. Stopped application containers remain discoverable by the
Mac app. They must not be running or automatically restart; closing the web
page or pausing collection is insufficient. Keep PostgreSQL running and the
source application stopped, including between windows.

The Mac app takes the source application's `ENCRYPTION_KEY` in a secure field, clears the
field as the take-over starts and passes the key and database password to the
local Hub verb on standard input. The key is used in memory to decrypt the
pair, then discarded. The Hub never puts the key in a command argument,
environment variable, log line or file, and never keeps it for later use.
See the [migration guide](../releases/migration.md) for where to find it.

After decryption and token validation, the Hub seals the access and refresh
pair in its credential vault in the empty `owner_api` slot, with origin
`source_token` and refresh owner `hub`. The pair is due for refresh at the next
collection window; take-over itself does not spend the refresh token. The
single-use rotation fence and unresolved-refresh stop still apply. The success
report contains only `status: taken_over`, `origin: source_token`,
`firstRefresh: next_window`, `refreshProgress: none_sent`, `sourceSessionsSeen: 0` and
`sourceNeverMutated: true`, with no token, key or fingerprint.

The verb requires `--source` with a password-free PostgreSQL URL and
`--accept-source-stopped`. Its bounded standard input must contain exactly
`postgresPassword` and `encryptionKey`; token-named keys and other shapes are
refused. It requires the Hub service stopped, the window Closed and its
handback recorded. Repeating take-over for the same login, including its refresh successors,
reports `already_taken_over` and writes nothing. A different login in the
owner slot is refused (`slot_occupied`). It also refuses an
opaque refresh fence requiring a fresh sign-in (`fresh_sign_in_required`),
spent tokens, malformed or foreign-issuer tokens, missing or multiple source
rows, oversized values and a wrong encryption key. Only the dedicated take-over path
can introduce this origin; refresh successors keep it. Direct, provider and pasted tokens, token input to
`setup` or `migrate`, source credentials owned by an external refresher or on
the Fleet slot, source writes and Tesla sends outside a window remain refused.

Status distinguishes `none_sent` (source login untouched), `attempt_recorded`
(refresh in progress, keep the source stopped until the outcome is known),
`completed` (Hub holds the login and the source copy no longer works) and
`sign_in_required` (sign in again in Hub). Only `none_sent` allows a return to
the source's stored login after signing out in Hub; the other states require
a new sign-in in the source application. Unknown progress is not proof that
no refresh was sent.

After an interrupted take-over, the app resumes without re-importing and
checks durable credential status. Reconnecting repeats the stop check. A
server sign-in password is requested again and is not kept. If take-over
fails or is declined and no login is stored, **Sign in with Tesla Instead**
starts Hub awaiting ordinary sign-in, with collection off. After sign-in in
an open window, collection starts without restarting Hub.

## Defensive defaults

- configuration rejects unknown fields and unsafe paths;
- non-loopback bind requires TLS;
- ordinary request bodies, concurrency, handlers, pack streams, logs, caches,
  retries, and shutdown are bounded;
- credential-like provider fields are stripped before retention;
- secrets enter through protected files or bounded standard input, not argv;
- log display, copy, and save redact credentials and identifying telemetry;
- service units use dedicated accounts and operating-system sandboxing;
- fake/development sources are unavailable in release operation; and
- migration and schema admission fail closed.

## Out of scope

This security model does not authorise testing Tesla, third-party source deployments, Apple, GitHub,
another vehicle, or another person's system.

Report product vulnerabilities privately under the [security policy](../../.github/SECURITY.md).
