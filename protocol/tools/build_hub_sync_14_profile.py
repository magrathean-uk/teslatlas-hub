#!/usr/bin/env python3
"""Build the additive, source-neutral PhysicalV3 changed-set profile."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import sqlite3
import tempfile

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
import zstandard

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.4.0"
PREVIOUS = ROOT / "profiles/hub-sync-v1/1.3.0/SHA256SUMS"
FORMAT = "teslatlas-physical-v3-delta-v1"
FULL_REPLACEMENT_MAX_CHUNKS = 1771  # Established schema-2.2 full manifest bound.
SEED = bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
MAX_INTEGER = 9007199254740991
UUID = {"type": "string", "format": "uuid", "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"}
OPAQUE = {"type": "string", "minLength": 1, "maxLength": 4096, "pattern": "^[A-Za-z0-9._~-]+$"}
HEX = {"type": "string", "pattern": "^[0-9a-f]{64}$"}
POSITIVE = {"type": "integer", "minimum": 1, "maximum": MAX_INTEGER}
NATURAL = {"type": "integer", "minimum": 0, "maximum": MAX_INTEGER}
SCHEMA = "https://json-schema.org/draft/2020-12/schema"
TABLES = ("global_settings", "car_settings", "cars", "drives", "positions", "charging_processes", "charges", "addresses", "geofences", "states", "updates")
PROJECTED = ("cars", "car_settings", "drives", "positions", "charges", "charge_samples", "car_states", "car_updates")


def obj(properties, required=None):
    return {"type": "object", "additionalProperties": False, "properties": properties,
            "required": required or list(properties)}


def schema(name, definition):
    return {"$schema": SCHEMA, "$id": f"urn:teslatlas:hub-sync-v1:1.4.0:{name}",
            "$defs": {name: definition}}


def put(path, value, check):
    raw = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode() if not isinstance(value, bytes) else value
    if check:
        if not path.exists() or path.read_bytes() != raw:
            raise SystemExit(f"outdated 1.4 profile artifact: {path.relative_to(ROOT)}")
    else:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(raw)


def catalog():
    value = json.loads((PROFILE / "physical-field-catalog.json").read_text())
    assert value["format"] == FORMAT and value["mapped_source_fields"] == 168
    assert tuple(table["name"] for table in value["tables"]) == TABLES
    assert sum(len(table["columns"]) for table in value["tables"]) == 205
    assert sum(table["mapped_source_fields"] for table in value["tables"]) == 168
    for table in value["tables"]:
        assert table["columns"][0] == {"name": "id", "sqlite_type": "INTEGER", "nullable": False, "primary_key": True}
        assert len({column["name"] for column in table["columns"]}) == len(table["columns"])
    return value


def pack_sql(value):
    lines = ["-- Source-neutral PhysicalV3 changed-set pack format v1.",
             "-- The field catalog defines full source rows; this schema adds operation and closure evidence.",
             "CREATE TABLE delta_metadata (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT, WITHOUT ROWID;"]
    for table in value["tables"]:
        columns = []
        for column in table["columns"]:
            part = f'{column["name"]} {column["sqlite_type"]}'
            if column["primary_key"]:
                part += " PRIMARY KEY"
            if not column["nullable"]:
                part += " NOT NULL"
            columns.append(part)
        lines.append(f'CREATE TABLE {table["name"]} (\n    ' + ",\n    ".join(columns) + "\n) STRICT, WITHOUT ROWID;")
    allowed = ", ".join(f"'{name}'" for name in TABLES)
    projected = ", ".join(f"'{name}'" for name in PROJECTED)
    lines.extend([
        f"CREATE TABLE row_roles (table_name TEXT NOT NULL CHECK(table_name IN ({allowed})), entity_id INTEGER NOT NULL, role TEXT NOT NULL CHECK(role IN ('changed', 'context')), PRIMARY KEY(table_name, entity_id)) STRICT, WITHOUT ROWID;",
        f"CREATE TABLE tombstones (table_name TEXT NOT NULL CHECK(table_name IN ({allowed})), entity_id INTEGER NOT NULL, PRIMARY KEY(table_name, entity_id)) STRICT, WITHOUT ROWID;",
        f"CREATE TABLE affected_projected_ids (table_name TEXT NOT NULL CHECK(table_name IN ({projected})), entity_id INTEGER NOT NULL, effect TEXT NOT NULL CHECK(effect IN ('recompute', 'delete')), PRIMARY KEY(table_name, entity_id)) STRICT, WITHOUT ROWID;",
        "CREATE TABLE impacted_roots (root_type TEXT NOT NULL CHECK(root_type IN ('drive', 'charge')), root_id INTEGER NOT NULL, base_state TEXT NOT NULL CHECK(base_state IN ('absent', 'open', 'closed')), target_state TEXT NOT NULL CHECK(target_state IN ('absent', 'open', 'closed')), target_child_count INTEGER NOT NULL CHECK(target_child_count >= 0), PRIMARY KEY(root_type, root_id)) STRICT, WITHOUT ROWID;",
    ])
    return ("\n\n".join(lines) + "\n").encode()


def control_schemas():
    ref = obj({"object_name": {"type": "string", "pattern": "^[a-z0-9][a-z0-9._/-]{0,1023}$"},
               "sha256": HEX, "compressed_bytes": {"type": "integer", "minimum": 1, "maximum": 16777216}})
    checkpoint = obj({"manifest_id": OPAQUE, "receipt_id": OPAQUE, "manifest_sha256": HEX,
                      "sequence": POSITIVE, "schema_version": {"const": "2.2"}})
    source = obj({"installation_id": UUID, "account_id": UUID, "vehicle_id": UUID,
                  "generation": NATURAL, "selected_car_id": POSITIVE})
    signature = obj({"algorithm": {"const": "ed25519"},
                     "key_id": {"type": "string", "pattern": "^ed25519-sha256-[0-9a-f]{64}$"},
                     "signed_payload_sha256": HEX,
                     "signature": {"type": "string", "pattern": "^[A-Za-z0-9+/]{86}==$"}})
    request = obj({"source": source, "base_receipt_id": OPAQUE, "base_manifest_id": OPAQUE,
                   "base_manifest_sha256": HEX, "base_manifest_schema": {"const": "2.2"},
                   "from_sequence": POSITIVE,
                   "schema_version_range": obj({"minimum": {"const": "2.2"}, "maximum": {"const": "2.2"}}),
                   "accepted_changed_set_formats": {"type": "array", "prefixItems": [{"const": FORMAT}], "minItems": 1, "maxItems": 1}})
    receipt = obj({"kind": {"const": "physical_changed_set"}, "payload_format": {"const": FORMAT},
                   "vehicle_id": UUID, "source": source, "base": checkpoint, "target": checkpoint,
                   "target_raw_sha256": HEX,
                   "chunks": {"type": "array", "minItems": 1, "maxItems": 64,
                              "items": obj({"chunk_index": {"type": "integer", "minimum": 0, "maximum": 63}, "pack": ref})},
                   "total_compressed_bytes": {"type": "integer", "minimum": 1, "maximum": 268435456},
                   "total_uncompressed_bytes": {"type": "integer", "minimum": 1, "maximum": 2147483648},
                   "total_rows": {"type": "integer", "minimum": 1, "maximum": 2000000},
                   "affected_projected_ids_sha256": HEX,
                   "affected_projected_ids_count": {"type": "integer", "minimum": 0, "maximum": 2000000},
                   "impacted_roots_sha256": HEX,
                   "impacted_roots_count": {"type": "integer", "minimum": 0, "maximum": 10000},
                   "affected_months": {"type": "array", "maxItems": 120, "uniqueItems": True,
                                       "items": {"type": "string", "pattern": "^[0-9]{4}-(0[1-9]|1[0-2])$"}},
                   "signature": signature})
    full_chunks = {"type": "array", "minItems": 1, "maxItems": FULL_REPLACEMENT_MAX_CHUNKS,
                   "items": obj({"chunk_index": {"type": "integer", "minimum": 0,
                                                  "maximum": FULL_REPLACEMENT_MAX_CHUNKS - 1},
                                 "pack": ref})}
    replacement = obj({**checkpoint["properties"], "chunks": full_chunks})
    rebase = obj({"kind": {"const": "rebase_required"}, "vehicle_id": UUID,
                  "requested_base": checkpoint, "reason": {"const": "compacted"},
                  "replacement": replacement, "retry_request": request,
                  "signature": signature})
    return {"changes-since-request.schema.json": schema("request", request),
            "physical-changed-set-receipt.schema.json": schema("receipt", receipt),
            "physical-rebase-hint.schema.json": schema("hint", rebase)}


def fixture_pack(sql, base, target, source):
    with tempfile.TemporaryDirectory() as temp:
        path = Path(temp) / "delta.sqlite"
        connection = sqlite3.connect(path)
        connection.execute("PRAGMA page_size=4096")
        connection.execute("PRAGMA journal_mode=OFF")
        connection.execute("PRAGMA secure_delete=ON")
        connection.executescript(sql.decode())
        metadata = {
            "payload_format": FORMAT,
            "base_manifest_id": base["manifest_id"],
            "base_receipt_id": base["receipt_id"],
            "base_manifest_sha256": base["manifest_sha256"],
            "base_sequence": str(base["sequence"]),
            "target_manifest_id": target["manifest_id"],
            "target_receipt_id": target["receipt_id"],
            "target_manifest_sha256": target["manifest_sha256"],
            "target_sequence": str(target["sequence"]),
            "target_raw_sha256": "4" * 64,
            **{key: str(value) for key, value in source.items()},
        }
        connection.executemany("INSERT INTO delta_metadata VALUES (?,?)", sorted(metadata.items()))
        rows = [("drives", 10, "changed"), ("positions", 20, "context"),
                ("charging_processes", 30, "changed"), ("charges", 40, "context"),
                ("charges", 41, "context")]
        for table, entity_id, role in rows:
            columns = [row[1] for row in connection.execute(f"PRAGMA table_xinfo('{table}')")]
            values = []
            for name in columns:
                if name == "id":
                    values.append(entity_id)
                elif name == "car_id":
                    values.append(1)
                elif name == "charging_process_id":
                    values.append(30)
                elif name == "drive_id":
                    values.append(10)
                elif name.endswith("_is_nan"):
                    values.append(0)
                elif name == "end_date_pg_us" and table == "charging_processes":
                    values.append(None)
                elif name in ("date_pg_us", "start_date_pg_us", "end_date_pg_us"):
                    values.append(1000000)
                else:
                    spec = next(c for t in catalog()["tables"] if t["name"] == table for c in t["columns"] if c["name"] == name)
                    values.append(None if spec["nullable"] else ("x" if spec["sqlite_type"] == "TEXT" else 0))
            connection.execute(f"INSERT INTO {table} ({','.join(columns)}) VALUES ({','.join('?' for _ in columns)})", values)
            connection.execute("INSERT INTO row_roles VALUES (?,?,?)", (table, entity_id, role))
        witness = [("drives", 10, "recompute"), ("positions", 20, "recompute"),
                   ("charges", 30, "recompute"), ("charge_samples", 40, "recompute"),
                   ("charge_samples", 41, "recompute"), ("car_states", 50, "delete")]
        connection.execute("INSERT INTO tombstones VALUES ('states', 50)")
        connection.executemany("INSERT INTO affected_projected_ids VALUES (?,?,?)", witness)
        roots = [("drive", 10, "closed", "closed", 1), ("charge", 30, "absent", "open", 2)]
        connection.executemany("INSERT INTO impacted_roots VALUES (?,?,?,?,?)", roots)
        connection.commit()
        connection.execute("VACUUM")
        connection.close()
        raw = path.read_bytes()
    return zstandard.ZstdCompressor(level=4).compress(raw), witness, roots


def lines_sha(rows):
    return hashlib.sha256("".join("\t".join(map(str, row)) + "\n" for row in sorted(rows)).encode("ascii")).hexdigest()


def signed(payload):
    data = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    key = Ed25519PrivateKey.from_private_bytes(SEED)
    import cryptography.hazmat.primitives.serialization as serialization
    public = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    result = dict(payload)
    result["signature"] = {"algorithm": "ed25519", "key_id": "ed25519-sha256-" + hashlib.sha256(public).hexdigest(),
                           "signed_payload_sha256": hashlib.sha256(data).hexdigest(),
                           "signature": base64.b64encode(key.sign(data)).decode()}
    return result


def json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def modified_pack(compressed, statement):
    raw = zstandard.ZstdDecompressor().decompress(compressed, max_output_size=256 * 1024 * 1024)
    with tempfile.TemporaryDirectory() as temp:
        path = Path(temp) / "mutated.sqlite"
        path.write_bytes(raw)
        connection = sqlite3.connect(path)
        connection.executescript(statement)
        connection.commit()
        connection.execute("VACUUM")
        connection.close()
        return zstandard.ZstdCompressor(level=4).compress(path.read_bytes())


def receipt_for_pack(original, compressed, name):
    unsigned = {key: value for key, value in original.items() if key != "signature"}
    unsigned["chunks"] = [{"chunk_index": 0, "pack": {
        "object_name": name, "sha256": hashlib.sha256(compressed).hexdigest(),
        "compressed_bytes": len(compressed)}}]
    unsigned["total_compressed_bytes"] = len(compressed)
    unsigned["total_uncompressed_bytes"] = len(zstandard.ZstdDecompressor().decompress(compressed))
    return signed(unsigned)


def build(check):
    catalog_value = catalog()
    sql = pack_sql(catalog_value)
    previous_sha = hashlib.sha256(PREVIOUS.read_bytes()).hexdigest()
    profile = {
        "profile_id": "hub-sync-v1@1.4.0", "status": "candidate", "previous_profile": "hub-sync-v1@1.3.0",
        "previous_profile_sha256": previous_sha, "snapshot_schema": "2.2", "changed_set_format": FORMAT,
        "bootstrap_selector": {"header": "x-teslatlas-sync-profile", "value": "hub-sync-v1@1.4.0",
                               "required_schema_header": "x-teslatlas-supported-schemas", "required_schema_value": "2.1,2.2"},
        "bootstrap_rule": "The 1.4 selector succeeds only with a signed schema 2.2 snapshot. If no schema 2.2 PhysicalV3 head is available, return an empty 406 no-store. The 2.1 and 2.2 header spelling is retained for selector compatibility; it does not permit a 2.1 base for this changed-set extension.",
        "checkpoint_resolution": "The Hub verifies base manifest ID, exact signed-manifest byte SHA-256, receipt, sequence and source binding before any signed no-op or changed set. Unknown or mismatched bases fail closed. Only the immediate predecessor may receive physical_changed_set; an older retained base receives a signed 409 complete replacement.",
        "compatibility": "A 1.3 client cannot receive physical_changed_set. A 1.4 client must advertise the exact changed-set format in each request. Both profiles keep signed no-op and 409 full-replacement semantics; 1.4 rebase carries an exact 1.4 retry request. Schema 2.2 snapshots are unchanged.",
        "first_delta_base": "immediate_predecessor_only", "fallback": "signed_409_complete_schema_2_2_replacement",
        "max_delta_chunks": 64, "max_pack_compressed_bytes": 16777216,
        "max_delta_compressed_bytes": 268435456, "max_delta_uncompressed_bytes": 2147483648,
        "max_delta_rows": 2000000, "max_impacted_roots": 10000,
        "field_catalog": "physical-field-catalog.json", "value_semantics": "physical-value-semantics.json",
        "pack_contract": "physical-delta-pack-v1-contract.json",
        "pack_sql": "physical-delta-pack-v1.sql", "signature": "Ed25519 over RFC 8785 canonical JSON without signature",
    }
    contract = {
        "format": FORMAT, "media_type": "application/vnd.teslatlas.physical-delta+sqlite+zstd;version=1",
        "field_catalog": "physical-field-catalog.json", "value_semantics": "physical-value-semantics.json",
        "sql": "physical-delta-pack-v1.sql",
        "full_row_rule": "All 205 wire columns representing the 168 mapped source fields are present for every changed/context row; absent columns, partial updates, and addresses.raw are forbidden.",
        "row_roles": {"changed": "full target row replacing or adding same typed ID", "context": "unchanged complete target row used only to close an impacted root; its presence never implies a source edit"},
        "tombstones": "Typed raw deletion is explicit. An omitted raw ID means unchanged. Upsert/tombstone overlap and duplicate IDs across chunks are invalid. Deleting or opening a root requires recompute intents for surviving children that detach or change; tombstones are only for rows actually absent from the target source.",
        "closure": "For each impacted drive or charging process include complete target root when present, every target position retaining its soft drive_id or every target charge sample with its required process parent, all needed address/geofence/position soft parents, and prior and target parent IDs for changed links. Mark unchanged rows context. Root child counts equal target raw references across all chunks. An absent drive has no raw root row but may retain surviving positions with drive_id set; include them and sign recompute intents so the reader detaches them. An absent charging process has zero surviving charge samples because that parent edge is required. Open/closed transitions require a root witness even when the target is omitted by reader policy.",
        "affected_projected_ids": {"tables": list(PROJECTED), "effects": ["recompute", "delete"], "digest": "SHA-256 of ASCII table_name TAB decimal_entity_id TAB effect LF, sorted by table_name then numeric entity_id", "rule": "Exact declared projection recomputation/write scope, not a claim that every value differs. The scope may conservatively include unchanged dependents. recompute means derive an eligible target row from the signed target closure, or remove it when the public projection policy omits it (for example an open drive); delete means the target raw row is absent. The App MUST keep delta-driven writes inside this scope, plus separately bounded local retention/access-window updates, and verify each affected root and relation is covered. Context rows alone never authorize writes."},
        "affected_scope_derivation": "Derive from both old and new raw relation edges: changed positions affect their row, old/new drive owners and drives/processes using them as endpoints; changed drives include old/new linked positions; drive deletion/opening detaches surviving positions; address/geofence changes affect old/new referring drives/processes; process/sample changes affect old/new sessions and all target charge samples; car/settings/updates affect singleton car and latest firmware; states/updates directly affect their projected IDs. Global settings changes are transported without a direct projected row.",
        "relation_edges": [
            {"from": "cars.settings_id", "to": "car_settings.id", "kind": "required"},
            {"from": "drives.car_id", "to": "cars.id", "kind": "required"},
            {"from": "positions.car_id", "to": "cars.id", "kind": "required"},
            {"from": "positions.drive_id", "to": "drives.id", "kind": "soft"},
            {"from": "charging_processes.car_id", "to": "cars.id", "kind": "required"},
            {"from": "charging_processes.position_id", "to": "positions.id", "kind": "soft"},
            {"from": "charges.charging_process_id", "to": "charging_processes.id", "kind": "required"},
            *[{"from": f"drives.{field}", "to": f"{target}.id", "kind": "soft"} for field, target in
              (("start_position_id", "positions"), ("end_position_id", "positions"),
               ("start_address_id", "addresses"), ("end_address_id", "addresses"),
               ("start_geofence_id", "geofences"), ("end_geofence_id", "geofences"))],
            *[{"from": f"charging_processes.{field}", "to": f"{target}.id", "kind": "soft"} for field, target in
              (("address_id", "addresses"), ("geofence_id", "geofences"))],
            {"from": "states.car_id", "to": "cars.id", "kind": "required"},
            {"from": "updates.car_id", "to": "cars.id", "kind": "required"},
        ],
        "impacted_roots_digest": "SHA-256 of ASCII root_type TAB decimal_root_id TAB base_state TAB target_state TAB decimal_target_child_count LF, sorted by root_type then numeric root_id",
        "target_raw_sha256": "Hub-signed all-history target commitment. A retained-window App candidate is not required to reproduce it; candidate digest and access-window availability are separate local evidence.",
        "aggregate_admission": "Before fetching, reject a receipt exceeding 64 contiguous packs, 256 MiB total compressed, 2 GiB total uncompressed, 2000000 unique typed rows/tombstones (context counted once) or 10000 impacted roots. Each pack remains at most 16 MiB compressed and 256 MiB uncompressed. Hub returns a signed 409 complete replacement when a valid changed set cannot fit; App never increases limits locally.",
        "apply": "Verify signature, vehicle-bound key, exact base and target signed-manifest hashes, source binding, contiguous pack hashes, complete rows, tombstones, closure and exact witness. Apply to a private retained-window candidate and atomically activate checkpoint and candidate. Replay of an already activated target is idempotent; interruption preserves the previous readable checkpoint.",
        "access_windows": "Free 30-day projection may discard older raw and projected rows. A later Full 365-day widening uses signed full-head rehydrate. If the required parent/child closure is missing or exceeds limits, return signed 409 complete replacement; never silently invent context.",
        "map_identity": "Affected months include old and new UTC months touched by a changed drive, position or relation. Prepared months bind target manifest ID, receipt ID, sequence and Compute algorithm identity; stale base-head preparations are rejected.",
    }
    files = {"profile.json": profile, "physical-delta-pack-v1-contract.json": contract,
             "physical-delta-pack-v1.sql": sql, **control_schemas()}
    files["physical-value-semantics.json"] = {
        "integer": "SQLite signed 64-bit INTEGER; JSON control members remain within the profile's I-JSON safe range",
        "text": "UTF-8 TEXT with explicit NULL for absent source values",
        "timestamp_pg_us": "Signed microseconds from 2000-01-01T00:00:00 UTC; suffix _pg_us",
        "fixed_decimal": {"_e1": "scaled by 10", "_e2": "scaled by 100", "_e4": "scaled by 10000", "_e6": "scaled by 1000000"},
        "nan_flags": "For a nullable numeric column x with companion x_is_nan, the flag is 0 or 1. A non-NULL finite x requires flag 0; flag 1 represents source NaN with x NULL. NULL plus flag 0 represents source NULL.",
        "float_bits_be": "Eight-byte big-endian IEEE 754 binary64 bit pattern, preserving signed zero and special values; suffix _f64_be or cars.efficiency",
        "nulls": "All catalog columns are present in every full row. SQLite NULL is permitted only where the catalog marks nullable.",
        "source_field_count": 168,
        "wire_column_count": 205,
        "excluded_source_field": "addresses.raw",
    }
    source = {"installation_id": "11111111-1111-4111-8111-111111111111",
              "account_id": "22222222-2222-4222-8222-222222222222",
              "vehicle_id": "33333333-3333-4333-8333-333333333333",
              "generation": 1, "selected_car_id": 1}
    checkpoints = []
    for sequence in (42, 43):
        snapshot = signed({"manifest_id": f"manifest_demo_{sequence:06}",
                           "receipt_id": f"receipt_demo_{sequence:06}",
                           "vehicle_id": source["vehicle_id"], "kind": "snapshot",
                           "schema_version": "2.2", "sequence": sequence,
                           "chunks": [{"chunk_index": 0, "pack": {"object_name": f"packs/demo-full-{sequence}.sqlite.zst",
                                                                  "sha256": f"{sequence % 10}" * 64,
                                                                  "compressed_bytes": 4096}}]})
        files[f"examples/signed-full-manifest-{sequence}.json"] = snapshot
        checkpoints.append({"manifest_id": snapshot["manifest_id"], "receipt_id": snapshot["receipt_id"],
                            "manifest_sha256": hashlib.sha256(json_bytes(snapshot)).hexdigest(),
                            "sequence": sequence, "schema_version": "2.2"})
    base, target = checkpoints
    pack, witness, roots = fixture_pack(sql, base, target, source)
    request = {"source": source, "base_receipt_id": base["receipt_id"], "base_manifest_id": base["manifest_id"],
               "base_manifest_sha256": base["manifest_sha256"], "base_manifest_schema": "2.2",
               "from_sequence": 42, "schema_version_range": {"minimum": "2.2", "maximum": "2.2"},
               "accepted_changed_set_formats": [FORMAT]}
    receipt = signed({"kind": "physical_changed_set", "payload_format": FORMAT,
                      "vehicle_id": source["vehicle_id"], "source": source,
                      "base": base, "target": target, "target_raw_sha256": "4" * 64,
                      "chunks": [{"chunk_index": 0, "pack": {"object_name": "packs/physical-changed-set-v1.sqlite.zst",
                                                              "sha256": hashlib.sha256(pack).hexdigest(),
                                                              "compressed_bytes": len(pack)}}],
                      "total_compressed_bytes": len(pack),
                      "total_uncompressed_bytes": len(zstandard.ZstdDecompressor().decompress(pack)),
                      "total_rows": 6,
                      "affected_projected_ids_sha256": lines_sha(witness),
                      "affected_projected_ids_count": len(witness),
                      "impacted_roots_sha256": lines_sha(roots), "impacted_roots_count": len(roots),
                      "affected_months": ["2026-08", "2026-09"]})
    files["examples/changes-since-request-physical.json"] = {"fixture_id": "changes-since-request-physical-v1", "request": request}
    files["examples/changes-since-physical-changed-set.json"] = {"fixture_id": "changes-since-physical-changed-set-v1", "receipt": receipt}
    retry_request = {**request, "base_receipt_id": target["receipt_id"],
                     "base_manifest_id": target["manifest_id"],
                     "base_manifest_sha256": target["manifest_sha256"],
                     "from_sequence": target["sequence"]}
    rebase = signed({"kind": "rebase_required", "vehicle_id": source["vehicle_id"],
                     "requested_base": base, "reason": "compacted",
                     "replacement": {**target, "chunks": files["examples/signed-full-manifest-43.json"]["chunks"]},
                     "retry_request": retry_request})
    files["examples/changes-since-physical-rebase.json"] = {
        "fixture_id": "changes-since-physical-rebase-v1", "hint": rebase}
    large_manifest = signed({"manifest_id": "manifest_demo_000043_large",
                             "receipt_id": "receipt_demo_000043_large",
                             "vehicle_id": source["vehicle_id"], "kind": "snapshot",
                             "schema_version": "2.2", "sequence": 43,
                             "chunks": [{"chunk_index": index, "pack": {
                                 "object_name": f"packs/demo-full-43-large-{index}.sqlite.zst",
                                 "sha256": f"{index + 1:064x}", "compressed_bytes": 4096}}
                                 for index in range(65)]})
    files["examples/signed-full-manifest-43-large.json"] = large_manifest
    large_target = {"manifest_id": large_manifest["manifest_id"],
                    "receipt_id": large_manifest["receipt_id"],
                    "manifest_sha256": hashlib.sha256(json_bytes(large_manifest)).hexdigest(),
                    "sequence": 43, "schema_version": "2.2"}
    large_retry = {**request, "base_receipt_id": large_target["receipt_id"],
                   "base_manifest_id": large_target["manifest_id"],
                   "base_manifest_sha256": large_target["manifest_sha256"],
                   "from_sequence": large_target["sequence"]}
    files["examples/changes-since-physical-rebase-large.json"] = {
        "fixture_id": "changes-since-physical-rebase-large-v1",
        "hint": signed({"kind": "rebase_required", "vehicle_id": source["vehicle_id"],
                        "requested_base": base, "reason": "compacted",
                        "replacement": {**large_target, "chunks": large_manifest["chunks"]},
                        "retry_request": large_retry})}
    files["packs/physical-changed-set-v1.sqlite.zst"] = pack
    missing_closure = modified_pack(pack, "DELETE FROM charges WHERE id=41; DELETE FROM row_roles WHERE table_name='charges' AND entity_id=41;")
    files["packs/negative/physical-missing-closure.sqlite.zst"] = missing_closure
    missing_receipt = receipt_for_pack(receipt, missing_closure, "packs/negative/physical-missing-closure.sqlite.zst")
    missing_unsigned = {key: value for key, value in missing_receipt.items() if key != "signature"}
    missing_unsigned["total_rows"] = 5
    files["examples/changes-since-physical-missing-closure.json"] = {
        "fixture_id": "changes-since-physical-missing-closure-v1",
        "receipt": signed(missing_unsigned)}
    wrong_binding = modified_pack(pack, "UPDATE delta_metadata SET value='44444444-4444-4444-8444-444444444444' WHERE key='vehicle_id'")
    files["packs/negative/physical-wrong-binding.sqlite.zst"] = wrong_binding
    files["examples/changes-since-physical-wrong-binding.json"] = {
        "fixture_id": "changes-since-physical-wrong-binding-v1",
        "receipt": receipt_for_pack(receipt, wrong_binding, "packs/negative/physical-wrong-binding.sqlite.zst")}
    over_limit = {key: value for key, value in receipt.items() if key != "signature"}
    over_limit["chunks"] = [{"chunk_index": 0, "pack": {**receipt["chunks"][0]["pack"], "compressed_bytes": 16777217}}]
    files["examples/changes-since-physical-over-limit.json"] = {
        "fixture_id": "changes-since-physical-over-limit-v1", "receipt": signed(over_limit)}
    wrong_vehicle = {key: value for key, value in receipt.items() if key != "signature"}
    wrong_vehicle["source"] = {**receipt["source"], "vehicle_id": "44444444-4444-4444-8444-444444444444"}
    files["examples/changes-since-physical-wrong-source.json"] = {
        "fixture_id": "changes-since-physical-wrong-source-v1", "receipt": signed(wrong_vehicle)}
    wrong_generation = {key: value for key, value in receipt.items() if key != "signature"}
    wrong_generation["source"] = {**receipt["source"], "generation": 2}
    files["examples/changes-since-physical-wrong-generation.json"] = {
        "fixture_id": "changes-since-physical-wrong-generation-v1", "receipt": signed(wrong_generation)}
    wrong_open = modified_pack(pack, "UPDATE charging_processes SET end_date_pg_us=1000000 WHERE id=30")
    files["packs/negative/physical-wrong-open-state.sqlite.zst"] = wrong_open
    wrong_open_receipt = receipt_for_pack(receipt, wrong_open, "packs/negative/physical-wrong-open-state.sqlite.zst")
    wrong_open_receipt_unsigned = {key: value for key, value in wrong_open_receipt.items() if key != "signature"}
    wrong_open_receipt_unsigned["total_uncompressed_bytes"] = len(zstandard.ZstdDecompressor().decompress(wrong_open))
    files["examples/changes-since-physical-wrong-open-state.json"] = {
        "fixture_id": "changes-since-physical-wrong-open-state-v1", "receipt": signed(wrong_open_receipt_unsigned)}
    absent_drive = modified_pack(pack, "DELETE FROM drives WHERE id=10; "
        "DELETE FROM row_roles WHERE table_name='drives' AND entity_id=10; "
        "INSERT INTO tombstones VALUES ('drives',10); "
        "UPDATE affected_projected_ids SET effect='delete' WHERE table_name='drives' AND entity_id=10; "
        "UPDATE impacted_roots SET target_state='absent' WHERE root_type='drive' AND root_id=10;")
    files["packs/physical-absent-drive-surviving-position.sqlite.zst"] = absent_drive
    absent_unsigned = {key: value for key, value in receipt_for_pack(
        receipt, absent_drive, "packs/physical-absent-drive-surviving-position.sqlite.zst").items()
        if key != "signature"}
    absent_witness = [("drives", 10, "delete"), ("positions", 20, "recompute"),
                      ("charges", 30, "recompute"), ("charge_samples", 40, "recompute"),
                      ("charge_samples", 41, "recompute"), ("car_states", 50, "delete")]
    absent_roots = [("drive", 10, "closed", "absent", 1),
                    ("charge", 30, "absent", "open", 2)]
    absent_unsigned["affected_projected_ids_sha256"] = lines_sha(absent_witness)
    absent_unsigned["impacted_roots_sha256"] = lines_sha(absent_roots)
    absent_receipt = signed(absent_unsigned)
    files["examples/changes-since-physical-absent-drive-surviving-position.json"] = {
        "fixture_id": "changes-since-physical-absent-drive-surviving-position-v1",
        "receipt": absent_receipt}
    absent_missing_child = modified_pack(absent_drive,
        "DELETE FROM positions WHERE id=20; DELETE FROM row_roles WHERE table_name='positions' AND entity_id=20;")
    files["packs/negative/physical-absent-drive-missing-surviving-position.sqlite.zst"] = absent_missing_child
    missing_child_unsigned = {key: value for key, value in receipt_for_pack(
        absent_receipt, absent_missing_child,
        "packs/negative/physical-absent-drive-missing-surviving-position.sqlite.zst").items()
        if key != "signature"}
    missing_child_unsigned["total_rows"] = 5
    files["examples/changes-since-physical-absent-drive-missing-surviving-position.json"] = {
        "fixture_id": "changes-since-physical-absent-drive-missing-surviving-position-v1",
        "receipt": signed(missing_child_unsigned)}
    absent_missing_scope = modified_pack(absent_drive,
        "DELETE FROM affected_projected_ids WHERE table_name='positions' AND entity_id=20;")
    files["packs/negative/physical-absent-drive-missing-detach-scope.sqlite.zst"] = absent_missing_scope
    missing_scope_unsigned = {key: value for key, value in receipt_for_pack(
        absent_receipt, absent_missing_scope,
        "packs/negative/physical-absent-drive-missing-detach-scope.sqlite.zst").items()
        if key != "signature"}
    missing_scope_unsigned["affected_projected_ids_count"] = 5
    missing_scope_unsigned["affected_projected_ids_sha256"] = lines_sha(
        [row for row in absent_witness if row[:2] != ("positions", 20)])
    files["examples/changes-since-physical-absent-drive-missing-detach-scope.json"] = {
        "fixture_id": "changes-since-physical-absent-drive-missing-detach-scope-v1",
        "receipt": signed(missing_scope_unsigned)}
    cases = {"cases": [
        {"id": "physical-changed-set", "request_fixture": "changes-since-request-physical-v1", "response_fixture": "changes-since-physical-changed-set-v1", "status": 200, "expected_errors": []},
        {"id": "physical-changed-set-unsupported-client", "status": 200, "expected_errors": ["capability not advertised"]},
        {"id": "physical-changed-set-wrong-base", "status": 200, "expected_errors": ["base checkpoint mismatch"]},
        {"id": "physical-changed-set-missing-closure", "response_fixture": "changes-since-physical-missing-closure-v1", "status": 200, "expected_errors": ["incomplete impacted root"]},
        {"id": "physical-changed-set-wrong-binding", "response_fixture": "changes-since-physical-wrong-binding-v1", "status": 200, "expected_errors": ["physical pack binding mismatch"]},
        {"id": "physical-changed-set-wrong-source", "response_fixture": "changes-since-physical-wrong-source-v1", "status": 200, "expected_errors": ["source vehicle mismatch"]},
        {"id": "physical-changed-set-wrong-generation", "response_fixture": "changes-since-physical-wrong-generation-v1", "status": 200, "expected_errors": ["source binding mismatch"]},
        {"id": "physical-changed-set-wrong-open-state", "response_fixture": "changes-since-physical-wrong-open-state-v1", "status": 200, "expected_errors": ["impacted root open/closed state mismatch"]},
        {"id": "physical-changed-set-over-limit", "response_fixture": "changes-since-physical-over-limit-v1", "status": 200, "expected_errors": ["receipt violates 1.4 schema"]},
        {"id": "physical-changed-set-tampered-pack", "status": 200, "expected_errors": ["pack digest mismatch"]},
        {"id": "physical-changed-set-compacted-base", "response_fixture": "changes-since-physical-rebase-v1", "status": 409, "expected_errors": []},
        {"id": "physical-changed-set-full-rebase-over-64", "response_fixture": "changes-since-physical-rebase-large-v1", "status": 409, "expected_errors": []},
        {"id": "physical-absent-drive-surviving-position", "response_fixture": "changes-since-physical-absent-drive-surviving-position-v1", "status": 200, "expected_errors": []},
        {"id": "physical-absent-drive-missing-surviving-position", "response_fixture": "changes-since-physical-absent-drive-missing-surviving-position-v1", "status": 200, "expected_errors": ["incomplete impacted root"]},
        {"id": "physical-absent-drive-missing-detach-scope", "response_fixture": "changes-since-physical-absent-drive-missing-detach-scope-v1", "status": 200, "expected_errors": ["missing surviving-child recomputation scope"]},
    ]}
    files["cases.json"] = cases
    openapi = {"openapi": "3.1.0", "info": {"title": "Teslatlas PhysicalV3 changed-set extension", "version": "1.4.0"},
               "paths": {"/v1/vehicles/{vehicle_id}/sync/changes-since": {"post": {
                   "operationId": "changesSincePhysicalV14", "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}],
                   "requestBody": {"required": True, "content": {"application/json": {"schema": {"$ref": "changes-since-request.schema.json#/$defs/request"}}}, "x-max-body-bytes": 8192},
                   "responses": {"200": {"description": "signed physical changed-set or inherited signed no-op", "content": {"application/json": {"schema": {"oneOf": [{"$ref": "physical-changed-set-receipt.schema.json#/$defs/receipt"}, {"$ref": "../1.3.0/noop.schema.json#/$defs/receipt"}]}}}},
                                 "409": {"description": "signed complete replacement with exact 1.4 retry request", "content": {"application/json": {"schema": {"$ref": "physical-rebase-hint.schema.json#/$defs/hint"}}}}}}}}}
    openapi["paths"]["/v1/vehicles/{vehicle_id}/sync/manifest"] = {"get": {
        "operationId": "bootstrapPhysicalV14",
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID},
                       {"name": "x-teslatlas-sync-profile", "in": "header", "required": True, "schema": {"const": "hub-sync-v1@1.4.0"}},
                       {"name": "x-teslatlas-supported-schemas", "in": "header", "required": True, "schema": {"const": "2.1,2.2"}}],
        "responses": {"200": {"description": "signed schema 2.2 PhysicalV3 full snapshot", "content": {"application/json": {"schema": {"$ref": "../1.3.0/sync-manifest.schema.json#/$defs/schema_2_2"}}}},
                      "406": {"description": "selector unavailable; empty body, Cache-Control: no-store"}}}}
    files["openapi.json"] = openapi
    for name, content in files.items():
        put(PROFILE / name, content, check)
    listed = sorted(["physical-field-catalog.json", *files])
    sums = "".join(hashlib.sha256((PROFILE / name).read_bytes()).hexdigest() + "  " + name + "\n" for name in listed).encode()
    put(PROFILE / "SHA256SUMS", sums, check)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    build(parser.parse_args().check)
