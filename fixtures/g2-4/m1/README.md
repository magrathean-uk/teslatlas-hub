# G2-4 M1 floor fixtures

The original synthetic stores were built stepwise from the untouched source at `9f5e783`, before the schema collapse. The floor F is 78. The shipped `store-f.sqlite.zst` and `store-f-minus-one.sqlite.zst` are compressed `VACUUM INTO` copies of those builds. Tests decode them into private scratch directories and never open the committed files in place.

`schema-f.json` records the shipped floor store's raw `(type, name, table, SQL)` records in `sqlite_schema` rowid order, including automatic indexes. SQL text is not normalised. VACUUM preserves every raw schema definition and the relative order of explicit indexes and triggers, while grouping tables before explicit indexes. The baseline SQL is emitted in the shipped store's order. `table-counts.json` records the positive counts of all 82 application tables; `rows.sql` reproduces all 95 synthetic application rows and `sqlite_sequence` on a new baseline.

Vehicles ending in 1, 2 and 3 are live, imported and migrated respectively. The migrated vehicle retains its live source and an import alias. Its imported open charge 7 was continued with a live lifecycle sample. The charge watermark is 7 and the position boundary is `(19, 1760000000000)`. Current import literals are `source_db`, `source_db_position_v1` and `default`. Credential cells are unusable synthetic bytes.

Regenerate in a writable pre-collapse source copy at `9f5e783`, with these support files copied into it. Python 3.14 or later supplies the zstd encoder. Run from that copy's root with a fresh routed scratch directory:

```sh
python3 -B fixtures/g2-4/m1/build.py --attach "$PWD"
G2_4_M1_FIXTURE_SCRATCH="$TMPDIR/g2-4-m1-regenerate" cargo test --offline --locked --lib g2_4_m1_build_stepwise_fixtures -- --ignored --nocapture
python3 -B fixtures/g2-4/m1/build.py --populate "$TMPDIR/g2-4-m1-regenerate"
python3 -B fixtures/g2-4/m1/build.py --detach "$PWD"
```

The helper fills synthetic rows, vacuums each store, checks integrity and raw schema preservation, compresses both stores and writes the schema/count/replay files. It also emits `schema-floor.sql` in scratch; copy that file into `src/storage/db/schema_floor.sql` in the collapsed source copy. To repackage existing untouched-tree builds, put both raw stores into a fresh scratch directory and use `build.py --package DIRECTORY`.

Regeneration changes binary bytes: initialization generates `installation_id` and the identity tidy's `completed_at_ms`. Review and update the fixture digests and binary pins after regeneration. The detached Rust builder is test support for the pre-collapse tree; it is not compiled by the collapsed module tree.
