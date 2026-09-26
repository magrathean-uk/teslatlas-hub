"""Deterministic validation for the source-neutral changes-since profile."""
import hashlib
import json
from pathlib import Path
import re
import base64
import binascii

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from jsonschema import Draft202012Validator, FormatChecker

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.3.0"
PROFILE_ID = "hub-sync-v1@1.3.0"


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
    required = {"profile.json", "changes-since-request.schema.json", "changed-set-receipt.schema.json", "rebase-hint.schema.json", "sync-manifest.schema.json", "noop.schema.json", "prepared-artefact.schema.json", "signing-keys.schema.json", "fixture-signing-keys.json", "status-tables.schema.json", "status-tables.json", "openapi.json", "cases.json"}
    if not required <= set(listed):
        raise ContractError("incomplete profile manifest")
    profile = strict_json((root / "profile.json").read_bytes())
    if profile.get("profile_id") != PROFILE_ID:
        raise ContractError("unsupported profile identity")
    return dict(profile, profile_sha256=hashlib.sha256(manifest).hexdigest())


def _schema(root, name, definition):
    schema = strict_json((Path(root) / name).read_bytes())
    return {**schema, "$ref": "#/$defs/" + definition}


def canonical_fixture_payload(value):
    payload = dict(value)
    payload.pop("signature", None)
    # Fixtures are in the narrow RFC 8785 subset of ASCII strings, integers,
    # arrays, and objects; sorted compact UTF-8 JSON is JCS for that subset.
    return json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def load_signing_keys(root=PROFILE):
    root = Path(root)
    profile = load_profile(root)
    name = profile["fixture_signing_keys"]
    value = strict_json((root / name).read_bytes())
    errors = list(Draft202012Validator(_schema(root, "signing-keys.schema.json", "document")).iter_errors(value))
    if errors:
        raise ContractError("invalid fixture signing keys")
    if value["profile_id"] != profile["profile_id"]:
        raise ContractError("fixture signing keys profile mismatch")
    return value


def validate_request(value, root=PROFILE):
    errors = list(Draft202012Validator(_schema(root, "changes-since-request.schema.json", "request"), format_checker=FormatChecker()).iter_errors(value))
    if errors:
        return ["request violates changes-since schema"]
    versions = value["schema_version_range"]
    if float(versions["minimum"]) > float(versions["maximum"]):
        return ["schema version range is reversed"]
    return []


def verify_signature(value, key_set):
    signature = value["signature"]
    if signature["signed_payload_sha256"] == "0" * 64:
        return ["signature digest is reserved"]
    payload = canonical_fixture_payload(value)
    if hashlib.sha256(payload).hexdigest() != signature["signed_payload_sha256"]:
        return ["signature payload digest mismatch"]
    matches = [item for item in key_set["keys"] if item["key_id"] == signature["key_id"] and item["algorithm"] == signature["algorithm"]]
    if len(matches) != 1:
        return ["signature key is unknown"]
    try:
        public_key = Ed25519PublicKey.from_public_bytes(base64.b64decode(matches[0]["public_key"], validate=True))
        public_key.verify(base64.b64decode(signature["signature"], validate=True), payload)
    except (InvalidSignature, ValueError, binascii.Error):
        return ["signature verification failed"]
    return []


def _validate_signature(value, root):
    return verify_signature(value, load_signing_keys(root))


def validate_response(status, value, root=PROFILE):
    if status == 200:
        errors = list(Draft202012Validator(_schema(root, "changed-set-receipt.schema.json", "receipt"), format_checker=FormatChecker()).iter_errors(value))
        if errors:
            return ["changed-set receipt violates schema"]
        if value["to_sequence"] <= value["from_sequence"]:
            return ["changed-set receipt does not advance"]
        pack_errors = _validate_pack_limits([value["pack"]], root)
        return pack_errors or _validate_signature(value, root)
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
        return pack_errors or _validate_signature(value, root)
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
        return _validate_pack_limits([value["pack"]], root) + _validate_signature(value, root)
    chunks = value["chunks"]
    if [item["chunk_index"] for item in chunks] != list(range(len(chunks))):
        return ["manifest chunks are not contiguous"]
    pack_errors = _validate_pack_limits([item["pack"] for item in chunks], root)
    return pack_errors or _validate_signature(value, root)


def validate_noop(status, value, headers=None, raw=b"", root=PROFILE):
    if status == 200:
        errors = list(Draft202012Validator(_schema(root, "noop.schema.json", "receipt"), format_checker=FormatChecker()).iter_errors(value))
        if errors:
            return ["no-op receipt violates schema"]
        return _validate_signature(value, root)
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
        "GET /v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}": {200, 401, 404},
    }
    actual = {item["route"]: {response["status"] for response in item["responses"]} for item in value["tables"]}
    if actual != expected:
        return ["status tables are incomplete"]
    return []


def validate_prepared_artefact(value, root=PROFILE):
    errors = list(Draft202012Validator(_schema(root, "prepared-artefact.schema.json", "receipt"), format_checker=FormatChecker()).iter_errors(value))
    if errors:
        return ["prepared-artefact receipt violates schema"]
    window = value["window"]
    if window["from_ms"] >= window["to_ms"]:
        return ["prepared-artefact window is invalid"]
    spans = value["dirty_spans"]
    if not spans["map_months"] and not spans["routes"]:
        return ["prepared-artefact has no dirty spans"]
    if len({span["month"] for span in spans["map_months"]}) != len(spans["map_months"]):
        return ["prepared-artefact repeats map month"]
    if len({span["route_id"] for span in spans["routes"]}) != len(spans["routes"]):
        return ["prepared-artefact repeats route"]
    for span in [*spans["map_months"], *spans["routes"]]:
        if span["from_ms"] >= span["to_ms"] or span["from_ms"] < window["from_ms"] or span["to_ms"] > window["to_ms"]:
            return ["prepared-artefact dirty span is outside window"]
    if value["generation"]["generated_at_ms"] < window["to_ms"]:
        return ["prepared-artefact generation predates window"]
    pack_errors = _validate_pack_limits([value["pack"]], root)
    return pack_errors or _validate_signature(value, root)
