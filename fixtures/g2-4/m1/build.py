#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Regenerate synthetic floor fixtures using the stepwise source tree."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
from compression import zstd
import sqlite3

HERE = Path(__file__).resolve().parent
LIVE = '10000000-0000-4000-8000-000000000001'
SOURCE = '10000000-0000-4000-8000-000000000002'
VEHICLE = '20000000-0000-4000-8000-000000000002'
UUID = '30000000-0000-4000-8000-000000000001'


def populate(root):
    root = root.resolve()
    if not root.is_relative_to(Path(os.environ['TMPDIR']).resolve()):
        raise SystemExit('fixture databases must be under routed scratch')
    connection = sqlite3.connect(root / 'store-f.sqlite')
    connection.execute('PRAGMA foreign_keys=ON')
    schema = json.loads((root / 'schema-raw.json').read_text())
    tables = {name: sql for kind, name, _, sql in schema if kind == 'table' and name != 'sqlite_sequence'}
    pending = {name for name in tables if not connection.execute(f'SELECT count(*) FROM "{name}"').fetchone()[0]}
    while pending:
        progress = False
        for table in sorted(pending):
            sql = tables[table]
            foreign = connection.execute(f'PRAGMA foreign_key_list("{table}")').fetchall()
            if any(parent in pending for _, _, parent, *_ in foreign):
                continue
            columns = connection.execute(f'PRAGMA table_info("{table}")').fetchall()
            values = {}
            for _, col, typ, required, default, pk in columns:
                if default is not None:
                    value = connection.execute('SELECT ' + default).fetchone()[0]
                elif not required and not pk:
                    value = None
                elif typ == 'INTEGER':
                    value = 1
                elif typ == 'REAL':
                    value = 1.0
                elif typ == 'BLOB':
                    value = b'{}' if 'json' in col else b'a' * 32
                else:
                    value = '{}' if 'json' in col else 'synthetic'
                if value is not None:
                    choice = re.search(r'\b' + col + r'\s+IN\s*\(([^)]+)\)', sql, re.I)
                    literal = re.search(r'\b' + col + r"\s*=\s*'([^']+)'", sql)
                    exact_number = re.search(r'\b' + col + r'\s*=\s*(\d+)', sql)
                    length = re.search(r'length\(' + col + r'\)\s*=\s*(\d+)', sql)
                    minimum = re.search(r'\b' + col + r'\s*(?:>=|BETWEEN)\s*(\d+)', sql, re.I)
                    blob_length = re.search(r'length\(' + col + r'\)\s+BETWEEN\s+(\d+)', sql, re.I)
                    if typ == 'TEXT':
                        if col in ('vehicle_id',): value = VEHICLE
                        elif col == 'source_id': value = SOURCE
                        elif col.endswith('_id') or col.endswith('_generation'): value = UUID
                        if choice:
                            raw = choice.group(1).split(',')[0].strip()
                            value = raw.strip("'")
                        if literal and not choice: value = literal.group(1)
                        if length: value = 'a' * int(length.group(1))
                        if 'json' in col: value = '{}'
                    elif typ == 'INTEGER':
                        if choice: value = int(choice.group(1).split(',')[0].strip())
                        if minimum: value = max(value, int(minimum.group(1)))
                        if exact_number: value = int(exact_number.group(1))
                    elif typ == 'BLOB' and blob_length:
                        value = value.ljust(int(blob_length.group(1)), b'a')
                values[col] = value
            special = {
                'collector_auth_state': {'state': 'ready', 'fuse_consecutive': 0},
                'fleet_tokens': {'expires_at': 2, 'next_refresh_at': 1},
                'legacy_owner_tokens': {'expires_at': 2, 'next_refresh_at': 1},
                'paired_devices': {'expires_at_ms': 2, 'max_expires_at_ms': 3},
                'pairing_challenges': {'expires_at_ms': 2},
                'supervised_collector_lease': {'lease_until_ms': 2},
                'sync_deltas': {'to_sequence': 2},
                'sync_live_delta_spans': {'to_sequence': 2, 'to_revision': 1},
                'sync_retired_lineages': {'expires_at_ms': 2, 'manifest_json': b'{}'},
                'retained_physical_v3_admissions': {'expires_at_ms': 2},
                'terrain_tile_digests': {'tile_name': 'N01E001'},
                'current_observations': {'record_type': 'source_db_position_v1'},
                'raw_observations': {'vehicle_id': '20000000-0000-4000-8000-000000000003'},
                'import_projection_state_rows': {'entity_ordinal': 0},
            }
            values.update(special.get(table, {}))
            for group in sorted({row[0] for row in foreign}):
                keys = [row for row in foreign if row[0] == group]
                if any(values[row[3]] is None for row in keys):
                    continue
                parent = keys[0][2]
                targets = ','.join('"' + row[4] + '"' for row in keys)
                where = ' AND '.join('\"' + row[4] + '\"=?' for row in keys)
                match = connection.execute(f'SELECT {targets} FROM \"{parent}\" WHERE {where}', [values[row[3]] for row in keys]).fetchone()
                if match is None:
                    match = connection.execute(f'SELECT {targets} FROM \"{parent}\" LIMIT 1').fetchone()
                if match is None:
                    raise RuntimeError(f'empty parent {parent} for {table}')
                for row, value in zip(keys, match): values[row[3]] = value
            names = ','.join('"' + col + '"' for col in values)
            marks = ','.join('?' for _ in values)
            try:
                connection.execute(f'INSERT INTO "{table}" ({names}) VALUES ({marks})', list(values.values()))
            except sqlite3.Error as error:
                raise RuntimeError(f'{table}: {error}; values={values}') from error
            pending.remove(table)
            progress = True
        if not progress: raise RuntimeError(f'unresolved table dependencies: {sorted(pending)}')
    assert not connection.execute('PRAGMA foreign_key_check').fetchall()
    assert connection.execute('PRAGMA integrity_check').fetchone() == ('ok',)
    counts = {table: connection.execute(f'SELECT count(*) FROM "{table}"').fetchone()[0] for table in sorted(tables)}
    assert all(counts.values())
    connection.commit()
    inserts = [line for line in connection.iterdump() if line.startswith('INSERT INTO') or line.startswith('DELETE FROM "sqlite_sequence"')]
    inserts.sort(key=lambda line: line.startswith('INSERT INTO \"raw_observations\"'))
    (HERE / 'rows.sql').write_text('-- SPDX-License-Identifier: AGPL-3.0-only\nPRAGMA foreign_keys=OFF;\nBEGIN;\nDELETE FROM terrain_tidy_state;\n' + '\n'.join(inserts) + '\nCOMMIT;\nPRAGMA foreign_keys=ON;\n')
    connection.execute('PRAGMA wal_checkpoint(TRUNCATE)')
    connection.close()
    (HERE / 'table-counts.json').write_text(json.dumps(counts, indent=2) + '\n')
    package(root)


def package(root):
    root = root.resolve()
    if not root.is_relative_to(Path(os.environ['TMPDIR']).resolve()):
        raise SystemExit('fixture databases must be under routed scratch')
    records = []
    for name in ['store-f.sqlite', 'store-f-minus-one.sqlite']:
        source = root / name
        compact = root / ('vacuum-' + name)
        if compact.exists():
            raise SystemExit('use a fresh scratch directory for packaging')
        connection = sqlite3.connect(source)
        before = connection.execute('SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY rowid').fetchall()
        connection.execute('VACUUM INTO ?', (str(compact),))
        connection.close()
        connection = sqlite3.connect(compact)
        schema = connection.execute('SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY rowid').fetchall()
        assert sorted(schema, key=lambda row: (row[0], row[1])) == sorted(before, key=lambda row: (row[0], row[1]))
        for kind in ['index', 'trigger']:
            assert [row for row in schema if row[0] == kind and row[3] is not None] == [row for row in before if row[0] == kind and row[3] is not None]
        assert connection.execute('PRAGMA integrity_check').fetchone() == ('ok',)
        assert connection.execute('PRAGMA freelist_count').fetchone() == (0,)
        if name == 'store-f.sqlite':
            (HERE / 'schema-f.json').write_text(json.dumps(schema, indent=2) + '\n')
            sql = '-- SPDX-License-Identifier: AGPL-3.0-only\n\n' + '\n\n'.join(text + ';' for kind, object_name, _, text in schema if text and object_name != 'sqlite_sequence')
            sql += "\n\nINSERT INTO terrain_tidy_state(step, resume_vehicle_id, resume_position_id, examined, cleared, requeued, removed, kept, drives_changed, completed_at_ms) VALUES ('earlier_build_values_v1', '', 0, 0, 0, 0, 0, 0, 0, NULL);\n"
            (root / 'schema-floor.sql').write_text(sql)
        connection.close()
        destination = HERE / (name + '.zst')
        destination.write_bytes(zstd.compress(compact.read_bytes(), level=19))
        records.append(dict(path=destination.name, bytes=destination.stat().st_size, sha256=hashlib.sha256(destination.read_bytes()).hexdigest(), decoded_sha256=hashlib.sha256(compact.read_bytes()).hexdigest()))
    print(json.dumps(dict(fixtures=records)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--populate', type=Path)
    parser.add_argument('--package', type=Path)
    parser.add_argument('--attach', type=Path)
    parser.add_argument('--detach', type=Path)
    args = parser.parse_args()
    if args.populate: populate(args.populate)
    if args.package: package(args.package)
    line = '\n#[path = "../../../fixtures/g2-4/m1/builder.rs"]\nmod g2_4_m1_fixture_builder;\n'
    for repo, attach in [(args.attach, True), (args.detach, False)]:
        if repo:
            path = repo / 'src/storage/db/tests.rs'
            source = path.read_text()
            if attach:
                if 'fn migrate_to_previous_import_names' not in (repo / 'src/storage/db/migrations.rs').read_text():
                    raise SystemExit('fixture builder requires the untouched stepwise tree at 9f5e783')
                if line not in source: path.write_text(source + line)
            else: path.write_text(source.replace(line, ''))


if __name__ == '__main__': main()
