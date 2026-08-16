# Migration from a user-controlled TeslaMate database

## Scope

The migration adapter exists to help an operator move selected historical data from a PostgreSQL database the operator is authorised to access into Teslatlas-owned storage. New Teslatlas installations do not require TeslaMate.

Migration is one-way unless a tagged release expressly states otherwise.

## Non-affiliation

**This project is an unofficial community tool and is not affiliated with, endorsed by, or supported by the official TeslaMate project.**

Do not request support for this migration from the TeslaMate maintainers.

## Read-only design

The importer must:

- accept an operator-supplied endpoint and credentials;
- establish a read-only, repeatable-read transaction;
- validate the supported schema and migration set;
- query only fixed, reviewed tables and columns;
- avoid `private.tokens` and other credential relations;
- never alter, lock for write, migrate or repair the source;
- fail closed on an unknown schema;
- stage output separately;
- verify row counts and integrity before cutover;
- retain a rollback path.

Read-only behaviour reduces risk but does not make a backup unnecessary.

## Operator responsibility

The operator must:

- own the data or have authority from the relevant controller and users;
- take a tested backup before migration;
- use a dedicated read-only PostgreSQL role where possible;
- restrict network exposure;
- preserve the source until validation is complete;
- verify time zones, units, selected vehicle and row counts;
- decide retention and deletion after cutover;
- comply with data-protection and employment rules.

## Compatibility

Compatibility is pinned to reviewed schema revisions. Later TeslaMate releases are rejected until reviewed. Supporting a schema does not imply support for every extension, custom migration, corrupted database or third-party fork.

## Data differences

Teslatlas and TeslaMate do not have identical internal models. A release must disclose:

- records imported;
- records intentionally omitted;
- transformations and unit conversions;
- handling of open drives and charging sessions;
- precision and timestamp treatment;
- deduplication behaviour;
- unsupported customisations;
- validation output.

## Evidence and logs

Migration logs must avoid secrets and precise location unless needed for a local validation report. A support bundle must be redacted before transmission.

## No write-back

The Hub must not use the migration adapter as a continuing write-back bridge to TeslaMate. Any future bidirectional feature requires a separate design, legal and data-integrity review.
