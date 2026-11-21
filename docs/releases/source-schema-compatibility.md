# Source schema compatibility review

The history importer admits exactly one reviewed source schema: the complete,
ordered migration set and the physical shape of every relation, column and
enum it reads. This page records each review. The admission rules are in
[Migration](migration.md#data-differences); the contract is
`src/import/source_db/schema.rs`.

## Current contract: schema 20261102130000

| Fact | Value |
|---|---|
| Source application release | v4.5.0, tag revision `83652a688ef4ff2118572e9f48c49807e348453f` |
| Newest migration | `20261102130000` |
| Migration count | 108 |
| Migration set SHA-256 | `65c5af7596b2a550cccc03203be2825ef1ce892154e1dc2ed573ad0cbb9db3da` |
| Refused | any newer migration, any other set, and the earlier sets below |

The SHA-256 covers the sorted, newline-terminated, 14-digit migration versions.
Several application releases can share one schema, so a match proves the
schema, not the running release.

## Review of 20261014120000 to 20261102130000

Reviewed on 19 November 2026 against a restored, read-only copy of a household
source database running v4.5.0 (released 17 November 2026), using only the
database catalogue, the schema dump of the same database at v4.4.0 and the
source application's public v4.5.0 release notes. No source code or migration
file of the source application was read.

- The copy's first 107 migration versions hash to the v4.4.0 set
  (`c497f904ef7426ea2468f7ed13044d6f74fe34e0c3e14e5c3360eea0ed35edea`), so
  v4.5.0 adds exactly one migration: `20261102130000`. The release notes and
  the public tag comparison do not mention it; the database does.
- The schema dumps before and after differ only by the new table
  `terrain_elevation_queue` (`drive_id integer not null` primary key with a
  cascading foreign key to `drives(id)`, and `attempted_at timestamp`). The
  importer does not read it, and no column, type, format, nullability, enum
  label or constraint of a relation the importer reads changed.
- Behaviour change: v4.5.0 adds missing elevation to every ended drive and
  updates its ascent and descent, including drives closed long ago and imported
  drives. A drive the Hub already imported can therefore change its ascent,
  descent or position elevation. The importer reads those columns, digests each
  projected row whole and re-reads the whole history on every import, so the
  next import publishes the changed rows as an update and the Hub keeps the new
  values. Parity does not cover these columns, so it is unaffected. A backfill
  that changes very many position rows makes that update large; if it no longer
  fits the vehicle's sync history, an import for a vehicle the Hub has also
  collected is refused without changing anything, as for any other large
  source change.

Consequences:

- A source still at `20261014120000` (the v4.4-compatible set) is refused as an
  older schema and must be upgraded to v4.5.0 first. The source check reports
  `older_than_v4_5_compatible_schema` with that guidance, and the Mac app's
  guided import asks for release 4.5.0 or later before it connects. Imports and
  published packs record the v4.5.0 revision and set hash; a successful check
  reports `v4_5_compatible_schema` with required release `4.5.0`, which is the
  only report the Mac app accepts for import.

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

- At the time of this review, a source still at `20260808090000` (the
  v4.2-compatible set) was refused as an older schema and had to be upgraded
  to v4.4.0 first. The source check reported
  `older_than_v4_4_compatible_schema` with that guidance, and the Mac app's
  guided import asked for release 4.4.0 or later before it connected. Imports
  and published packs recorded the v4.4.0 revision and set hash; a successful
  check reported `v4_4_compatible_schema` with required release `4.4.0`.
- Earlier contracts: v4.2.0 (`e8d24886f97f22469c2675f89be843f6d401c76a`) and
  v4.1.1 (`d6c43bc8c48784da8f0b701945b80b20911b3d1a`) share the 105-migration
  set ending at `20260808090000`.

## Verifying artefacts published under an earlier set

Source admission accepts only the current set. Artefacts that an earlier
import already published (source witnesses, captures and packs) keep the
revision they were captured under. Each reviewed set is a tuple of revision,
migration-set hash, migration count and newest migration, and an artefact is
verified against the tuple of its own revision:

| Revision | Release | Set SHA-256 | Count | Newest migration |
|---|---|---|---|---|
| `83652a688ef4ff2118572e9f48c49807e348453f` | 4.5.0 (current) | `65c5af75…b3da` | 108 | `20261102130000` |
| `42d187daadd0104fc9c4509785df47a26705845a` | 4.4.0 | `c497f904…edea` | 107 | `20261014120000` |
| `e8d24886f97f22469c2675f89be843f6d401c76a` | 4.2.0 | `ea850d1b…8868` | 105 | `20260808090000` |
| `d6c43bc8c48784da8f0b701945b80b20911b3d1a` | 4.1.1 | `ea850d1b…8868` | 105 | `20260808090000` |

A store published under the 4.2.0 set therefore still verifies after the
upgrade, and the next import from a 4.4.0 source publishes a successor
snapshot over it. A store published under the 4.4.0 set likewise verifies
after the v4.5.0 pin. An unknown revision, or a known revision with another set's
hash, count or newest migration, is refused.
