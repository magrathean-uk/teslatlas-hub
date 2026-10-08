"""Deterministic validation for the source-neutral changes-since profile."""
import copy
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import re
import base64
import binascii
import sqlite3
import struct
import tempfile
from datetime import datetime, timezone

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from jsonschema import Draft202012Validator, FormatChecker
import zstandard

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.3.0"
PROFILE_ID = "hub-sync-v1@1.3.0"
PREPARED_MONTH_ID_DOMAIN = "teslatlas-prepared-map-month-id-v1"


def prepared_month_artifact_id(receipt, month):
    source = receipt["source"]
    window = receipt["window"]
    fields = (PREPARED_MONTH_ID_DOMAIN, receipt["vehicle_id"],
              source["input_manifest_id"], source["input_receipt_id"],
              str(source["input_sequence"]), month, str(window["from_ms"]),
              str(window["to_ms"]), receipt["map_style"],
              receipt["tile_geometry_version"], receipt["algorithm_version"])
    return "map-month-v1." + hashlib.sha256(("\n".join(fields) + "\n").encode("ascii")).hexdigest()


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
    required = {"profile.json", "changes-since-request.schema.json", "changed-set-receipt.schema.json", "rebase-hint.schema.json", "sync-manifest.schema.json", "noop.schema.json", "prepared-artefact.schema.json", "prepared-pack-v1-contract.json", "prepared-pack-v1.sql", "signing-keys.schema.json", "fixture-signing-keys.json", "sync-error.schema.json", "status-tables.schema.json", "status-tables.json", "openapi.json", "cases.json"}
    if not required <= set(listed):
        raise ContractError("incomplete profile manifest")
    profile = strict_json((root / "profile.json").read_bytes())
    if profile.get("profile_id") != PROFILE_ID:
        raise ContractError("unsupported profile identity")
    return dict(profile, profile_sha256=hashlib.sha256(manifest).hexdigest())


def _schema(root, name, definition):
    schema = strict_json((Path(root) / name).read_bytes())
    return {**schema, "$ref": "#/$defs/" + definition}


def canonical_integral_value(value):
    """Normalize the published safe integral JSON number domain for JCS."""
    if isinstance(value, bool) or value is None or isinstance(value, str):
        return value
    if isinstance(value, (int, float)):
        if (isinstance(value, float) and (not math.isfinite(value) or not value.is_integer())) or abs(value) > 9007199254740991:
            raise ContractError("signed number is outside the safe integral domain")
        return int(value)
    if isinstance(value, list):
        return [canonical_integral_value(item) for item in value]
    if isinstance(value, dict):
        return {key: canonical_integral_value(item) for key, item in value.items()}
    raise ContractError("unsupported signed JSON value")


def canonical_fixture_payload(value):
    payload = dict(value)
    payload.pop("signature", None)
    # Fixtures are in the narrow RFC 8785 subset of ASCII strings, integers,
    # arrays, and objects; sorted compact UTF-8 JSON is JCS for that subset.
    return json.dumps(canonical_integral_value(payload), ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


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


def load_fixtures(root=PROFILE):
    fixtures = {}
    for path in sorted((Path(root) / "examples").glob("*.json")):
        value = strict_json(path.read_bytes())
        fixture_id = value.get("fixture_id") if isinstance(value, dict) else None
        if not isinstance(fixture_id, str) or not fixture_id or fixture_id in fixtures:
            raise ContractError("invalid fixture identifier")
        fixtures[fixture_id] = value
    return fixtures


def _version(value):
    return tuple(int(part) for part in value.split("."))


def validate_request(value, root=PROFILE):
    errors = list(Draft202012Validator(_schema(root, "changes-since-request.schema.json", "request"), format_checker=FormatChecker()).iter_errors(value))
    if errors:
        return ["request violates changes-since schema"]
    versions = value["schema_version_range"]
    minimum = _version(versions["minimum"])
    maximum = _version(versions["maximum"])
    base = _version(value["base_manifest_schema"])
    if minimum > maximum:
        return ["schema version range is reversed"]
    supported = [_version(version) for version in load_profile(root)["schema_version_range"].values()]
    if maximum < min(supported) or minimum > max(supported):
        return ["schema version range is unsupported"]
    if not minimum <= base <= maximum:
        return ["base manifest schema is outside accepted range"]
    return []


def validate_request_body(raw, root=PROFILE):
    limit = load_profile(root)["limits"]["max_request_bytes"]
    if len(raw) > limit:
        return [f"request body exceeds {limit} bytes"]
    try:
        value = strict_json(raw)
    except (UnicodeDecodeError, ValueError, json.JSONDecodeError):
        return ["request body is not valid JSON"]
    return validate_request(value, root)


def validate_bootstrap_selection(value, status, root=PROFILE):
    if not isinstance(value, dict) or not isinstance(value.get("vehicle_id"), str):
        return ["bootstrap fixture is invalid"]
    request = value.get("request")
    if not isinstance(request, dict) or request.get("method") != "GET":
        return ["bootstrap fixture request is invalid"]
    if request.get("path") != f"/v1/vehicles/{value['vehicle_id']}/sync/manifest":
        return ["bootstrap fixture route is invalid"]
    headers = request.get("headers")
    if not isinstance(headers, list) or not all(
        isinstance(item, dict)
        and set(item) == {"name", "value"}
        and isinstance(item["name"], str)
        and isinstance(item["value"], str)
        for item in headers
    ):
        return ["bootstrap fixture headers are invalid"]
    selector = load_profile(root)["bootstrap_selector"]
    profile_values = [item["value"] for item in headers if item["name"].lower() == selector["header"]]
    schema_values = [item["value"] for item in headers if item["name"].lower() == selector["required_schema_header"]]
    if not profile_values:
        actual_response = {"status": 200, "representation": selector["without_selector"]}
    elif profile_values == [selector["value"]] and schema_values == [selector["required_schema_value"]]:
        actual_response = {"status": 200, "representation": selector["value"]}
    else:
        rejection = selector["invalid_explicit_selector"]
        actual_response = {
            "status": rejection["status"],
            "representation": "empty",
            "headers": {"cache_control": rejection["cache_control"]},
            "body_bytes": rejection["body_bytes"],
            "manifest_signature": rejection["manifest_signature"],
        }
    if status != actual_response["status"]:
        return ["bootstrap fixture status does not match request headers"]
    if value.get("expected_response") != actual_response:
        return ["bootstrap response does not match request headers"]
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


def _validate_signature(value, vehicle_id, root):
    key_set = load_signing_keys(root)
    key_errors = validate_signing_keys(key_set, vehicle_id, root)
    return key_errors or verify_signature(value, key_set)


def validate_signing_keys(value, vehicle_id, root=PROFILE):
    errors = list(Draft202012Validator(_schema(root, "signing-keys.schema.json", "document"), format_checker=FormatChecker()).iter_errors(value))
    if errors:
        return ["signing keys violate schema"]
    if value["vehicle_id"] != vehicle_id:
        return ["signing keys are bound to another vehicle"]
    seen = set()
    for item in value["keys"]:
        try:
            raw = base64.b64decode(item["public_key"], validate=True)
        except (ValueError, binascii.Error):
            return ["signing key encoding is invalid"]
        expected = "ed25519-sha256-" + hashlib.sha256(raw).hexdigest()
        if item["key_id"] != expected or item["key_id"] in seen:
            return ["signing key identifier is not stable"]
        seen.add(item["key_id"])
    return []


def validate_signing_keys_response(status, value, vehicle_id, headers=None, root=PROFILE):
    if status != 200:
        return ["status is not specified by profile"]
    headers = {key.lower(): item for key, item in (headers or {}).items()}
    if headers.get("cache-control") != "no-store":
        return ["signing keys response must be no-store"]
    return validate_signing_keys(value, vehicle_id, root)


def validate_http_error(status, value, headers=None, root=PROFILE):
    expected = {400: {"invalid_json"}, 404: {"vehicle_not_found"}, 406: {"schema_range_unsupported"}, 413: {"request_too_large"}, 422: {"invalid_request", "invalid_schema_range", "unknown_base_receipt"}}
    if status not in expected:
        return ["status does not carry a sync error"]
    errors = list(Draft202012Validator(_schema(root, "sync-error.schema.json", "error")).iter_errors(value))
    if errors:
        return ["sync error violates schema"]
    if value["code"] not in expected[status]:
        return ["sync error code does not match status"]
    headers = {key.lower(): item for key, item in (headers or {}).items()}
    if status == 406 and headers.get("cache-control") != "no-store":
        return ["schema range unavailable must be no-store"]
    return []


def validate_response(status, value, vehicle_id, headers=None, root=PROFILE):
    if status == 200:
        if isinstance(value, dict) and value.get("kind") == "no_op":
            return validate_noop(status, value, vehicle_id, root=root)
        errors = list(Draft202012Validator(_schema(root, "changed-set-receipt.schema.json", "receipt"), format_checker=FormatChecker()).iter_errors(value))
        if errors:
            return ["changed-set receipt violates schema"]
        if value["vehicle_id"] != vehicle_id:
            return ["signed response vehicle does not match route"]
        if value["to_sequence"] <= value["from_sequence"]:
            return ["changed-set receipt does not advance"]
        if value["manifest_schema"] != value["base_manifest_schema"]:
            return ["changed-set schema differs from base"]
        pack_errors = _validate_pack_limits([value["pack"]], root)
        return pack_errors or _validate_signature(value, vehicle_id, root)
    if status == 409:
        errors = list(Draft202012Validator(_schema(root, "rebase-hint.schema.json", "hint"), format_checker=FormatChecker()).iter_errors(value))
        if errors:
            return ["rebase hint violates schema"]
        if value["vehicle_id"] != vehicle_id:
            return ["signed response vehicle does not match route"]
        replacement = value["replacement"]
        retry = value["retry_request"]
        if retry["base_receipt_id"] != replacement["receipt_id"] or retry["from_sequence"] != replacement["sequence"] or retry["base_manifest_schema"] != replacement["manifest_schema"]:
            return ["rebase retry does not bind replacement"]
        minimum = _version(retry["schema_version_range"]["minimum"])
        maximum = _version(retry["schema_version_range"]["maximum"])
        replacement_schema = _version(replacement["manifest_schema"])
        if minimum > maximum:
            return ["schema version range is reversed"]
        if not minimum <= replacement_schema <= maximum:
            return ["rebase replacement schema is outside accepted range"]
        if replacement["manifest_schema"] == "2.1":
            packs = [replacement["pack"]]
        else:
            chunks = replacement["chunks"]
            if [item["chunk_index"] for item in chunks] != list(range(len(chunks))):
                return ["rebase chunks are not contiguous"]
            packs = [item["pack"] for item in chunks]
        pack_errors = _validate_pack_limits(packs, root)
        return pack_errors or _validate_signature(value, vehicle_id, root)
    if status in {400, 404, 406, 413, 422}:
        return validate_http_error(status, value, headers, root)
    return ["status is not specified by profile"]


def _validate_pack_limits(packs, root):
    limits = load_profile(root)["limits"]
    if any(not limits["min_pack_compressed_bytes"] <= item["compressed_bytes"] <= limits["max_pack_compressed_bytes"] for item in packs):
        return ["pack compressed bytes exceed profile limits"]
    return []


def validate_manifest(value, vehicle_id, root=PROFILE):
    errors = list(Draft202012Validator(_schema(root, "sync-manifest.schema.json", "manifest"), format_checker=FormatChecker()).iter_errors(value))
    if errors:
        return ["sync manifest violates schema"]
    if value["vehicle_id"] != vehicle_id:
        return ["signed response vehicle does not match route"]
    if value["schema_version"] == "2.1":
        return _validate_pack_limits([value["pack"]], root) + _validate_signature(value, vehicle_id, root)
    chunks = value["chunks"]
    if [item["chunk_index"] for item in chunks] != list(range(len(chunks))):
        return ["manifest chunks are not contiguous"]
    pack_errors = _validate_pack_limits([item["pack"] for item in chunks], root)
    return pack_errors or _validate_signature(value, vehicle_id, root)


def materialize_body_fixture(value):
    compact = json.dumps(value["document"], ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    raw = compact + b" " * value["padding_bytes"]
    if len(raw) != value["body_bytes"] or hashlib.sha256(raw).hexdigest() != value["body_sha256"]:
        raise ContractError("body fixture binding is invalid")
    return raw


def validate_control_response_body(raw, response_type, status, vehicle_id, headers=None, root=PROFILE):
    limit = load_profile(root)["limits"]["max_response_bytes"]
    if len(raw) > limit:
        return [f"response body exceeds {limit} bytes"]
    try:
        value = strict_json(raw)
    except (UnicodeDecodeError, ValueError, json.JSONDecodeError):
        return ["response body is not valid JSON"]
    if response_type == "manifest":
        if status != 200:
            return ["status is not specified by profile"]
        return validate_manifest(value, vehicle_id, root)
    if response_type in {"changed_set", "rebase"}:
        return validate_response(status, value, vehicle_id, headers, root)
    if response_type == "noop":
        return validate_noop(status, value, vehicle_id, headers, root=root)
    if response_type == "error":
        return validate_http_error(status, value, headers, root)
    if response_type == "signing_keys":
        return validate_signing_keys_response(status, value, vehicle_id, headers, root)
    if response_type == "prepared_artefact":
        if status != 200:
            return ["status is not specified by profile"]
        return validate_prepared_artefact(value, vehicle_id, root)
    return ["response type is not specified by profile"]


def validate_manifest_body(raw, vehicle_id, root=PROFILE):
    return validate_control_response_body(raw, "manifest", 200, vehicle_id, root=root)


def validate_noop(status, value, vehicle_id, headers=None, raw=b"", root=PROFILE):
    if status == 200:
        errors = list(Draft202012Validator(_schema(root, "noop.schema.json", "receipt"), format_checker=FormatChecker()).iter_errors(value))
        if errors:
            return ["no-op receipt violates schema"]
        if value["vehicle_id"] != vehicle_id:
            return ["signed response vehicle does not match route"]
        if value["manifest_schema"] != value["base_manifest_schema"]:
            return ["no-op schema differs from base"]
        return _validate_signature(value, vehicle_id, root)
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
        "POST /v1/vehicles/{vehicle_id}/sync/changes-since": {200, 400, 401, 404, 406, 409, 413, 422},
        "GET /v1/vehicles/{vehicle_id}/sync/manifest": {200, 401, 406},
        "GET /v1/vehicles/{vehicle_id}/sync/noop": {200, 401, 406},
        "GET /v1/packs/sha256/{object_name}": {200, 206, 401, 404},
        "GET /v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}": {200, 401, 404},
        "GET /v1/vehicles/{vehicle_id}/sync/signing-keys": {200, 401, 404},
    }
    actual = {item["route"]: {response["status"] for response in item["responses"]} for item in value["tables"]}
    if actual != expected:
        return ["status tables are incomplete"]
    return []


def validate_prepared_artefact(value, vehicle_id, root=PROFILE):
    errors = list(Draft202012Validator(_schema(root, "prepared-artefact.schema.json", "receipt"), format_checker=FormatChecker()).iter_errors(value))
    if errors:
        return ["prepared-artefact receipt violates schema"]
    if value["vehicle_id"] != vehicle_id:
        return ["signed response vehicle does not match route"]
    window = value["window"]
    if window["from_ms"] >= window["to_ms"]:
        return ["prepared-artefact window is invalid"]
    spans = value["dirty_spans"]
    if len({span["month"] for span in spans["map_months"]}) != len(spans["map_months"]):
        return ["prepared-artefact repeats map month"]
    for span in spans["map_months"]:
        if span["from_ms"] >= span["to_ms"] or span["from_ms"] < window["from_ms"] or span["to_ms"] > window["to_ms"]:
            return ["prepared-artefact dirty span is outside window"]
    if value["artifact_id"].startswith("map-month-v1."):
        if len(spans["map_months"]) != 1:
            return ["identified prepared artefact must contain one full UTC month"]
        span = spans["map_months"][0]
        try:
            start = datetime.fromtimestamp(window["from_ms"] / 1000, timezone.utc)
        except (OverflowError, ValueError):
            return ["identified prepared artefact must contain one full UTC month"]
        if (start.day, start.hour, start.minute, start.second, start.microsecond) != (1, 0, 0, 0, 0) or start.strftime("%Y-%m") != span["month"]:
            return ["identified prepared artefact must contain one full UTC month"]
        next_year = start.year + (start.month == 12)
        next_month = start.month % 12 + 1
        end = datetime(next_year, next_month, 1, tzinfo=timezone.utc)
        if (window["to_ms"] != int(end.timestamp() * 1000)
                or span["from_ms"] != window["from_ms"]
                or span["to_ms"] != window["to_ms"]):
            return ["identified prepared artefact must contain one full UTC month"]
        if value["artifact_id"] != prepared_month_artifact_id(value, span["month"]):
            return ["prepared artefact identity mismatch"]
    if value["generation"]["generated_at_ms"] < window["to_ms"]:
        return ["prepared-artefact generation predates window"]
    pack_errors = _validate_pack_limits([value["pack"]], root)
    return pack_errors or _validate_signature(value, vehicle_id, root)


def _read_prepared_pack_fixture(value, root):
    name = value.get("pack_file")
    if not isinstance(name, str) or name.startswith("/") or ".." in Path(name).parts:
        raise ContractError("prepared pack fixture path is invalid")
    path = Path(root) / name
    if not path.is_file():
        raise ContractError("prepared pack fixture is missing")
    return path.read_bytes()


def _prepared_schema_rows(connection):
    return connection.execute(
        "SELECT type, name, tbl_name, sql FROM sqlite_schema "
        "WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name"
    ).fetchall()


def _prepared_schema_sha256(rows):
    encoded = json.dumps(rows, ensure_ascii=False, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def _expected_prepared_schema(root, contract):
    schema_path = Path(root) / contract["sqlite"]["schema"]
    try:
        schema_bytes = schema_path.read_bytes()
    except OSError:
        return None
    if hashlib.sha256(schema_bytes).hexdigest() != contract["sqlite"]["schema_sha256"]:
        return None
    connection = sqlite3.connect(":memory:")
    try:
        connection.execute("PRAGMA trusted_schema = OFF")
        connection.executescript(schema_bytes.decode("utf-8"))
        rows = _prepared_schema_rows(connection)
        if _prepared_schema_sha256(rows) != contract["sqlite"]["sqlite_schema_sha256"]:
            return None
        xinfo = {
            table: connection.execute(f"PRAGMA table_xinfo('{table}')").fetchall()
            for table in contract["sqlite"]["allowed_tables"]
        }
        foreign_keys = {
            table: connection.execute(f"PRAGMA foreign_key_list('{table}')").fetchall()
            for table in contract["sqlite"]["allowed_tables"]
        }
        return rows, xinfo, foreign_keys
    except (UnicodeDecodeError, sqlite3.Error):
        return None
    finally:
        connection.close()


def _sqlite_varint(data, offset, limit):
    start = offset
    value = 0
    for index in range(9):
        if offset >= limit:
            raise ValueError("truncated SQLite varint")
        byte = data[offset]
        offset += 1
        if index == 8:
            value = (value << 8) | byte
            break
        value = (value << 7) | (byte & 0x7F)
        if byte < 0x80:
            break
    else:
        raise ValueError("invalid SQLite varint")
    if data[start:offset] != _sqlite_encode_varint(value):
        raise ValueError("non-canonical SQLite varint")
    return value, offset


def _sqlite_encode_varint(value):
    if type(value) is not int or not 0 <= value <= 0xFFFFFFFFFFFFFFFF:
        raise ValueError("SQLite varint value is outside unsigned 64-bit range")
    if value > 0x00FFFFFFFFFFFFFF:
        encoded = bytearray(9)
        encoded[8] = value & 0xFF
        value >>= 8
        for index in range(7, -1, -1):
            encoded[index] = (value & 0x7F) | 0x80
            value >>= 7
        return bytes(encoded)
    groups = [value & 0x7F]
    value >>= 7
    while value:
        groups.append(value & 0x7F)
        value >>= 7
    groups.reverse()
    return bytes(
        group | (0x80 if index < len(groups) - 1 else 0)
        for index, group in enumerate(groups)
    )


def _sqlite_canonical_serial(value):
    if value is None:
        return 0, b""
    if type(value) is int:
        if value == 0:
            return 8, b""
        if value == 1:
            return 9, b""
        for serial_type, byte_count in ((1, 1), (2, 2), (3, 3), (4, 4), (5, 6), (6, 8)):
            minimum = -(1 << (byte_count * 8 - 1))
            maximum = (1 << (byte_count * 8 - 1)) - 1
            if minimum <= value <= maximum:
                return serial_type, value.to_bytes(byte_count, "big", signed=True)
        raise ValueError("SQLite integer is outside signed 64-bit range")
    if type(value) is float:
        if not math.isfinite(value):
            raise ValueError("non-finite SQLite real is unsupported")
        return 7, struct.pack(">d", value)
    if isinstance(value, bytes):
        return 12 + 2 * len(value), value
    if type(value) is str:
        encoded = value.encode("utf-8")
        return 13 + 2 * len(encoded), encoded
    raise ValueError("unsupported SQLite record value")


def _sqlite_record(payload, expected_fields):
    header_size, offset = _sqlite_varint(payload, 0, len(payload))
    if not offset <= header_size <= len(payload):
        raise ValueError("invalid SQLite record header size")
    serial_types = []
    while offset < header_size:
        serial_type, offset = _sqlite_varint(payload, offset, header_size)
        serial_types.append(serial_type)
        if len(serial_types) > expected_fields:
            raise ValueError("SQLite record has surplus fields")
    if offset != header_size or len(serial_types) != expected_fields:
        raise ValueError("SQLite record field count is not exact")

    values = []
    offset = header_size
    for serial_type in serial_types:
        if serial_type == 0:
            byte_count = 0
            value = None
        elif 1 <= serial_type <= 6:
            byte_count = (1, 2, 3, 4, 6, 8)[serial_type - 1]
            end = offset + byte_count
            if end > len(payload):
                raise ValueError("truncated SQLite integer")
            value = int.from_bytes(payload[offset:end], "big", signed=True)
        elif serial_type == 7:
            byte_count = 8
            end = offset + byte_count
            if end > len(payload):
                raise ValueError("truncated SQLite real")
            value = struct.unpack(">d", payload[offset:end])[0]
        elif serial_type == 8:
            byte_count = 0
            value = 0
        elif serial_type == 9:
            byte_count = 0
            value = 1
        elif serial_type in {10, 11}:
            raise ValueError("reserved SQLite serial type")
        else:
            byte_count = (serial_type - (12 if serial_type % 2 == 0 else 13)) // 2
            end = offset + byte_count
            if end > len(payload):
                raise ValueError("truncated SQLite string or blob")
            encoded = payload[offset:end]
            value = encoded if serial_type % 2 == 0 else encoded.decode("utf-8")
        end = offset + byte_count
        encoded = payload[offset:end]
        canonical_type, canonical_bytes = _sqlite_canonical_serial(value)
        if serial_type != canonical_type or encoded != canonical_bytes:
            raise ValueError("non-canonical SQLite serial encoding")
        values.append(value)
        offset = end
    if offset != len(payload):
        raise ValueError("SQLite record has trailing body bytes")
    return tuple(values)


def _sqlite_local_payload_bytes(payload_bytes, usable_bytes, page_type):
    minimum = ((usable_bytes - 12) * 32 // 255) - 23
    if page_type == 0x0D:
        maximum = usable_bytes - 35
    else:
        maximum = ((usable_bytes - 12) * 64 // 255) - 23
    if payload_bytes <= maximum:
        return payload_bytes
    candidate = minimum + ((payload_bytes - minimum) % (usable_bytes - 4))
    return candidate if candidate <= maximum else minimum


def _prepared_sqlite_byte_domain_is_canonical(raw, connection, contract):
    try:
        raw = bytes(raw)
        page_size = contract["sqlite"]["page_size"]
        page_count = len(raw) // page_size
        if (
            not 1 <= page_count <= contract["sqlite"]["max_page_count"]
            or raw[:16] != b"SQLite format 3\x00"
            or len(raw) != page_count * page_size
        ):
            return False
        if int.from_bytes(raw[16:18], "big") != page_size:
            return False
        if raw[18:24] != bytes((1, 1, 0, 64, 32, 32)):
            return False
        if int.from_bytes(raw[28:32], "big") != page_count:
            return False
        if raw[32:40] != bytes(8):
            return False
        if int.from_bytes(raw[40:44], "big") != 4:
            return False
        if int.from_bytes(raw[44:48], "big") != 4:
            return False
        if raw[48:56] != bytes(8):
            return False
        if int.from_bytes(raw[56:60], "big") != 1:
            return False
        if int.from_bytes(raw[60:64], "big") != contract["sqlite"]["user_version"]:
            return False
        if raw[64:68] != bytes(4):
            return False
        if int.from_bytes(raw[68:72], "big") != contract["sqlite"]["application_id"]:
            return False
        if (
            int.from_bytes(raw[24:28], "big")
            != contract["sqlite"]["file_change_counter"]
            or raw[72:92] != bytes(20)
            or raw[24:28] != raw[92:96]
        ):
            return False
        if int.from_bytes(raw[96:100], "big") not in contract["sqlite"]["writer_versions"]:
            return False

        root_rows = connection.execute(
            "SELECT name, rootpage FROM sqlite_schema "
            "WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        ).fetchall()
        roots = [(1, "sqlite_schema")]
        roots.extend(
            (rootpage, name)
            for name, rootpage in root_rows
        )
        table_arities = {
            name: len(connection.execute(f"PRAGMA table_xinfo('{name}')").fetchall())
            for name, _ in root_rows
        }
        logical_rows = {
            name: Counter(connection.execute(f'SELECT * FROM "{name}"').fetchall())
            for name, _ in root_rows
        }
        logical_schema_rows = Counter(
            connection.execute(
                "SELECT rowid, type, name, tbl_name, rootpage, sql "
                "FROM sqlite_schema ORDER BY rowid"
            ).fetchall()
        )
        physical_rows = {name: [] for name, _ in root_rows}
        physical_schema_rows = []
        pending = []
        scheduled_pages = set()
        page_roles = {}

        def page_bytes(page_number):
            if type(page_number) is not int or not 1 <= page_number <= page_count:
                raise ValueError("SQLite page number is outside the image")
            start = (page_number - 1) * page_size
            return raw[start:start + page_size]

        def claim_page(page_number, role):
            if page_number in page_roles or page_number in scheduled_pages:
                raise ValueError("SQLite page is multiply referenced")
            if len(page_roles) >= contract["sqlite"]["max_page_count"]:
                raise ValueError("SQLite page traversal exceeded its bound")
            page_roles[page_number] = role

        def schedule_btree(page_number, depth, table_name):
            if type(page_number) is not int or not 1 <= page_number <= page_count:
                raise ValueError("SQLite B-tree page number is outside the image")
            if page_number in page_roles or page_number in scheduled_pages:
                raise ValueError("SQLite page is multiply referenced")
            if len(page_roles) + len(scheduled_pages) >= page_count:
                raise ValueError("SQLite page traversal exceeded its bound")
            scheduled_pages.add(page_number)
            pending.append((page_number, depth, table_name))

        for root_page, table_name in reversed(roots):
            schedule_btree(root_page, 0, table_name)

        def follow_overflow(first_page, remaining_bytes):
            page_number = first_page
            chunks = []
            while remaining_bytes:
                claim_page(page_number, "overflow")
                page = page_bytes(page_number)
                next_page = int.from_bytes(page[0:4], "big")
                used = min(remaining_bytes, page_size - 4)
                chunks.append(page[4:4 + used])
                if any(page[4 + used:]):
                    raise ValueError("SQLite overflow page has nonzero unused bytes")
                remaining_bytes -= used
                if remaining_bytes:
                    if next_page == 0:
                        raise ValueError("SQLite overflow chain is truncated")
                elif next_page != 0:
                    raise ValueError("SQLite overflow chain has a trailing page")
                page_number = next_page
            return b"".join(chunks)

        while pending:
            page_number, depth, table_name = pending.pop()
            scheduled_pages.remove(page_number)
            if depth > contract["sqlite"]["max_btree_depth"]:
                return False
            claim_page(page_number, "btree")
            page = page_bytes(page_number)
            header_offset = 100 if page_number == 1 else 0
            page_type = page[header_offset]
            if page_type not in {0x02, 0x05, 0x0A, 0x0D}:
                return False
            if table_name == "sqlite_schema":
                if page_type not in {0x05, 0x0D}:
                    return False
            elif page_type not in {0x02, 0x0A}:
                return False
            interior = page_type in {0x02, 0x05}
            header_bytes = 12 if interior else 8
            first_freeblock = int.from_bytes(page[header_offset + 1:header_offset + 3], "big")
            cell_count = int.from_bytes(page[header_offset + 3:header_offset + 5], "big")
            if cell_count > contract["sqlite"]["max_cells_per_page"]:
                return False
            content_start = int.from_bytes(page[header_offset + 5:header_offset + 7], "big")
            fragmented_bytes = page[header_offset + 7]
            if content_start == 0:
                content_start = page_size
            pointer_start = header_offset + header_bytes
            pointer_end = pointer_start + cell_count * 2
            if pointer_end > content_start or content_start > page_size:
                return False
            used = bytearray(page_size)
            used[:100 if page_number == 1 else 0] = b"\x01" * (100 if page_number == 1 else 0)
            used[header_offset:pointer_end] = b"\x01" * (pointer_end - header_offset)
            live = bytearray(used)
            freeblock_offset = first_freeblock
            freeblock_count = 0
            freeblock_ranges = []
            while freeblock_offset:
                freeblock_count += 1
                if freeblock_count > page_size // 4:
                    return False
                if not content_start <= freeblock_offset <= page_size - 4:
                    return False
                next_freeblock = int.from_bytes(
                    page[freeblock_offset:freeblock_offset + 2], "big"
                )
                freeblock_size = int.from_bytes(
                    page[freeblock_offset + 2:freeblock_offset + 4], "big"
                )
                freeblock_end = freeblock_offset + freeblock_size
                if (
                    freeblock_size < 4
                    or freeblock_end > page_size
                    or any(used[freeblock_offset:freeblock_end])
                    or any(page[freeblock_offset + 4:freeblock_end])
                    or (next_freeblock and next_freeblock < freeblock_end)
                ):
                    return False
                used[freeblock_offset:freeblock_end] = b"\x01" * freeblock_size
                freeblock_ranges.append((freeblock_offset, freeblock_end))
                freeblock_offset = next_freeblock
            if interior:
                right_child = int.from_bytes(
                    page[header_offset + 8:header_offset + 12], "big"
                )
                schedule_btree(right_child, depth + 1, table_name)

            cell_offsets = [
                int.from_bytes(page[offset:offset + 2], "big")
                for offset in range(pointer_start, pointer_end, 2)
            ]
            if len(set(cell_offsets)) != len(cell_offsets):
                return False
            for cell_offset in cell_offsets:
                if not content_start <= cell_offset < page_size:
                    return False
                offset = cell_offset
                if interior:
                    left_child = int.from_bytes(page[offset:offset + 4], "big")
                    schedule_btree(left_child, depth + 1, table_name)
                    offset += 4
                if page_type == 0x05:
                    _, offset = _sqlite_varint(page, offset, page_size)
                    payload_bytes = 0
                    local_bytes = 0
                else:
                    payload_bytes, offset = _sqlite_varint(page, offset, page_size)
                    if payload_bytes > len(raw):
                        return False
                    if page_type == 0x0D:
                        rowid, offset = _sqlite_varint(page, offset, page_size)
                    local_bytes = _sqlite_local_payload_bytes(
                        payload_bytes, page_size, page_type
                    )
                cell_end = offset + local_bytes
                overflow_bytes = payload_bytes - local_bytes
                overflow_page = 0
                if overflow_bytes:
                    if cell_end + 4 > page_size:
                        return False
                    overflow_page = int.from_bytes(page[cell_end:cell_end + 4], "big")
                    cell_end += 4
                if cell_end > page_size or any(used[cell_offset:cell_end]):
                    return False
                used[cell_offset:cell_end] = b"\x01" * (cell_end - cell_offset)
                live[cell_offset:cell_end] = b"\x01" * (cell_end - cell_offset)
                payload = page[offset:offset + local_bytes]
                if overflow_bytes:
                    payload += follow_overflow(overflow_page, overflow_bytes)
                if len(payload) != payload_bytes:
                    return False
                if page_type == 0x0D:
                    physical_schema_rows.append(
                        (rowid, *_sqlite_record(payload, 5))
                    )
                elif page_type in {0x02, 0x0A}:
                    record = _sqlite_record(payload, table_arities[table_name])
                    physical_rows[table_name].append(record)
            actual_fragmented_bytes = 0
            expected_freeblock_ranges = []
            index = content_start
            while index < page_size:
                if live[index]:
                    index += 1
                    continue
                fragment_start = index
                while index < page_size and not live[index]:
                    index += 1
                fragment_bytes = index - fragment_start
                if fragment_bytes >= 4:
                    expected_freeblock_ranges.append((fragment_start, index))
                else:
                    actual_fragmented_bytes += fragment_bytes
            if (
                expected_freeblock_ranges != freeblock_ranges
                or actual_fragmented_bytes != fragmented_bytes
            ):
                return False
            if any(byte for index, byte in enumerate(page) if not used[index]):
                return False
        return (
            len(page_roles) == page_count
            and Counter(physical_schema_rows) == logical_schema_rows
            and all(Counter(physical_rows[name]) == rows for name, rows in logical_rows.items())
        )
    except (IndexError, OverflowError, TypeError, UnicodeError, ValueError, sqlite3.Error):
        return False


def validate_prepared_pack(receipt, compressed, vehicle_id, root=PROFILE):
    receipt_errors = validate_prepared_artefact(receipt, vehicle_id, root)
    if receipt_errors:
        return receipt_errors
    profile = load_profile(root)
    contract = strict_json((Path(root) / profile["prepared_pack_contract"]).read_bytes())
    pack = receipt["pack"]
    if len(compressed) != pack["compressed_bytes"]:
        return ["prepared pack compressed size does not match receipt"]
    if hashlib.sha256(compressed).hexdigest() != pack["sha256"]:
        return ["prepared pack digest does not match receipt"]
    if not pack["object_name"].endswith(".sqlite.zst"):
        return ["prepared pack object name is unsupported"]
    try:
        declared_size = zstandard.frame_content_size(compressed)
    except zstandard.ZstdError:
        return ["prepared pack is not a valid zstd frame"]
    if declared_size in {zstandard.CONTENTSIZE_UNKNOWN, zstandard.CONTENTSIZE_ERROR}:
        return ["prepared pack zstd content size is required"]
    if declared_size != pack["uncompressed_bytes"]:
        return ["prepared pack uncompressed size does not match receipt"]
    if declared_size > contract["limits"]["max_uncompressed_bytes"]:
        return ["prepared pack uncompressed size exceeds limit"]
    try:
        raw = zstandard.ZstdDecompressor().decompress(
            compressed,
            max_output_size=contract["limits"]["max_uncompressed_bytes"],
            allow_extra_data=False,
        )
    except zstandard.ZstdError:
        return ["prepared pack zstd payload is invalid"]
    if len(raw) != pack["uncompressed_bytes"]:
        return ["prepared pack uncompressed size does not match receipt"]

    with tempfile.TemporaryDirectory(prefix="teslatlas-prepared-validate-") as directory:
        path = Path(directory) / "pack.sqlite"
        path.write_bytes(raw)
        try:
            connection = sqlite3.connect(f"file:{path}?mode=ro&immutable=1", uri=True)
            try:
                connection.execute("PRAGMA trusted_schema = OFF")
                connection.execute("PRAGMA query_only = ON")
                if connection.execute("PRAGMA trusted_schema").fetchone() != (0,):
                    return ["prepared pack SQLite schema is unsupported"]
                if connection.execute("PRAGMA query_only").fetchone() != (1,):
                    return ["prepared pack SQLite schema is unsupported"]
                if connection.execute("PRAGMA integrity_check").fetchall() != [("ok",)]:
                    return ["prepared pack SQLite integrity check failed"]
                if connection.execute("PRAGMA application_id").fetchone()[0] != contract["sqlite"]["application_id"]:
                    return ["prepared pack SQLite application id is unsupported"]
                if connection.execute("PRAGMA user_version").fetchone()[0] != contract["sqlite"]["user_version"]:
                    return ["prepared pack payload version is unsupported"]
                page_size = connection.execute("PRAGMA page_size").fetchone()[0]
                if page_size != contract["sqlite"]["page_size"]:
                    return ["prepared pack SQLite page size is unsupported"]
                page_count = connection.execute("PRAGMA page_count").fetchone()[0]
                if len(raw) != page_count * page_size:
                    return ["prepared pack SQLite image is not canonical"]
                if connection.execute("PRAGMA freelist_count").fetchone() != (0,):
                    return ["prepared pack SQLite image is not canonical"]
                if connection.execute("PRAGMA encoding").fetchone()[0] != contract["sqlite"]["encoding"]:
                    return ["prepared pack SQLite encoding is unsupported"]
                schema_rows = _prepared_schema_rows(connection)
                if any("route" in name for _, name, _, _ in schema_rows):
                    return ["prepared pack contains unsupported route content"]
                expected_schema = _expected_prepared_schema(root, contract)
                if expected_schema is None:
                    return ["prepared pack SQLite schema is unsupported"]
                expected_rows, expected_xinfo, expected_foreign_keys = expected_schema
                if (
                    schema_rows != expected_rows
                    or _prepared_schema_sha256(schema_rows)
                    != contract["sqlite"]["sqlite_schema_sha256"]
                ):
                    return ["prepared pack SQLite schema is unsupported"]
                for table in contract["sqlite"]["allowed_tables"]:
                    if connection.execute(f"PRAGMA table_xinfo('{table}')").fetchall() != expected_xinfo[table]:
                        return ["prepared pack SQLite schema is unsupported"]
                    if connection.execute(f"PRAGMA foreign_key_list('{table}')").fetchall() != expected_foreign_keys[table]:
                        return ["prepared pack SQLite schema is unsupported"]
                if connection.execute("PRAGMA foreign_key_check").fetchall():
                    return ["prepared pack SQLite foreign keys are invalid"]
                metadata_rows = connection.execute(
                    "SELECT singleton, payload, payload_version, scope, map_style, tile_geometry_version, artifact_schema_version, artifact_id, vehicle_id, input_manifest_id, input_receipt_id, input_manifest_schema, input_sequence, algorithm_version, window_from_ms, window_to_ms FROM prepared_metadata"
                ).fetchall()
                if len(metadata_rows) != 1:
                    return ["prepared pack metadata is invalid"]
                metadata = metadata_rows[0]
                if metadata[1] != contract["contract_id"] or metadata[2] != contract["sqlite"]["user_version"]:
                    return ["prepared pack payload version is unsupported"]
                expected_metadata = (
                    1,
                    receipt["payload"],
                    contract["sqlite"]["user_version"],
                    receipt["scope"],
                    receipt["map_style"],
                    receipt["tile_geometry_version"],
                    receipt["artifact_schema_version"],
                    receipt["artifact_id"],
                    receipt["vehicle_id"],
                    receipt["source"]["input_manifest_id"],
                    receipt["source"]["input_receipt_id"],
                    receipt["source"]["input_manifest_schema"],
                    receipt["source"]["input_sequence"],
                    receipt["algorithm_version"],
                    receipt["window"]["from_ms"],
                    receipt["window"]["to_ms"],
                )
                if metadata != expected_metadata:
                    return ["prepared pack metadata does not match receipt"]
                months = connection.execute(
                    "SELECT month, from_ms, to_ms, resolution, drive_count, tile_count FROM map_months ORDER BY month"
                ).fetchall()
                expected_months = sorted(
                    (
                        item["month"],
                        item["from_ms"],
                        item["to_ms"],
                        item["resolution"],
                        item["drive_count"],
                        item["tile_count"],
                    )
                    for item in receipt["dirty_spans"]["map_months"]
                )
                if months != expected_months:
                    return ["prepared pack spans do not match receipt"]
                tiles = connection.execute(
                    "SELECT month, zoom, tile_x, tile_y, segments FROM map_tiles ORDER BY month, zoom, tile_x, tile_y"
                ).fetchall()
                month_names = {item[0] for item in months}
                tiles_by_month = {month: [] for month in month_names}
                for month, zoom, tile_x, tile_y, segments in tiles:
                    if (
                        month not in month_names
                        or type(zoom) is not int
                        or not contract["map_tiles"]["zoom_min"] <= zoom <= contract["map_tiles"]["zoom_max"]
                        or type(tile_x) is not int
                        or type(tile_y) is not int
                        or not 0 <= tile_x < 1 << zoom
                        or not 0 <= tile_y < 1 << zoom
                        or not isinstance(segments, bytes)
                        or not 8 <= len(segments) <= contract["limits"]["max_tile_bytes"]
                        or len(segments) % 8
                        or len(segments) // 8
                        > contract["map_tiles"]["max_segments_per_tile"]
                    ):
                        return ["prepared pack tile is invalid"]
                    segment_tuples = [
                        struct.unpack_from("<hhhh", segments, offset)
                        for offset in range(0, len(segments), 8)
                    ]
                    if segment_tuples != sorted(set(segment_tuples)):
                        return ["prepared pack tile is invalid"]
                    tiles_by_month[month].append((zoom, tile_x, tile_y, segments))
                for month, _, _, resolution, _, declared_tile_count in months:
                    month_tiles = tiles_by_month[month]
                    if len(month_tiles) != declared_tile_count:
                        return ["prepared pack month resolution is invalid"]
                    if resolution == "readyEmpty":
                        if month_tiles:
                            return ["prepared pack month resolution is invalid"]
                        continue
                    if not month_tiles or len(month_tiles) > contract["limits"]["max_tiles_per_month"]:
                        return ["prepared pack month resolution is invalid"]
                    keys = [(zoom, tile_x, tile_y) for zoom, tile_x, tile_y, _ in month_tiles]
                    if keys != sorted(keys):
                        return ["prepared pack tile order is invalid"]
                    encoded_bytes = 8 + sum(
                        40 + len(segments) for _, _, _, segments in month_tiles
                    )
                    if encoded_bytes > contract["limits"]["max_tile_publication_bytes_per_month"]:
                        return ["prepared pack tile publication exceeds limit"]
                if not _prepared_sqlite_byte_domain_is_canonical(
                    raw, connection, contract
                ):
                    return ["prepared pack SQLite image is not canonical"]
            finally:
                connection.close()
        except sqlite3.Error:
            return ["prepared pack SQLite payload is invalid"]
    return []


def _case_errors(case, fixtures, root):
    if not isinstance(case, dict):
        return ["case is not an object"]
    validator = case.get("validator")
    if validator not in {"bootstrap_selection", "changes_since", "changes_since_request_error", "control_response_body", "manifest", "manifest_body", "noop", "prepared_artefact", "prepared_pack", "signing_keys"}:
        return ["case has an unknown validator"]
    status = case.get("status")
    if type(status) is not int:
        return ["case status is invalid"]

    if validator == "bootstrap_selection":
        request = fixtures.get(case.get("request_fixture"))
        if case.get("response_fixture") is not None:
            return ["bootstrap selection case has an unexpected response fixture"]
        return validate_bootstrap_selection(copy.deepcopy(request), status, root)

    vehicle_id = case.get("vehicle_id")
    request_value = None
    request_errors = []
    request_id = case.get("request_fixture")
    if request_id is not None:
        request = fixtures.get(request_id)
        if not isinstance(request, dict) or "request" not in request or not isinstance(request.get("vehicle_id"), str):
            return ["request fixture is missing"]
        if vehicle_id is None:
            vehicle_id = request["vehicle_id"]
        elif vehicle_id != request["vehicle_id"]:
            return ["case route vehicle does not match request fixture"]
        request_value = copy.deepcopy(request["request"])
        request_errors = validate_request(request_value, root)
        if request_errors and validator == "changes_since" and status != 406:
            return request_errors
    if not isinstance(vehicle_id, str):
        return ["case route vehicle is missing"]

    response_id = case.get("response_fixture")
    response = fixtures.get(response_id)
    if not isinstance(response, dict):
        return ["response fixture is missing"]
    if validator == "changes_since_request_error":
        value = response.get("response")
        if not isinstance(value, dict) or value.get("status") != status:
            return ["sync error fixture status does not match case"]
        code = value.get("body", {}).get("code") if isinstance(value.get("body"), dict) else None
        expected_request_errors = {
            "invalid_request": {"request violates changes-since schema"},
            "invalid_schema_range": {"schema version range is reversed", "base manifest schema is outside accepted range"},
        }
        if len(request_errors) != 1 or code not in expected_request_errors or request_errors[0] not in expected_request_errors[code]:
            return ["request fixture does not match sync error"]
        headers = {"Cache-Control": item for key, item in value.get("headers", {}).items() if key == "cache_control"}
        return validate_http_error(status, copy.deepcopy(value.get("body")), headers, root)
    if validator == "changes_since":
        if status == 200:
            value = response.get("receipt")
            if request_value is not None:
                if value.get("kind") == "no_op":
                    bound = (value.get("base_receipt_id"), value.get("sequence"), value.get("base_manifest_schema"))
                else:
                    bound = (value.get("base_receipt_id"), value.get("from_sequence"), value.get("base_manifest_schema"))
                expected = (request_value["base_receipt_id"], request_value["from_sequence"], request_value["base_manifest_schema"])
                if bound != expected:
                    return ["changes-since response does not bind request"]
            return validate_response(status, copy.deepcopy(value), vehicle_id, root=root)
        if status in {400, 404, 406, 413, 422}:
            value = response.get("response")
            if not isinstance(value, dict) or value.get("status") != status:
                return ["sync error fixture status does not match case"]
            if status == 406 and request_errors != ["schema version range is unsupported"]:
                return ["unsupported-range response lacks an unsupported request"]
            headers = {"Cache-Control": item for key, item in value.get("headers", {}).items() if key == "cache_control"}
            return validate_response(status, copy.deepcopy(value.get("body")), vehicle_id, headers, root)
        value = response.get("response")
        if status == 409 and request_value is not None:
            bound = (value.get("requested_base_receipt_id"), value.get("requested_from_sequence"), value.get("requested_base_manifest_schema"))
            expected = (request_value["base_receipt_id"], request_value["from_sequence"], request_value["base_manifest_schema"])
            if bound != expected or value.get("retry_request", {}).get("schema_version_range") != request_value["schema_version_range"]:
                return ["rebase response does not bind request"]
        return validate_response(status, copy.deepcopy(value), vehicle_id, root=root)
    if validator == "manifest":
        return validate_manifest(copy.deepcopy(response.get("manifest")), vehicle_id, root)
    if validator == "manifest_body":
        try:
            raw = materialize_body_fixture(response)
        except ContractError as error:
            return [str(error)]
        return validate_manifest_body(raw, vehicle_id, root)
    if validator == "control_response_body":
        response_type = case.get("response_type")
        if response.get("response_type") != response_type or response.get("status") != status:
            return ["control response fixture does not match case"]
        try:
            raw = materialize_body_fixture(response)
        except ContractError as error:
            return [str(error)]
        headers = {"Cache-Control": item for key, item in response.get("headers", {}).items() if key == "cache_control"}
        return validate_control_response_body(raw, response_type, status, vehicle_id, headers, root)
    if validator == "prepared_artefact":
        return validate_prepared_artefact(copy.deepcopy(response.get("receipt")), vehicle_id, root)
    if validator == "prepared_pack":
        try:
            compressed = _read_prepared_pack_fixture(response, root)
        except ContractError as error:
            return [str(error)]
        return validate_prepared_pack(copy.deepcopy(response.get("receipt")), compressed, vehicle_id, root)
    if validator == "signing_keys":
        value = response.get("response")
        if not isinstance(value, dict) or value.get("status") != status:
            return ["signing keys fixture status does not match case"]
        headers = {"Cache-Control": item for key, item in value.get("headers", {}).items() if key == "cache_control"}
        return validate_signing_keys_response(status, copy.deepcopy(value.get("body")), vehicle_id, headers, root)
    if status == 200:
        return validate_noop(status, copy.deepcopy(response.get("receipt")), vehicle_id, root=root)
    unavailable = response.get("response")
    if not isinstance(unavailable, dict):
        return ["no-op fixture is invalid"]
    if unavailable.get("status") != status:
        return ["no-op fixture status does not match case"]
    headers = {
        "Cache-Control": value
        for key, value in unavailable.get("headers", {}).items()
        if key == "cache_control"
    }
    raw = b"x" * unavailable.get("body_bytes", 0)
    return validate_noop(status, {}, vehicle_id, headers, raw, root)


def run_fixture_cases(root=PROFILE):
    profile = load_profile(root)
    cases_document = strict_json((Path(root) / "cases.json").read_bytes())
    if not isinstance(cases_document, dict) or cases_document.get("profile_id") != profile["profile_id"]:
        raise ContractError("fixture cases profile mismatch")
    cases = cases_document.get("cases")
    if not isinstance(cases, list) or not cases:
        raise ContractError("fixture cases are invalid")
    fixtures = load_fixtures(root)
    results = []
    seen = set()
    for case in cases:
        case_id = case.get("id") if isinstance(case, dict) else None
        if not isinstance(case_id, str) or not case_id or case_id in seen:
            raise ContractError("fixture case identifier is invalid")
        seen.add(case_id)
        expected = case.get("expected_errors", [])
        if not isinstance(expected, list) or not all(isinstance(item, str) for item in expected):
            raise ContractError("fixture case expected errors are invalid")
        errors = _case_errors(case, fixtures, root)
        results.append(
            {
                "case_id": case_id,
                "fixture_ids": [
                    value
                    for value in (case.get("request_fixture"), case.get("response_fixture"))
                    if isinstance(value, str)
                ],
                "errors": errors,
                "expected_errors": expected,
                "passed": errors == expected,
            }
        )
    return results
