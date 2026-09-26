"""Deterministic validation for the source-neutral changes-since profile."""
import hashlib
import json
from pathlib import Path
import re

from jsonschema import Draft202012Validator, FormatChecker

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.1.0"
PROFILE_ID = "hub-sync-v1@1.1.0"


class ContractError(ValueError):
    """Only fixed labels leave this validator."""


def strict_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate member")
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique, parse_constant=lambda _: (_ for _ in ()).throw(ValueError("non-finite number")))


def load_profile(root=PROFILE):
    root = Path(root)
    manifest = (root / "SHA256SUMS").read_bytes()
    listed = {}
    for line in manifest.decode("ascii").splitlines():
        digest, name = line.split("  ", 1)
        if not re.fullmatch("[0-9a-f]{64}", digest) or name in listed or name.startswith("/") or ".." in Path(name).parts:
            raise ContractError("invalid profile manifest")
        if hashlib.sha256((root / name).read_bytes()).hexdigest() != digest:
            raise ContractError("profile file digest mismatch")
        listed[name] = digest
    required = {"profile.json", "changes-since-request.schema.json", "changed-set-receipt.schema.json", "rebase-hint.schema.json", "sync-manifest.schema.json", "noop.schema.json", "status-tables.schema.json", "status-tables.json", "openapi.json", "cases.json"}
    if not required <= set(listed):
        raise ContractError("incomplete profile manifest")
    profile = strict_json((root / "profile.json").read_bytes())
    if profile.get("profile_id") != PROFILE_ID:
        raise ContractError("unsupported profile identity")
    return dict(profile, profile_sha256=hashlib.sha256(manifest).hexdigest())


def _schema(root, name, definition):
    schema = strict_json((Path(root) / name).read_bytes())
    return {**schema, "$ref": "#/$defs/" + definition}


def validate_request(value, root=PROFILE):
    errors = list(Draft202012Validator(_schema(root, "changes-since-request.schema.json", "request"), format_checker=FormatChecker()).iter_errors(value))
    if errors:
        return ["request violates changes-since schema"]
    versions = value["schema_version_range"]
    if float(versions["minimum"]) > float(versions["maximum"]):
        return ["schema version range is reversed"]
    return []


def _validate_signature(value):
    signature = value["signature"]
    if signature["signed_payload_sha256"] == "0" * 64:
        return ["signature digest is reserved"]
    return []


def validate_response(status, value, root=PROFILE):
    if status == 200:
        errors = list(Draft202012Validator(_schema(root, "changed-set-receipt.schema.json", "receipt"), format_checker=FormatChecker()).iter_errors(value))
        if errors:
            return ["changed-set receipt violates schema"]
        if value["to_sequence"] <= value["from_sequence"]:
            return ["changed-set receipt does not advance"]
        pack_errors = _validate_pack_limits([value["pack"]], root)
        return pack_errors or _validate_signature(value)
    if status == 409:
        errors = list(Draft202012Validator(_schema(root, "rebase-hint.schema.json", "hint"), format_checker=FormatChecker()).iter_errors(value))
        if errors:
            return ["rebase hint violates schema"]
        replacement = value["replacement"]
        retry = value["retry_request"]
        if retry["base_receipt_id"] != replacement["receipt_id"] or retry["from_sequence"] != replacement["sequence"]:
            return ["rebase retry does not bind replacement"]
        if float(retry["schema_version_range"]["minimum"]) > float(retry["schema_version_range"]["maximum"]):
            return ["schema version range is reversed"]
        pack_errors = _validate_pack_limits([replacement["pack"]], root)
        return pack_errors or _validate_signature(value)
    return ["status is not specified by profile"]


def _validate_pack_limits(packs, root):
    limits = load_profile(root)["limits"]
    if any(not limits["min_pack_compressed_bytes"] <= item["compressed_bytes"] <= limits["max_pack_compressed_bytes"] for item in packs):
        return ["pack compressed bytes exceed profile limits"]
    return []


def validate_manifest(value, root=PROFILE):
    errors = list(Draft202012Validator(_schema(root, "sync-manifest.schema.json", "manifest"), format_checker=FormatChecker()).iter_errors(value))
    if errors:
        return ["sync manifest violates schema"]
    if value["schema_version"] == "2.1":
        return _validate_pack_limits([value["pack"]], root) + _validate_signature(value)
    chunks = value["chunks"]
    if [item["chunk_index"] for item in chunks] != list(range(len(chunks))):
        return ["manifest chunks are not contiguous"]
    pack_errors = _validate_pack_limits([item["pack"] for item in chunks], root)
    return pack_errors or _validate_signature(value)


def validate_noop(status, value, headers=None, raw=b"", root=PROFILE):
    if status == 200:
        errors = list(Draft202012Validator(_schema(root, "noop.schema.json", "receipt"), format_checker=FormatChecker()).iter_errors(value))
        if errors:
            return ["no-op receipt violates schema"]
        return _validate_signature(value)
    if status == 406:
        headers = {key.lower(): item for key, item in (headers or {}).items()}
        if raw or headers.get("cache-control") != "no-store":
            return ["no-op unavailable must be empty no-store"]
        return []
    return ["status is not specified by profile"]


def validate_status_tables(value, root=PROFILE):
    errors = list(Draft202012Validator(_schema(root, "status-tables.schema.json", "document"), format_checker=FormatChecker()).iter_errors(value))
    if errors:
        return ["status tables violate schema"]
    expected = {
        "POST /v1/vehicles/{vehicle_id}/sync/changes-since": {200, 401, 409},
        "GET /v1/vehicles/{vehicle_id}/sync/manifest": {200, 401, 406},
        "GET /v1/vehicles/{vehicle_id}/sync/noop": {200, 401, 406},
        "GET /v1/packs/sha256/{object_name}": {200, 206, 401, 404},
    }
    actual = {item["route"]: {response["status"] for response in item["responses"]} for item in value["tables"]}
    if actual != expected:
        return ["status tables are incomplete"]
    return []
