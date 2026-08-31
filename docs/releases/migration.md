# Migration from a user-controlled TeslaMate database

Migration exists to move authorised historical data and, where expressly selected, encrypted legacy credentials into Teslatlas-owned storage.

New Hub installations do not require TeslaMate.

**Teslatlas Hub is an independent project and is not affiliated with, endorsed by or supported by the TeslaMate project.**

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

The explicit credential-transfer path may read the relevant encrypted token relation only where requested and authorised.

## Operator duties

The operator must have authority, back up and test recovery, update TeslaMate
to 4.2.0 or newer, start it once, wait for its database migrations to finish,
stop concurrent credential refresh during cutover, preserve the source until
validation, and comply with privacy/employment rules.

## Data differences

The v1 beta importer is a selected-car projection, not a PostgreSQL backup and
not a continuing TeslaMate bridge. The running app must be TeslaMate 4.2.0 or
newer, and the database must match the exact reviewed v4.2-compatible migration
set. That schema is also present in v4.1.1, so database evidence cannot prove
the app version and the operator must acknowledge the limitation explicitly.
Extra tables and columns are allowed, but a missing or changed reviewed column,
enum, relationship, or migration set fails closed. A later TeslaMate release
that adds migrations remains blocked until its schema delta is reviewed.

### Imported records

One import selects one TeslaMate car. Run a separately reviewed import for each
additional car. The read-only snapshot captures:

- the selected `cars` row, its referenced `car_settings` row, and the single
  source-wide `settings` row;
- that car's `drives`, `positions`, `charging_processes`, `charges`, `states`,
  and `updates` rows;
- only `addresses` and `geofences` referenced by that car's drives or charging
  processes;
- current open-drive, open-charge, and open-state rows plus their bounded
  position and charge-sample tails;
- encrypted legacy access/refresh token ciphertext only when the operator
  separately selects the credential-transfer scope.

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
- TeslaMate runtime configuration, database users/permissions, containers,
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
- Hub does not convert history according to the TeslaMate display preferences.
  TeslaMate's unit-of-length, temperature, pressure, and preferred-range labels
  are preserved separately. Fields already defined by TeslaMate as kilometres,
  minutes, energy, power, cost, elevation, speed, or pressure retain those
  source values; no currency or tariff conversion is inferred.
- Exact car/settings text is retained in the physical import. Compatibility
  views may normalise known model/trim labels and trim an update version for
  display; the physical source row remains available in the typed pack.

### Open sessions and deduplication

Completed history and the open tail are first captured from one exported,
repeatable-read snapshot. Immediately before publication, Hub performs a second
bounded open-tail read. Direct publication succeeds only when both reads are
identical; movement, a changed parent, or new child rows aborts the unpublished
generation and requires a retry.

When exactly one drive or charge is open, Hub seeds it and its children as
provisional lifecycle state so later native collection can close it. If several
stale drives or charges are open, Hub does not guess which parent is live. The
newest open state row is used as current state. The reviewed rows still remain
in the immutable physical capture.

Source row identity, per-table watermarks, and a content fingerprint make a
retry idempotent. Re-importing an unchanged capture skips a new history
publication while still preserving the reconciled open tail. Changed rows are
published as a successor. Removed rows become typed tombstones only for the
inventoried telemetry entities: drives, positions, charging processes, charge
samples, states, and updates. The selected car identity is never tombstoned;
car settings, geofences, and addresses are represented by the successor
snapshot but are not part of the tombstone inventory. Open-child reconciliation
unions rows by source ID and replaces an older value with the later value rather
than duplicating it.

### TeslaMate customisations

Global and per-car TeslaMate settings, referenced geofences, and supported
address fields are captured as data; they are not executed as Hub service
configuration. Extra custom tables/columns are ignored. A customisation that
changes a reviewed type, nullability, fixed precision, enum ordering, settings
cardinality, car/settings relationship, or migration history is unsupported and
causes admission to fail. Remove or separately export unsupported custom data;
Hub does not silently coerce it.

## Separate bounded write-back

Migration is always read-only and is not a continuing bridge. A separate,
explicit `write-back charge-cost` command can lock and update one selected
TeslaMate charging-process cost. It defaults to rollback and requires
`--apply` to commit. Ordinary migration and collection never call it; no other
TeslaMate table or field is writable through Hub.
