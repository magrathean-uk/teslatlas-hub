"""Independent admission checks for the additive PhysicalV3 delta profile."""
import base64
import hashlib
import io
import json
from pathlib import Path
import re
import sqlite3
import tempfile

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from jsonschema import Draft202012Validator, FormatChecker
import zstandard
from conformance.hub_sync import canonical_integral_value

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.4.0"
MAX_REQUEST_BYTES = 8192
MAX_CONTROL_BYTES = 2097152


class ContractError(ValueError):
    pass


def _strict_json(raw):
    def unique(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ContractError("duplicate JSON member")
            value[key] = item
        return value
    return json.loads(raw, object_pairs_hook=unique)


def read_json(path):
    return _strict_json(Path(path).read_bytes())


def verify_bundle(root=PROFILE):
    root = Path(root)
    listed = {}
    for line in (root / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        if not re.fullmatch("[0-9a-f]{64}", digest) or name in listed or ".." in Path(name).parts:
            raise ContractError("invalid profile manifest")
        if hashlib.sha256((root / name).read_bytes()).hexdigest() != digest:
            raise ContractError("profile file digest mismatch")
        listed[name] = digest
    actual = {str(path.relative_to(root)) for path in root.rglob("*") if path.is_file() and path.name != "SHA256SUMS"}
    if set(listed) != actual:
        raise ContractError("profile manifest is incomplete")
    profile = read_json(root / "profile.json")
    if profile["profile_id"] != "hub-sync-v1@1.4.0":
        raise ContractError("wrong profile")
    prior = ROOT / "profiles/hub-sync-v1/1.3.0/SHA256SUMS"
    if hashlib.sha256(prior.read_bytes()).hexdigest() != profile["previous_profile_sha256"]:
        raise ContractError("previous profile digest mismatch")
    return profile


def _schema(name, kind, root=PROFILE):
    source = read_json(Path(root) / name)
    return {**source, "$ref": "#/$defs/" + kind}


def validate_request(request, root=PROFILE):
    schema = _schema("changes-since-request.schema.json", "request", root)
    if list(Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(request)):
        raise ContractError("request violates 1.4 schema")
    _check_encoded_size(request, MAX_REQUEST_BYTES, "request")
    return request


def _check_encoded_size(value, limit, label):
    if len(json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode()) > limit:
        raise ContractError(f"{label} exceeds byte limit")


def _bounded_json(raw, limit, label):
    if len(raw.encode("utf-8") if isinstance(raw, str) else raw) > limit:
        raise ContractError(f"{label} exceeds byte limit")
    try:
        return _strict_json(raw)
    except (UnicodeDecodeError, ValueError) as exc:
        raise ContractError(f"{label} is not valid JSON") from exc


def validate_request_body(raw, root=PROFILE):
    return validate_request(_bounded_json(raw, MAX_REQUEST_BYTES, "request"), root)


def verify_receipt_body(request_raw, receipt_raw, route_vehicle_id, root=PROFILE):
    return verify_receipt(validate_request_body(request_raw, root),
                          _bounded_json(receipt_raw, MAX_CONTROL_BYTES, "receipt"), route_vehicle_id, root)


def verify_rebase_body(request_raw, hint_raw, route_vehicle_id, replacement_manifest_raw, root=PROFILE):
    return verify_rebase(validate_request_body(request_raw, root),
                         _bounded_json(hint_raw, MAX_CONTROL_BYTES, "rebase"), route_vehicle_id,
                         replacement_manifest_raw, root)


def _canonical_unsigned(receipt):
    value = dict(receipt)
    value.pop("signature", None)
    # The profile constrains signed members to the ASCII JCS subset.
    return json.dumps(canonical_integral_value(value), sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode()


def _verify_fixture_signature(value, vehicle_id, root=PROFILE, label="signature"):
    document = read_json(Path(root) / "fixture-signing-keys.json")
    if list(Draft202012Validator(_schema("signing-keys.schema.json", "document", root),
                                format_checker=FormatChecker()).iter_errors(document)):
        raise ContractError("fixture signing keys violate schema")
    if document["vehicle_id"] != vehicle_id:
        raise ContractError("fixture signing keys vehicle mismatch")
    seen = set()
    for item in document["keys"]:
        raw = base64.b64decode(item["public_key"], validate=True)
        if item["key_id"] != "ed25519-sha256-" + hashlib.sha256(raw).hexdigest() or item["key_id"] in seen:
            raise ContractError("fixture signing key identifier mismatch")
        seen.add(item["key_id"])
    signature = value["signature"]
    payload = _canonical_unsigned(value)
    if hashlib.sha256(payload).hexdigest() != signature["signed_payload_sha256"]:
        raise ContractError(f"{label} payload mismatch")
    selected = next((item for item in document["keys"] if item["key_id"] == signature["key_id"]), None)
    if selected is None:
        raise ContractError("unknown signing key")
    try:
        key = Ed25519PublicKey.from_public_bytes(base64.b64decode(selected["public_key"], validate=True))
        key.verify(base64.b64decode(signature["signature"], validate=True), payload)
    except Exception as exc:
        raise ContractError(f"{label} invalid") from exc


def verify_receipt(request, receipt, route_vehicle_id, root=PROFILE):
    validate_request(request, root)
    if list(Draft202012Validator(_schema("physical-changed-set-receipt.schema.json", "receipt", root),
                                  format_checker=FormatChecker()).iter_errors(receipt)):
        raise ContractError("receipt violates 1.4 schema")
    _check_encoded_size(receipt, MAX_CONTROL_BYTES, "receipt")
    if receipt["vehicle_id"] != route_vehicle_id or receipt["source"]["vehicle_id"] != route_vehicle_id:
        raise ContractError("source vehicle mismatch")
    if receipt["source"] != request["source"]:
        raise ContractError("source binding mismatch")
    base = receipt["base"]
    if (base["receipt_id"] != request["base_receipt_id"] or base["manifest_id"] != request["base_manifest_id"]
            or base["manifest_sha256"] != request["base_manifest_sha256"] or base["sequence"] != request["from_sequence"]):
        raise ContractError("base checkpoint mismatch")
    if receipt["target"]["sequence"] != base["sequence"] + 1:
        raise ContractError("target is not immediate successor")
    chunks = receipt["chunks"]
    if [entry["chunk_index"] for entry in chunks] != list(range(len(chunks))):
        raise ContractError("delta chunks are not contiguous")
    if sum(entry["pack"]["compressed_bytes"] for entry in chunks) != receipt["total_compressed_bytes"]:
        raise ContractError("aggregate compressed byte count mismatch")
    _verify_fixture_signature(receipt, route_vehicle_id, root)
    return receipt


def verify_signed_manifest_bindings(receipt, base_manifest_raw, target_manifest_raw, root=PROFILE):
    for label, raw in (("base", base_manifest_raw), ("target", target_manifest_raw)):
        value = _verify_replacement_manifest(raw, receipt["vehicle_id"], root)
        checkpoint = receipt[label]
        if hashlib.sha256(raw).hexdigest() != checkpoint["manifest_sha256"]:
            raise ContractError(f"{label} signed manifest digest mismatch")
        if (value["manifest_id"] != checkpoint["manifest_id"] or
                value["receipt_id"] != checkpoint["receipt_id"] or
                value["sequence"] != checkpoint["sequence"] or
                value["schema_version"] != checkpoint["schema_version"] or
                value["vehicle_id"] != receipt["vehicle_id"]):
            raise ContractError(f"{label} signed manifest identity mismatch")
    return True


def verify_rebase(request, hint, route_vehicle_id, replacement_manifest_raw, root=PROFILE):
    validate_request(request, root)
    if list(Draft202012Validator(_schema("physical-rebase-hint.schema.json", "hint", root),
                                  format_checker=FormatChecker()).iter_errors(hint)):
        raise ContractError("rebase violates 1.4 schema")
    _check_encoded_size(hint, MAX_CONTROL_BYTES, "rebase")
    validate_request(hint["retry_request"], root)
    if hint["vehicle_id"] != route_vehicle_id or hint["retry_request"]["source"] != request["source"]:
        raise ContractError("rebase source mismatch")
    base = hint["requested_base"]
    if any(base[key] != request[request_key] for key, request_key in
           (("manifest_id", "base_manifest_id"), ("receipt_id", "base_receipt_id"),
            ("manifest_sha256", "base_manifest_sha256"), ("sequence", "from_sequence"))):
        raise ContractError("rebase requested base mismatch")
    replacement = hint["replacement"]
    expected_retry = {**request, "base_manifest_id": replacement["manifest_id"],
                      "base_receipt_id": replacement["receipt_id"],
                      "base_manifest_sha256": replacement["manifest_sha256"],
                      "from_sequence": replacement["sequence"]}
    if hint["retry_request"] != expected_retry:
        raise ContractError("rebase retry request mismatch")
    if hashlib.sha256(replacement_manifest_raw).hexdigest() != replacement["manifest_sha256"]:
        raise ContractError("rebase replacement manifest digest mismatch")
    manifest = _verify_replacement_manifest(replacement_manifest_raw, route_vehicle_id, root)
    if any(manifest[key] != replacement[key] for key in ("manifest_id", "receipt_id", "sequence", "schema_version")) or manifest["chunks"] != replacement["chunks"]:
        raise ContractError("rebase replacement manifest mismatch")
    _verify_fixture_signature(hint, route_vehicle_id, root, "rebase signature")
    return True


def _verify_replacement_manifest(raw, vehicle_id, root=PROFILE):
    # Physical 1.4 inherits the complete signed schema-2.2 snapshot authority.
    previous = ROOT / "profiles/hub-sync-v1/1.3.0"
    manifest = _bounded_json(raw, MAX_CONTROL_BYTES, "replacement manifest")
    schema = _schema("sync-manifest.schema.json", "schema_2_2", previous)
    if list(Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(manifest)):
        raise ContractError("replacement manifest violates schema 2.2")
    if manifest["vehicle_id"] != vehicle_id:
        raise ContractError("replacement manifest vehicle mismatch")
    chunks = manifest["chunks"]
    if [item["chunk_index"] for item in chunks] != list(range(len(chunks))):
        raise ContractError("replacement manifest chunks are not contiguous")
    _verify_fixture_signature(manifest, vehicle_id, root, "replacement manifest signature")
    return manifest


def _check_zstd_frames(compressed, limit):
    # stream_reader bounds output reads but does not report a truncated final frame.
    # Check frame boundaries first; accept any number of complete standard/skippable
    # frames, then let the decoder verify compressed blocks and checksums.
    offset = known_bytes = 0
    while offset < len(compressed):
        if len(compressed) - offset < 4:
            raise ContractError("incomplete zstd frame")
        magic = int.from_bytes(compressed[offset:offset + 4], "little")
        if 0x184D2A50 <= magic <= 0x184D2A5F:
            if len(compressed) - offset < 8:
                raise ContractError("incomplete zstd frame")
            size = int.from_bytes(compressed[offset + 4:offset + 8], "little")
            offset += 8 + size
        elif magic == 0xFD2FB528:
            header = compressed[offset:offset + 18]
            parameters = zstandard.get_frame_parameters(header)
            if parameters.content_size != zstandard.CONTENTSIZE_UNKNOWN:
                known_bytes += parameters.content_size
                if known_bytes > limit:
                    raise ContractError("pack exceeds uncompressed byte limit")
            offset += zstandard.frame_header_size(header)
            while True:
                if len(compressed) - offset < 3:
                    raise ContractError("incomplete zstd frame")
                block = int.from_bytes(compressed[offset:offset + 3], "little")
                block_type = (block >> 1) & 3
                if block_type == 3:
                    raise ContractError("invalid zstd block")
                offset += 3 + (1 if block_type == 1 else block >> 3)
                if offset > len(compressed):
                    raise ContractError("incomplete zstd frame")
                if block & 1:
                    break
            offset += 4 if parameters.has_checksum else 0
        else:
            raise ContractError("invalid zstd frame")
        if offset > len(compressed):
            raise ContractError("incomplete zstd frame")


def _decompress_pack(compressed, path, expected_bytes, max_output_bytes=256 * 1024 * 1024):
    if not 0 <= expected_bytes <= max_output_bytes:
        raise ContractError("pack exceeds uncompressed byte limit")
    try:
        _check_zstd_frames(compressed, expected_bytes)
        total = 0
        with zstandard.ZstdDecompressor().stream_reader(io.BytesIO(compressed), read_across_frames=True) as reader, Path(path).open("wb") as output:
            while True:
                block = reader.read(min(64 * 1024, expected_bytes - total + 1))
                if not block:
                    break
                total += len(block)
                if total > expected_bytes:
                    raise ContractError("pack exceeds uncompressed byte limit")
                output.write(block)
    except zstandard.ZstdError as exc:
        raise ContractError("invalid zstd pack") from exc
    if total != expected_bytes:
        raise ContractError("aggregate uncompressed byte count mismatch")


def _verify_physical_values(connection, table):
    invalid = []
    text_fields = [field["name"] for field in table["columns"] if field["sqlite_type"] == "TEXT"]
    if text_fields:
        columns = ",".join(f"CAST({name} AS BLOB)" for name in text_fields)
        for row in connection.execute(f"SELECT {columns} FROM {table['name']}"):
            for raw in row:
                if raw is not None:
                    try:
                        raw.decode("utf-8", errors="strict")
                    except UnicodeDecodeError as exc:
                        raise ContractError("physical TEXT is not valid UTF-8") from exc
    for field in table["columns"]:
        name = field["name"]
        if field["sqlite_type"] == "BLOB":
            invalid.append(f"({name} IS NOT NULL AND length({name}) != 8)")
        if name.endswith("_is_nan"):
            value = name[:-7]
            invalid.append(f"({name} NOT IN (0, 1) OR ({name} = 1 AND {value} IS NOT NULL))")
    if invalid and connection.execute(f"SELECT 1 FROM {table['name']} WHERE {' OR '.join(invalid)} LIMIT 1").fetchone():
        raise ContractError("physical value representation mismatch")


def _rows_digest(rows):
    # Sorting is by table/root name and numeric ID, matching the pack contract.
    ordered = sorted(rows, key=lambda row: (row[0], row[1]))
    raw = "".join("\t".join(map(str, row)) + "\n" for row in ordered).encode("ascii")
    return hashlib.sha256(raw).hexdigest()


def verify_pack(receipt, compressed, root=PROFILE):
    receipt = canonical_integral_value(receipt)
    if len(receipt["chunks"]) != 1:
        raise ContractError("single-pack fixture expected")
    descriptor = receipt["chunks"][0]["pack"]
    if len(compressed) != descriptor["compressed_bytes"] or hashlib.sha256(compressed).hexdigest() != descriptor["sha256"]:
        raise ContractError("pack digest mismatch")
    catalog = read_json(Path(root) / "physical-field-catalog.json")
    with tempfile.TemporaryDirectory() as temp:
        path = Path(temp) / "pack.sqlite"
        _decompress_pack(compressed, path, receipt["total_uncompressed_bytes"])
        connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        try:
            if connection.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                raise ContractError("invalid SQLite pack")
            expected_db = sqlite3.connect(":memory:")
            expected_db.executescript((Path(root) / "physical-delta-pack-v1.sql").read_text())
            schema_query = "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name"
            expected_schema = list(expected_db.execute(schema_query))
            observed_schema = list(connection.execute(schema_query))
            expected_db.close()
            if observed_schema != expected_schema:
                raise ContractError("physical pack schema mismatch")
            roles = {}
            for table in catalog["tables"]:
                name = table["name"]
                observed = [(row[1], row[2], not bool(row[3]), bool(row[5]))
                            for row in connection.execute(f"PRAGMA table_xinfo('{name}')")]
                expected = [(field["name"], field["sqlite_type"], field["nullable"], field["primary_key"])
                            for field in table["columns"]]
                if observed != expected:
                    raise ContractError("physical row layout mismatch")
                _verify_physical_values(connection, table)
                roles[name] = {row[0] for row in connection.execute(f"SELECT id FROM {name}")}
            assigned = {(name, entity_id): role for name, entity_id, role in connection.execute("SELECT table_name, entity_id, role FROM row_roles")}
            if len(assigned) != sum(len(ids) for ids in roles.values()) or any((name, entity_id) not in assigned for name, ids in roles.items() for entity_id in ids):
                raise ContractError("row role set is incomplete")
            tombstones = {(name, entity_id) for name, entity_id in connection.execute("SELECT table_name, entity_id FROM tombstones")}
            if tombstones & set(assigned):
                raise ContractError("upsert overlaps tombstone")
            if len(assigned) + len(tombstones) != receipt["total_rows"]:
                raise ContractError("typed row total mismatch")
            required_metadata = {
                "payload_format": receipt["payload_format"],
                "base_manifest_id": receipt["base"]["manifest_id"],
                "base_receipt_id": receipt["base"]["receipt_id"],
                "base_manifest_sha256": receipt["base"]["manifest_sha256"],
                "base_sequence": str(receipt["base"]["sequence"]),
                "target_manifest_id": receipt["target"]["manifest_id"],
                "target_receipt_id": receipt["target"]["receipt_id"],
                "target_manifest_sha256": receipt["target"]["manifest_sha256"],
                "target_sequence": str(receipt["target"]["sequence"]),
                "target_raw_sha256": receipt["target_raw_sha256"],
                **{key: str(value) for key, value in receipt["source"].items()},
            }
            metadata = dict(connection.execute("SELECT key,value FROM delta_metadata"))
            if metadata != required_metadata:
                raise ContractError("physical pack binding mismatch")
            witness = list(connection.execute("SELECT table_name, entity_id, effect FROM affected_projected_ids"))
            if len(witness) != receipt["affected_projected_ids_count"] or _rows_digest(witness) != receipt["affected_projected_ids_sha256"]:
                raise ContractError("affected projected ID witness mismatch")
            scoped = {(table, entity_id): effect for table, entity_id, effect in witness}
            roots = list(connection.execute("SELECT root_type, root_id, base_state, target_state, target_child_count FROM impacted_roots"))
            if len(roots) != receipt["impacted_roots_count"] or _rows_digest(roots) != receipt["impacted_roots_sha256"]:
                raise ContractError("impacted root witness mismatch")
            root_keys = {(kind, root_id) for kind, root_id, *_ in roots}
            projected = {"cars": "cars", "car_settings": "car_settings", "drives": "drives",
                         "positions": "positions", "charging_processes": "charges", "charges": "charge_samples",
                         "states": "car_states", "updates": "car_updates"}
            # Direct operations are decidable from this pack. Old relation edges
            # still require the caller's separately admitted retained base.
            for (table, entity_id), role in assigned.items():
                if role == "changed" and table in projected and scoped.get((projected[table], entity_id)) != "recompute":
                    raise ContractError("missing changed-row recomputation scope")
                if role == "changed" and table in ("drives", "charging_processes"):
                    kind = "drive" if table == "drives" else "charge"
                    if (kind, entity_id) not in root_keys:
                        raise ContractError("missing impacted root witness")
            for table, entity_id in tombstones:
                if table in projected and scoped.get((projected[table], entity_id)) != "delete":
                    raise ContractError("missing tombstone deletion scope")
                if table in ("drives", "charging_processes"):
                    kind = "drive" if table == "drives" else "charge"
                    if (kind, entity_id) not in root_keys:
                        raise ContractError("missing impacted root witness")
            for table, foreign_key, kind in (("positions", "drive_id", "drive"),
                                              ("charges", "charging_process_id", "charge")):
                for entity_id, owner_id in connection.execute(
                        f"SELECT child.id, child.{foreign_key} FROM {table} AS child "
                        "JOIN row_roles AS role ON role.table_name=? AND role.entity_id=child.id "
                        "WHERE role.role='changed'", (table,)):
                    if owner_id is not None and (kind, owner_id) not in root_keys:
                        raise ContractError("missing changed-child impacted root witness")
            child_counts = {
                kind: dict(connection.execute(
                    f"SELECT {foreign_key}, COUNT(*) FROM {child_table} WHERE {foreign_key} IN "
                    "(SELECT root_id FROM impacted_roots WHERE root_type=?) "
                    f"GROUP BY {foreign_key}", (kind,)))
                for kind, child_table, foreign_key in (("drive", "positions", "drive_id"),
                                                       ("charge", "charges", "charging_process_id"))
            }
            for kind, root_id, base_state, target_state, count in roots:
                observed_count = child_counts[kind].get(root_id, 0)
                if observed_count != count:
                    raise ContractError("incomplete impacted root")
                root_table = "drives" if kind == "drive" else "charging_processes"
                root_present = root_id in roles[root_table]
                if (target_state == "absent") != (root_present == 0):
                    raise ContractError("incomplete impacted root")
                projected_root = "drives" if kind == "drive" else "charges"
                if target_state == "absent" and base_state != "absent" and (root_table, root_id) not in tombstones:
                    raise ContractError("absent root is missing raw tombstone")
                if target_state == base_state == "absent":
                    if (root_table, root_id) in tombstones or scoped.get((projected_root, root_id)) == "delete":
                        raise ContractError("absent context root invents deletion")
                    continue
                expected_effect = "delete" if target_state == "absent" else "recompute"
                if scoped.get((projected_root, root_id)) != expected_effect:
                    raise ContractError("missing root recomputation scope")
                if kind == "charge" and target_state == "absent" and observed_count != 0:
                    raise ContractError("absent required parent retains charge samples")
                if root_present:
                    end_date = connection.execute(f"SELECT end_date_pg_us FROM {root_table} WHERE id=?", (root_id,)).fetchone()[0]
                    if (target_state == "open") != (end_date is None):
                        raise ContractError("impacted root open/closed state mismatch")
            for (child_id,) in connection.execute(
                    "SELECT id FROM positions WHERE drive_id IN "
                    "(SELECT root_id FROM impacted_roots WHERE root_type='drive' "
                    "AND target_state IN ('absent', 'open'))"):
                if scoped.get(("positions", child_id)) != "recompute":
                    raise ContractError("missing surviving-child recomputation scope")
            for table, foreign_key, kind, projection in (("positions", "drive_id", "drive", "positions"),
                                                         ("charges", "charging_process_id", "charge", "charge_samples")):
                for (child_id,) in connection.execute(
                        f"SELECT id FROM {table} WHERE {foreign_key} IN "
                        "(SELECT root_id FROM impacted_roots WHERE root_type=?)", (kind,)):
                    if scoped.get((projection, child_id)) != "recompute":
                        raise ContractError("missing root-child recomputation scope")
            return True
        finally:
            connection.close()
