"""Independent admission checks for the additive PhysicalV3 delta profile."""
import base64
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import tempfile

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from jsonschema import Draft202012Validator, FormatChecker
import zstandard

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.4.0"


class ContractError(ValueError):
    pass


def read_json(path):
    def unique(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ContractError("duplicate JSON member")
            value[key] = item
        return value
    return json.loads(Path(path).read_bytes(), object_pairs_hook=unique)


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
    return request


def _canonical_unsigned(receipt):
    value = dict(receipt)
    value.pop("signature", None)
    # The profile constrains signed members to the ASCII JCS subset.
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def verify_receipt(request, receipt, route_vehicle_id, root=PROFILE):
    validate_request(request, root)
    if list(Draft202012Validator(_schema("physical-changed-set-receipt.schema.json", "receipt", root),
                                  format_checker=FormatChecker()).iter_errors(receipt)):
        raise ContractError("receipt violates 1.4 schema")
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
    signature = receipt["signature"]
    payload = _canonical_unsigned(receipt)
    if hashlib.sha256(payload).hexdigest() != signature["signed_payload_sha256"]:
        raise ContractError("signature payload digest mismatch")
    key_document = read_json(ROOT / "profiles/hub-sync-v1/1.3.0/fixture-signing-keys.json")
    keys = key_document["keys"]
    selected = next((item for item in keys if item["key_id"] == signature["key_id"]), None)
    if selected is None:
        raise ContractError("unknown signing key")
    key = Ed25519PublicKey.from_public_bytes(base64.b64decode(selected["public_key"]))
    try:
        key.verify(base64.b64decode(signature["signature"]), payload)
    except Exception as exc:
        raise ContractError("signature verification failed") from exc
    return receipt


def verify_signed_manifest_bindings(receipt, base_manifest_raw, target_manifest_raw):
    for label, raw in (("base", base_manifest_raw), ("target", target_manifest_raw)):
        value = json.loads(raw)
        checkpoint = receipt[label]
        if hashlib.sha256(raw).hexdigest() != checkpoint["manifest_sha256"]:
            raise ContractError(f"{label} signed manifest digest mismatch")
        if (value["manifest_id"] != checkpoint["manifest_id"] or
                value["receipt_id"] != checkpoint["receipt_id"] or
                value["sequence"] != checkpoint["sequence"] or
                value["schema_version"] != checkpoint["schema_version"] or
                value["vehicle_id"] != receipt["vehicle_id"]):
            raise ContractError(f"{label} signed manifest identity mismatch")
        signature = value["signature"]
        payload = _canonical_unsigned(value)
        if hashlib.sha256(payload).hexdigest() != signature["signed_payload_sha256"]:
            raise ContractError(f"{label} signed manifest payload mismatch")
        document = read_json(ROOT / "profiles/hub-sync-v1/1.3.0/fixture-signing-keys.json")
        selected = next((key for key in document["keys"] if key["key_id"] == signature["key_id"]), None)
        if selected is None:
            raise ContractError("unknown signing key")
        key = Ed25519PublicKey.from_public_bytes(base64.b64decode(selected["public_key"]))
        try:
            key.verify(base64.b64decode(signature["signature"]), payload)
        except Exception as exc:
            raise ContractError(f"{label} signed manifest signature invalid") from exc
    return True


def verify_rebase(request, hint, route_vehicle_id, replacement_manifest_raw, root=PROFILE):
    validate_request(request, root)
    if list(Draft202012Validator(_schema("physical-rebase-hint.schema.json", "hint", root),
                                  format_checker=FormatChecker()).iter_errors(hint)):
        raise ContractError("rebase violates 1.4 schema")
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
    manifest = json.loads(replacement_manifest_raw)
    if hashlib.sha256(replacement_manifest_raw).hexdigest() != replacement["manifest_sha256"]:
        raise ContractError("rebase replacement manifest digest mismatch")
    if any(manifest[key] != replacement[key] for key in ("manifest_id", "receipt_id", "sequence")) or manifest["chunks"] != replacement["chunks"]:
        raise ContractError("rebase replacement manifest mismatch")
    signature = hint["signature"]
    payload = _canonical_unsigned(hint)
    if hashlib.sha256(payload).hexdigest() != signature["signed_payload_sha256"]:
        raise ContractError("rebase signature digest mismatch")
    document = read_json(ROOT / "profiles/hub-sync-v1/1.3.0/fixture-signing-keys.json")
    selected = next((key for key in document["keys"] if key["key_id"] == signature["key_id"]), None)
    if selected is None:
        raise ContractError("unknown signing key")
    key = Ed25519PublicKey.from_public_bytes(base64.b64decode(selected["public_key"]))
    try:
        key.verify(base64.b64decode(signature["signature"]), payload)
    except Exception as exc:
        raise ContractError("rebase signature invalid") from exc
    return True


def _rows_digest(rows):
    # Sorting is by table/root name and numeric ID, matching the pack contract.
    ordered = sorted(rows, key=lambda row: (row[0], row[1]))
    raw = "".join("\t".join(map(str, row)) + "\n" for row in ordered).encode("ascii")
    return hashlib.sha256(raw).hexdigest()


def verify_pack(receipt, compressed, root=PROFILE):
    if len(receipt["chunks"]) != 1:
        raise ContractError("single-pack fixture expected")
    descriptor = receipt["chunks"][0]["pack"]
    if len(compressed) != descriptor["compressed_bytes"] or hashlib.sha256(compressed).hexdigest() != descriptor["sha256"]:
        raise ContractError("pack digest mismatch")
    raw = zstandard.ZstdDecompressor().decompress(compressed, max_output_size=256 * 1024 * 1024)
    if len(raw) != receipt["total_uncompressed_bytes"]:
        raise ContractError("aggregate uncompressed byte count mismatch")
    catalog = read_json(Path(root) / "physical-field-catalog.json")
    with tempfile.TemporaryDirectory() as temp:
        path = Path(temp) / "pack.sqlite"
        path.write_bytes(raw)
        connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        try:
            if connection.execute("PRAGMA quick_check").fetchone()[0] != "ok":
                raise ContractError("invalid SQLite pack")
            expected_db = sqlite3.connect(":memory:")
            expected_db.executescript((Path(root) / "physical-delta-pack-v1.sql").read_text())
            expected_schema = {name: sql for name, sql in expected_db.execute("SELECT name, sql FROM sqlite_schema WHERE type='table'")}
            observed_schema = {name: sql for name, sql in connection.execute("SELECT name, sql FROM sqlite_schema WHERE type='table'")}
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
            for kind, root_id, _, target_state, count in roots:
                child_table, foreign_key = ("positions", "drive_id") if kind == "drive" else ("charges", "charging_process_id")
                observed_count = connection.execute(f"SELECT COUNT(*) FROM {child_table} WHERE {foreign_key}=?", (root_id,)).fetchone()[0]
                if observed_count != count:
                    raise ContractError("incomplete impacted root")
                root_table = "drives" if kind == "drive" else "charging_processes"
                root_present = connection.execute(f"SELECT COUNT(*) FROM {root_table} WHERE id=?", (root_id,)).fetchone()[0]
                if (target_state == "absent") != (root_present == 0):
                    raise ContractError("incomplete impacted root")
                projected_root = "drives" if kind == "drive" else "charges"
                expected_effect = "delete" if target_state == "absent" else "recompute"
                if scoped.get((projected_root, root_id)) != expected_effect:
                    raise ContractError("missing root recomputation scope")
                if kind == "charge" and target_state == "absent" and observed_count != 0:
                    raise ContractError("absent required parent retains charge samples")
                if kind == "drive" and target_state in ("absent", "open"):
                    for (child_id,) in connection.execute(
                            "SELECT id FROM positions WHERE drive_id=?", (root_id,)):
                        if scoped.get(("positions", child_id)) != "recompute":
                            raise ContractError("missing surviving-child recomputation scope")
                if root_present:
                    end_date = connection.execute(f"SELECT end_date_pg_us FROM {root_table} WHERE id=?", (root_id,)).fetchone()[0]
                    if (target_state == "open") != (end_date is None):
                        raise ContractError("impacted root open/closed state mismatch")
            return True
        finally:
            connection.close()
