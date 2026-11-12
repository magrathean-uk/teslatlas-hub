# Source schema compatibility review

The history importer admits exactly one reviewed source schema: the complete,
ordered migration set and the physical shape of every relation, column and
enum it reads. This page records each review. The admission rules are in
[Migration](migration.md#data-differences); the contract is
`src/import/source_db/schema.rs`.

## Current contract: schema 20261014120000

| Fact | Value |
|---|---|
| Source application release | v4.4.0, tag revision `42d187daadd0104fc9c4509785df47a26705845a` |
| Newest migration | `20261014120000` |
| Migration count | 107 |
| Migration set SHA-256 | `c497f904ef7426ea2468f7ed13044d6f74fe34e0c3e14e5c3360eea0ed35edea` |
| Refused | any newer migration, any other set, and the earlier set below |

The SHA-256 covers the sorted, newline-terminated, 14-digit migration versions.
Several application releases can share one schema, so a match proves the
schema, not the running release.

## Review of 20260808090000 to 20261014120000

Reviewed on 12 November 2026 against a restored, read-only copy of a household
source database at `20261014120000`, using only the database catalogue and the
source application's public v4.4.0 release notes. No source code or migration
file of the source application was read.

Method:

- The copy's first 105 migration versions hash to the previous pinned set
  (`ea850d1b038c4af950db32e7a0939aa5ebe8f1dcefe5e56dcd592f3451038868`), so
  the new schema adds exactly two migrations: `20261009120000` and
  `20261014120000`.
- The importer's own column probe (`SCHEMA_PROBE_SQL`) and enum probe
  (`ENUM_PROBE_SQL`) were run on the copy and compared with every pinned
  column: 169 columns in the eleven read relations, with type, physical format
  and nullability.
- Constraints on the read relations (primary keys, foreign keys, checks and
  not-null) were listed from `pg_constraint`.

Differences that reach the importer:

| Relation | Column or type | Before | After | Effect on the importer |
|---|---|---|---|---|
| `settings` | `preferred_range` column type | enum `range` | enum `preferred_range` | Admission now expects the new type name. Same labels (`ideal`, `rated`) in the same order and still not null, so imported values keep their meaning. The reader casts the value to text and is unchanged. |
| (enum) | `range` | labels `ideal`, `rated` | renamed `preferred_range`, same labels | The enum probe now reads `preferred_range`. |

Differences outside the importer's reads:

- `import_runs.car_id` is now `smallint`, matching `cars.id` (release note
  "align import_runs.car_id type with cars.id"). The importer never reads
  `import_runs`.

No other column, type, format, nullability, enum label or constraint read by
the importer changed. The release notes also say that editing a geofence's
location or radius re-assigns drives and charges by distance. That changes which
geofence id a row may carry after an edit, not the meaning of the column, and
the importer already imports whatever id the row holds.

Consequences:

- A source still at `20260808090000` (the v4.2-compatible set) is refused as an
  older schema and must be upgraded to v4.4.0 first. Imports and published
  packs record the v4.4.0 revision and set hash.
- Earlier contracts: v4.2.0 (`e8d24886f97f22469c2675f89be843f6d401c76a`) and
  v4.1.1 (`d6c43bc8c48784da8f0b701945b80b20911b3d1a`) share the 105-migration
  set ending at `20260808090000`; both revisions remain accepted only as beta
  provenance.
