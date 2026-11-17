# Import your existing history

Tesatlas Hub can import vehicle history from another program's PostgreSQL
database, which this guide calls the source database. Hub does not go by that
program's name: it reads the database's schema and compares it with the schema
sets this release recognises. Run the check first
(`tesatlas-hub source-check`, or Import History in the Mac app). It only reads
the database, and it tells you whether Hub recognises it before anything is
imported.

Tesatlas Hub is developed independently of the software it imports from.

## Which databases Hub recognises

Each release recognises a fixed list of schema sets. A schema set is the list
of schema changes the source application has applied to its database, and Hub
identifies it by a SHA-256 digest. The release notes list the recognised sets
by the first 12 characters of that digest, with the source application
releases each set was tested with. If the check does not recognise your
database, do not import it with this release.

## Before you start

1. Back up the source application and keep a recoverable copy. Do not remove the original
   installation after the first successful import.
2. Update the source application to release 4.4.0 or later, start it once, and
   let its database changes finish. A newer release is not automatically
   supported: the check tells you.
3. Prepare your server address, SSH port, user and authentication. The account
   must be able to access the source application Docker deployment; if it needs sudo,
   select that option in the Mac form. Do not grant broad permissions simply
   to bypass a failed check.
4. For migration, stop the source application and its dashboards and leave
   it stopped. Keep PostgreSQL running so Hub can read it. Disable any
   automatic restart that would bring the source application back. Hub will take over its
   Tesla login and become its only refresher. For a new installation without
   migration, choose Fleet or Legacy and sign in instead.

## Import using the Mac app

1. Stop the source application and its dashboards before importing, and leave it
   stopped throughout and afterwards. The Hub does not stop it for you.
2. Choose **Import History** during setup, or from **Manage Tesla**. Enter
   the server, port and SSH user. For **SSH key**, use **Choose Key…** to
   select the private key, or leave it empty for your SSH agent or default
   keys. Use **Password** only for password authentication.
3. Choose **Connect to Server**. Read the compatibility result and
   acknowledgement. The app checks over SSH and Docker that the source application is
   stopped, and checks again immediately before import. If it is running,
   the page asks you to stop it and its dashboards, then choose **Check Again**.
4. Find the `ENCRYPTION_KEY` value in your TeslaMate deployment configuration,
   such as its Docker Compose environment settings or the environment file
   those settings reference. Its public
   [TeslaMate Environment Variables documentation](https://docs.teslamate.org/docs/configuration/environment_variables)
   identifies `ENCRYPTION_KEY` as the required key used to encrypt its Tesla
   API tokens. Paste that value once into the secure **Encryption key** field.
   Hub clears the field when take-over starts, passes it on standard input
   and never keeps it in settings, a file or logs. It is not your Tesla
   password or a token.
5. Select **Import Data**. History imports first, without reading credentials.
   The next step shows **Taking over the Tesla login…**. Hub reads and decrypts
   the source application's stored access and refresh pair, checks it, and stores the pair
   encrypted in its own credential vault as origin `source_token`, with the
   Hub as refresh owner. The owner slot must be empty. This step sends nothing
   to Tesla and changes nothing in the source application's database.
6. Complete the Hub checks and finish page. Acknowledge that you will keep
   the source application stopped, then choose **Start Hub**. Hub now holds the login, but
   its first refresh waits for a collection window. Your cars appear once
   the first collection window opens, without another Tesla sign-in. Check
   the intended cars, imported journeys and charges, diagnostics and subsequent
   collection. Keep the original data until you are satisfied with the result.

### If the take-over cannot finish

A wrong encryption key gives `source_token_key_mismatch`: the key does not
open the stored login. The app shows **History imported; Tesla login not taken
over**. Check the deployment's key, paste it again and choose **Retry**. The
field was cleared, so each attempt needs the key again. History stays imported.
You can choose **Sign in with Tesla Instead** to use the Hub's sign-in page
after starting it; the sign-in itself needs an open collection window.

A running source application blocks the app's stop check. The Hub also checks database
sessions before reading the token row: `source_still_running` means another
client is connected, which can be a dashboard or another database tool even
if the source application itself is stopped. Stop those clients and retry. Keep PostgreSQL
running. `source_token_invalid` means the stored login is not a usable Tesla
Owner login; sign in with Tesla in the Hub instead. `slot_occupied` means Hub
already holds a login; sign out first if you intend to replace it by take-over.
These refusals leave the existing login and the source database untouched.

### Going back to the source application

- Before the Hub's first refresh: sign out in Hub, then start the source application. Its
  stored login still works because take-over has not spent its refresh token.
- After the first refresh: sign out in Hub, start the source application and sign in again
  through the source application's own sign-in page. Its old stored login no longer works.
  Running both on that login can lock you out.

Hub never writes a replacement login into the source application's database and never
exports a token. If you cannot tell whether the first refresh happened, use
the source application's own sign-in again after signing out in Hub.

### Command-line take-over

`take-over-source-token --source <password-free PostgreSQL URL>
--accept-source-stopped` is separate from `migrate`. Stop the Hub service too;
the window must be Closed with its handback recorded. Standard input takes
one JSON object with exactly `postgresPassword` and `encryptionKey`. Pass it
directly through a protected input pipe, never through shell arguments,
environment variables or a saved JSON file. Do not paste a token. A successful
run prints only:

```json
{"status":"taken_over","origin":"source_token","firstRefresh":"next_window","sourceSessionsSeen":0,"sourceNeverMutated":true}
```

The [security model](../architecture/security-model.md#source-login-take-over)
describes the read-only connection and remaining refusals. The
[collection-window guide](../operations/collection-windows.md) covers the first
refresh. For authentication, Docker access or compatibility errors, see
[Troubleshooting](../guides/troubleshooting.md).

## What transfers

An import selects one car and its supported driving, charging, position, state
and update history, with referenced settings, addresses and geofences. It is
not a copy of the entire source deployment. Grafana dashboards, containers,
logs, custom tables and unselected vehicles are not transferred. Credentials
are separate from an ordinary history import. The exact scope follows below.

## Source protection

The importer must:

- use an operator-supplied endpoint and credentials;
- use a read-only transaction and least-privilege source role;
- validate a supported schema/migration set;
- query only reviewed relations;
- never repair, migrate or write to the source;
- fail closed on unknown schema;
- write output only to Hub-owned storage separate from the source;
- verify integrity and counts before cutover;
- preserve rollback.

Only the separate login take-over reads `private.tokens`, through its own
read-only connection. The history importer never reads that relation.

## Operator duties

The operator must have authority, back up and test recovery, update the source application
to a supported release, start it once, wait for its database migrations to finish,
stop concurrent credential refresh during cutover, preserve the source until
validation, and comply with privacy/employment rules.

## Data differences

The importer is a selected-car projection, not a PostgreSQL backup or live
replication service. An operator can repeat a history-only, read-only import
with `--online-snapshot --history-only` while retaining the
Hub's stored Legacy pair. The running source application must use a supported
release, and the database must match a schema set this Hub recognises. Several
application releases can share one schema set, so the schema cannot establish
the running release; the operator must acknowledge that limit explicitly.
Extra tables and columns are allowed, but a missing or changed reviewed column,
enum, relationship, or migration set fails closed. A later source application release
that adds migrations remains blocked until its schema delta is reviewed. The
current contract and each review are in
[Source schema compatibility](source-schema-compatibility.md).

### Imported records

One import selects one source car. Run a separately reviewed import for each
additional car. The read-only snapshot captures:

- the selected `cars` row, its referenced `car_settings` row, and the single
  source-wide `settings` row;
- that car's `drives`, `positions`, `charging_processes`, `charges`, `states`,
  and `updates` rows;
- only `addresses` and `geofences` referenced by that car's drives or charging
  processes;
- current open-drive, open-charge, and open-state rows plus their bounded
  position and charge-sample tails.

Login take-over is a separate operation and is not part of this history capture.

Source row IDs and relationships remain represented in the typed import. Hub
also assigns its own source, vehicle, snapshot, and sequence identities. Valid
referenced geofences are materialised for Hub use with their name, geometry,
billing type, cost per unit, and session fee.

### Omitted records

The importer does not copy:

- any unselected car or its telemetry;
- unreferenced address or geofence rows;
- `addresses.raw` JSON, because it can contain unrelated provider data and is
  not used by the app;
- the contents of `schema_migrations` (the ordered versions are checked only as
  source-admission evidence);
- token ciphertext during an ordinary history import;
- source deployment configuration, database users/permissions, containers,
  dashboards, Grafana data, logs, caches, or any unlisted relation;
- custom columns or tables. They may remain in the source, but are not part of
  the fixed projection.

Invalid geofence geometry, an empty name, or a name longer than 256 characters
is retained in the typed source capture where representable but is not
materialised as an active Hub geofence.

### Transformations, time, units, and precision

The schema-2.2 physical import preserves source values before presentation
policy is applied:

- PostgreSQL `timestamp without time zone` bytes remain signed microseconds
  since PostgreSQL's 2000-01-01 epoch, including infinity sentinels. Source
  `timestamp(0)` fields must be whole seconds. The compatibility history view
  treats finite source timestamps as UTC and represents them as Unix epoch
  milliseconds; no local-time-zone conversion is performed.
- Fixed-scale PostgreSQL numerics retain their declared scale and preserve the
  distinction between finite values and `NaN`: coordinates use 6 decimals,
  temperatures and tyre pressures use 1, ranges and energy/cost/session fee use
  2, and geofence cost-per-unit uses 4.
- PostgreSQL `double precision` odometer, distance, start/end kilometre, and
  efficiency values retain their exact IEEE-754 bit patterns in the physical
  import.
- Hub does not convert history according to the source display preferences.
  The source application's unit-of-length, temperature, pressure, and preferred-range labels
  are preserved separately. Fields already defined by the source application as kilometres,
  minutes, energy, power, cost, elevation, speed, or pressure retain those
  source values; no currency or tariff conversion is inferred.
- Exact car/settings text is retained in the physical import. Compatibility
  views may normalise known model/trim labels and trim an update version for
  display; the physical source row remains available in the typed pack.

### Open sessions and deduplication

Completed history and the open tail are first captured from one exported,
repeatable-read snapshot. Immediately before publication, Hub performs a second
bounded open-tail read. Direct publication succeeds only when both reads are
identical; movement, a changed parent, or new child rows aborts that import
generation and requires a retry.

When exactly one drive or charge is open, Hub seeds it and its children as
provisional lifecycle state so later native collection can close it. If several
stale drives or charges are open, Hub does not guess which parent is live. The
newest open state row is used as current state. The reviewed rows still remain
in the immutable physical capture.

Source row identity, per-table watermarks, and a content fingerprint make a
retry idempotent. Re-importing an unchanged capture skips a new history
publication while still preserving the reconciled open tail. Changed rows are
published as a complete signed PhysicalV3 successor; when bounded by Protocol
1.4, an immediate predecessor also gets a signed changed set. A client with an
older retained base receives a signed full replacement. Removed rows become
typed tombstones in the compatibility projection only for the
inventoried telemetry entities: drives, positions, charging processes, charge
samples, states, and updates. The selected car identity is never tombstoned;
car settings, geofences, and addresses are represented by the successor
snapshot but are not part of the tombstone inventory. Open-child reconciliation
unions rows by source ID and replaces an older value with the later value rather
than duplicating it.

### Source customisations

Global and per-car source application settings, referenced geofences, and supported
address fields are captured as data; they are not executed as Hub service
configuration. Extra custom tables/columns are ignored. A customisation that
changes a reviewed type, nullability, fixed precision, enum ordering, settings
cardinality, car/settings relationship, or migration history is unsupported and
causes admission to fail. Remove or separately export unsupported custom data;
Hub does not silently coerce it.

## Never writes back to the source

Migration is always read-only and is not live replication. Hub never writes to
the source database: no command, migration step or collection path updates
any table or field of the source database. The former `write-back charge-cost` command was
removed.
