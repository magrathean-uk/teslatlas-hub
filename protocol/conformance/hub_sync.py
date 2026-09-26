"""Deterministic validation for the source-neutral changes-since profile."""
import copy
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
    required = {"profile.json", "changes-since-request.schema.json", "changed-set-receipt.schema.json", "rebase-hint.schema.json", "sync-manifest.schema.json", "noop.schema.json", "prepared-artefact.schema.json", "signing-keys.schema.json", "fixture-signing-keys.json", "sync-error.schema.json", "status-tables.schema.json", "status-tables.json", "openapi.json", "cases.json"}
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
    expected = {400: {"invalid_json"}, 404: {"vehicle_not_found"}, 406: {"schema_range_unsupported"}, 413: {"request_too_large"}, 422: {"invalid_request", "invalid_schema_range"}}
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


def validate_manifest_body(raw, vehicle_id, root=PROFILE):
    limit = load_profile(root)["limits"]["max_response_bytes"]
    if len(raw) > limit:
        return [f"response body exceeds {limit} bytes"]
    try:
        value = strict_json(raw)
    except (UnicodeDecodeError, ValueError, json.JSONDecodeError):
        return ["response body is not valid JSON"]
    return validate_manifest(value, vehicle_id, root)


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
    return pack_errors or _validate_signature(value, vehicle_id, root)


def _case_errors(case, fixtures, root):
    if not isinstance(case, dict):
        return ["case is not an object"]
    validator = case.get("validator")
    if validator not in {"changes_since", "changes_since_request_error", "manifest", "manifest_body", "noop", "prepared_artefact", "signing_keys"}:
        return ["case has an unknown validator"]
    status = case.get("status")
    if type(status) is not int:
        return ["case status is invalid"]

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
        if request_errors != ["request violates changes-since schema"]:
            return ["request fixture is not schema-invalid"]
        value = response.get("response")
        if not isinstance(value, dict) or value.get("status") != status:
            return ["sync error fixture status does not match case"]
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
    if validator == "prepared_artefact":
        return validate_prepared_artefact(copy.deepcopy(response.get("receipt")), vehicle_id, root)
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
