# Migration from a user-controlled TeslaMate database

Migration exists to move authorised historical data and, where expressly selected, encrypted legacy credentials into Teslatlas-owned storage.

New Hub installations do not require TeslaMate.

**This project is an unofficial community tool and is not affiliated with, endorsed by or supported by the official TeslaMate project.**

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

The operator must have authority, back up and test recovery, stop concurrent credential refresh during cutover, preserve the source until validation, and comply with privacy/employment rules.

## Data differences

A tagged release must disclose imported and omitted records, transformations, units, time treatment, precision, open-session handling, deduplication and unsupported customisations.

## Separate bounded write-back

Migration is always read-only and is not a continuing bridge. A separate,
explicit `write-back charge-cost` command can lock and update one selected
TeslaMate charging-process cost. It defaults to rollback and requires
`--apply` to commit. Ordinary migration and collection never call it; no other
TeslaMate table or field is writable through Hub.
