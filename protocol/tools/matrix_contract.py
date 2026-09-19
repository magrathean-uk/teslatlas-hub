"""Pure semantic admission for the installed Protocol HTTP matrix adapter.

The installed runner performs common file, process, and controller checks.  This
module only evaluates the Protocol-owned case contract and is deliberately free
of imports from the conformance runner or from an installed Hub.
"""

from collections.abc import Mapping
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
from types import MappingProxyType
from typing import Literal
import uuid


ADAPTER_ID = "protocol_actual_hub"
CONTRACT_REVISION = 1
PRODUCT_VERSION = "2026.36.2"
PROFILE_ID = "hub-http-v1"
PROFILE_REVISION = "1.0.0"
ACTOR_IDS = ("protocol_http",)
ACTOR_KIND = "protocol_http"
RUNTIME_REF = "root_python"
ENTRYPOINT_REF = "protocol_actual_hub"
SOURCE_ROLE = "protocol_source"
UNKNOWN_VEHICLE = "33333333-3333-4333-8333-333333333333"
UNICODE_NAME = "Interop – Árvíztűrő 🚗"
HEX64 = re.compile(r"^[0-9a-f]{64}$")
TOKEN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
REQUEST_ID = re.compile(r"^[A-Za-z0-9._:-]{1,128}$")


REQUIRED_CASES = (
    "candidate_artifact_identity",
    "installed_service_runtime",
    "discovery_identity_profile",
    "unauthenticated_discovery",
    "bad_invitation",
    "expired_invitation",
    "replayed_invitation",
    "real_auth",
    "credential_lifecycle_reauth",
    "revocation",
    "unknown_vehicle",
    "exact_current_values",
    "endpoint_restart",
    "outage_recovery",
    "unsupported_operation_zero_requests",
    "credential_rotation_api",
    "drives_three_page_order",
    "drives_terminal_cursor",
    "drives_etag_304",
    "drives_wrong_vehicle_cursor",
    "drives_wrong_filter_cursor",
)


CASE_OPERATIONS = MappingProxyType(
    {
        "candidate_artifact_identity": ("observe_identity",),
        "installed_service_runtime": ("observe_identity",),
        "discovery_identity_profile": ("discovery",),
        "unauthenticated_discovery": ("unauthenticated_probes",),
        "bad_invitation": ("bad_invitation",),
        "expired_invitation": ("expired_invitation",),
        "replayed_invitation": ("replayed_invitation",),
        "real_auth": ("real_auth",),
        "credential_lifecycle_reauth": ("reauthentication",),
        "revocation": ("revoked_credential",),
        "unknown_vehicle": ("unknown_vehicle",),
        "exact_current_values": ("exact_current",),
        "endpoint_restart": ("endpoint_restart",),
        "outage_recovery": ("outage_recovery",),
        "unsupported_operation_zero_requests": ("unsupported_operations",),
        "credential_rotation_api": ("credential_rotation",),
        "drives_three_page_order": ("drives_three_pages",),
        "drives_terminal_cursor": ("drives_terminal_cursor",),
        "drives_etag_304": ("drives_etag",),
        "drives_wrong_vehicle_cursor": ("wrong_vehicle_cursor",),
        "drives_wrong_filter_cursor": ("wrong_filter_cursor",),
    }
)


CASE_KINDS = MappingProxyType(
    {
        case_id: (
            "identity"
            if case_id in {"candidate_artifact_identity", "installed_service_runtime"}
            else "zero_request"
            if case_id
            in {
                "expired_invitation",
                "unsupported_operation_zero_requests",
            }
            else "http"
        )
        for case_id in REQUIRED_CASES
    }
)


DECISION_CODES = frozenset(
    {
        "accepted",
        "runner_owned_service_runtime",
        "case_shape",
        "unknown_case",
        "wrong_context",
        "wrong_actor",
        "wrong_source_role",
        "wrong_artifact_role",
        "installed_manifest_mismatch",
        "invocation_missing",
        "operation_mismatch",
        "sequence_mismatch",
        "controller_mismatch",
        "raw_missing",
        "raw_identity_mismatch",
        "raw_fact_mismatch",
        "request_mismatch",
        "literal_mismatch",
        "cleanup_failure",
        "independent_observation_pending",
        "validator_error",
    }
)


@dataclass(frozen=True)
class AdmittedActor:
    id: str
    kind: str
    runtime_ref: str
    entrypoint_ref: str
    artifact_roles: tuple[str, ...]
    source_roles: tuple[str, ...]
    installed_manifest: Mapping[str, object]
    runtime: Mapping[str, object]


@dataclass(frozen=True)
class AdmittedInvocation:
    id: str
    case_id: str
    actor_id: str
    operation: str
    session_sequence_before: int
    session_sequence_after: int
    evidence_id: str
    request_ids: tuple[str, ...]


@dataclass(frozen=True)
class AdmissionContext:
    adapter_id: str
    cell_id: str
    session_id: str
    header: Mapping[str, object]
    scenario: Mapping[str, object]
    actors: Mapping[str, AdmittedActor]
    invocations: tuple[AdmittedInvocation, ...]
    raw: Mapping[str, Mapping[str, object]]
    controller_observations: Mapping[int, Mapping[str, object]]


@dataclass(frozen=True)
class AdmissionDecision:
    status: Literal["passed", "failed", "pending"]
    code: str


def _decision(status: Literal["passed", "failed", "pending"], code: str) -> AdmissionDecision:
    if code not in DECISION_CODES:
        return AdmissionDecision("failed", "validator_error")
    return AdmissionDecision(status, code)


def _typed_equal(left: object, right: object) -> bool:
    if type(left) is not type(right):
        return False
    if isinstance(left, Mapping):
        return set(left) == set(right) and all(
            _typed_equal(left[key], right[key]) for key in left
        )
    if isinstance(left, (list, tuple)):
        return len(left) == len(right) and all(
            _typed_equal(a, b) for a, b in zip(left, right)
        )
    return left == right


def _canonical_uuid(value: object) -> bool:
    try:
        parsed = uuid.UUID(value) if isinstance(value, str) else None
    except (ValueError, AttributeError):
        return False
    return parsed is not None and parsed.version == 4 and str(parsed) == value


def _digest(value: object) -> bool:
    return isinstance(value, str) and HEX64.fullmatch(value) is not None


def _actor_value(actor: object, key: str, default=None):
    if isinstance(actor, Mapping):
        return actor.get(key, default)
    return getattr(actor, key, default)


def _invocation_value(invocation: object, key: str, default=None):
    if isinstance(invocation, Mapping):
        return invocation.get(key, default)
    return getattr(invocation, key, default)


def _header_artifact(header: Mapping[str, object], role: str):
    artifacts = header.get("artifacts")
    if not isinstance(artifacts, list):
        return None
    matches = [item for item in artifacts if isinstance(item, Mapping) and item.get("role") == role]
    return matches[0] if len(matches) == 1 else None


def _header_source(header: Mapping[str, object], role: str):
    sources = header.get("source_identities")
    if not isinstance(sources, list):
        return None
    matches = [item for item in sources if isinstance(item, Mapping) and item.get("role") == role]
    return matches[0] if len(matches) == 1 else None


def _hub_id(context: AdmissionContext):
    for sequence in sorted(context.controller_observations):
        row = context.controller_observations[sequence]
        if isinstance(row, Mapping) and isinstance(row.get("hub_id"), str):
            return row["hub_id"]
    return None


def _vehicles(context: AdmissionContext):
    value = context.scenario.get("vehicles") if isinstance(context.scenario, Mapping) else None
    if isinstance(value, list):
        return value
    return [
        {"vehicle_id": "11111111-1111-4111-8111-111111111111", "display_name": UNICODE_NAME},
        {"vehicle_id": "22222222-2222-4222-8222-222222222222", "display_name": "Interop empty"},
    ]


def _expected(case_id: str, context: AdmissionContext) -> Mapping[str, object]:
    artifacts = {
        role: item.get("sha256")
        for role in ("hub_executable", "protocol_fixture_seed")
        if isinstance((item := _header_artifact(context.header, role)), Mapping)
    }
    service_mode = None
    runtime = context.header.get("runtime") if isinstance(context.header, Mapping) else None
    if isinstance(runtime, Mapping) and isinstance(runtime.get("hub"), Mapping):
        service_mode = runtime["hub"].get("service_mode")
    common = {
        "candidate_artifact_identity": {
            "hub_sha256": artifacts.get("hub_executable"),
            "seed_sha256": artifacts.get("protocol_fixture_seed"),
            "product_version": PRODUCT_VERSION,
        },
        "installed_service_runtime": {"service_mode": service_mode},
        "discovery_identity_profile": {
            "hub_id": _hub_id(context),
            "api_versions": ["1.0"],
            "protocol": "teslatlas-sync",
            "protocol_major": 1,
            "pack_format": "sqlite-zstd",
            "version": PRODUCT_VERSION,
        },
        "unauthenticated_discovery": {
            "discovery": 200,
            "health": 200,
            "readiness": 200,
            "credential_absent": True,
        },
        "bad_invitation": {"typed_error": "hub_http_error", "http_status": 401},
        "expired_invitation": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
        "replayed_invitation": {"typed_error": "hub_http_error", "http_status": 401},
        "real_auth": {"claimed": 200, "vehicles": _vehicles(context)},
        "credential_lifecycle_reauth": {"new_device": True, "vehicles": 200},
        "revocation": {"typed_error": "hub_http_error", "http_status": 401},
        "unknown_vehicle": {"typed_error": "hub_http_error", "http_status": 404},
        "exact_current_values": {
            "battery_level": 0,
            "inside_temp": 21.5,
            "outside_temp": None,
            "observed_at_ms": 1788566400000,
            "est_battery_range_km": 160.93,
            "odometer": 16093.44,
            "speed": 16,
            "scheduled_charging_start_time": 1788570000,
            "active_route_miles_to_arrival": 12.5,
            "empty_vehicle_observed_at_ms": None,
        },
        "endpoint_restart": {"same_hub": True, "new_process": True, "vehicles": 200},
        "outage_recovery": {"outage_observed": True, "vehicles": 200},
        "unsupported_operation_zero_requests": {"outgoing_requests": 0},
        "credential_rotation_api": {
            "rotated": True,
            "same_device": True,
            "vehicles": 200,
            "old_credential_error": "hub_http_error",
            "old_credential_status": 401,
        },
        "drives_three_page_order": {"pages": [[105, 104], [103, 102], [101]]},
        "drives_terminal_cursor": {"next_cursor": None, "ids": [101]},
        "drives_etag_304": {"kind": "notModified", "post_304_ids": [103, 102]},
        "drives_wrong_vehicle_cursor": {"typed_error": "hub_http_error", "http_status": 400},
        "drives_wrong_filter_cursor": {"typed_error": "hub_http_error", "http_status": 400},
    }
    return common[case_id]


def expected_normalized_facts(case_id: str, context: AdmissionContext) -> Mapping[str, object]:
    """Return independently derived normalized facts for adapter construction."""
    return dict(_expected(case_id, context))


def _valid_context(context: AdmissionContext) -> bool:
    return (
        isinstance(context, AdmissionContext)
        and context.adapter_id == ADAPTER_ID
        and isinstance(context.cell_id, str)
        and context.cell_id.startswith(ADAPTER_ID + "__")
        and _canonical_uuid(context.session_id)
        and isinstance(context.header, Mapping)
        and isinstance(context.scenario, Mapping)
        and isinstance(context.actors, Mapping)
        and isinstance(context.invocations, tuple)
        and isinstance(context.raw, Mapping)
        and isinstance(context.controller_observations, Mapping)
    )


def _valid_actor(actor_id: str, actor: object) -> str | None:
    if actor_id != ACTOR_IDS[0]:
        return "wrong_actor"
    if (
        _actor_value(actor, "id") != actor_id
        or _actor_value(actor, "kind") != ACTOR_KIND
        or _actor_value(actor, "runtime_ref") != RUNTIME_REF
        or _actor_value(actor, "entrypoint_ref") != ENTRYPOINT_REF
        or tuple(_actor_value(actor, "artifact_roles", ())) != ()
        or tuple(_actor_value(actor, "source_roles", ())) != (SOURCE_ROLE,)
    ):
        return "wrong_actor"
    manifest = _actor_value(actor, "installed_manifest")
    if not isinstance(manifest, Mapping) or set(manifest) != {"path", "sha256"} or not _digest(manifest.get("sha256")):
        return "installed_manifest_mismatch"
    return None


def _request_shape(request: object) -> bool:
    if not isinstance(request, Mapping):
        return False
    common = (
        request.get("method") in {"GET", "POST"}
        and isinstance(request.get("route"), str)
        and request["route"].startswith("/")
        and "?" not in request["route"]
    )
    if set(request) == {"method", "route", "status", "request_id"}:
        return common and type(request.get("status")) is int and isinstance(request.get("request_id"), str) and REQUEST_ID.fullmatch(request["request_id"]) is not None
    return set(request) == {"method", "route", "failure"} and common and request.get("failure") == "transport_unavailable"


def _controller_anchor(case_id: str, invocation: AdmittedInvocation, context: AdmissionContext) -> bool:
    before = invocation.session_sequence_before
    after = invocation.session_sequence_after
    if type(before) is not int or type(after) is not int or before <= 0 or after < before:
        return False
    if case_id in {"candidate_artifact_identity", "installed_service_runtime", "discovery_identity_profile", "unauthenticated_discovery", "bad_invitation", "expired_invitation", "replayed_invitation", "real_auth", "unknown_vehicle", "exact_current_values", "unsupported_operation_zero_requests", "credential_rotation_api", "drives_three_page_order", "drives_terminal_cursor", "drives_etag_304", "drives_wrong_vehicle_cursor", "drives_wrong_filter_cursor"}:
        return before in context.controller_observations and after in context.controller_observations
    if case_id == "revocation":
        return before in context.controller_observations and after in context.controller_observations
    return before in context.controller_observations and after in context.controller_observations


def _raw_for(invocation: AdmittedInvocation, context: AdmissionContext):
    value = context.raw.get(invocation.evidence_id)
    return value if isinstance(value, Mapping) else None


def _admit(case: Mapping[str, object], context: AdmissionContext) -> AdmissionDecision:
    required = {"id", "status", "expected", "actual", "evidence_kind", "request_transcript"}
    if set(case) != required:
        return _decision("failed", "case_shape")
    case_id = case.get("id")
    if case_id not in REQUIRED_CASES:
        return _decision("failed", "unknown_case")
    if not _valid_context(context):
        return _decision("failed", "wrong_context")
    if set(context.actors) != set(ACTOR_IDS):
        return _decision("failed", "wrong_actor")
    actor = context.actors[ACTOR_IDS[0]]
    actor_problem = _valid_actor(ACTOR_IDS[0], actor)
    if actor_problem is not None:
        return _decision("failed", actor_problem)
    if _header_source(context.header, SOURCE_ROLE) is None:
        return _decision("failed", "wrong_source_role")
    expected_status = "pending" if case_id == "installed_service_runtime" else "passed"
    if case.get("status") != expected_status or case.get("evidence_kind") != CASE_KINDS[case_id]:
        return _decision("failed", "case_shape")
    expected = _expected(case_id, context)
    if not _typed_equal(case.get("expected"), case.get("actual")) or not _typed_equal(case.get("actual"), expected):
        return _decision("failed", "literal_mismatch")
    invocations = [item for item in context.invocations if _invocation_value(item, "case_id") == case_id]
    if len(invocations) != 1:
        return _decision("failed", "invocation_missing")
    invocation = invocations[0]
    if _invocation_value(invocation, "actor_id") != ACTOR_IDS[0]:
        return _decision("failed", "wrong_actor")
    operation = _invocation_value(invocation, "operation")
    if operation not in CASE_OPERATIONS[case_id]:
        return _decision("failed", "operation_mismatch")
    if not _controller_anchor(case_id, invocation, context):
        return _decision("failed", "sequence_mismatch")
    raw = _raw_for(invocation, context)
    raw_required = {
        "schema_version", "session_id", "cell_id", "session_input_sha256", "actor_id", "operation",
        "actor_manifest_sha256", "session_sequence_before", "session_sequence_after",
        "credential_device_id", "facts", "requests", "request_ids", "cleanup",
    }
    if raw is None or set(raw) != raw_required:
        return _decision("failed", "raw_missing" if raw is None else "raw_identity_mismatch")
    if (
        raw.get("schema_version") != 1
        or raw.get("session_id") != context.session_id
        or raw.get("cell_id") != context.cell_id
        or raw.get("actor_id") != ACTOR_IDS[0]
        or raw.get("operation") != operation
        or not _digest(raw.get("session_input_sha256"))
        or raw.get("actor_manifest_sha256") != _actor_value(actor, "installed_manifest").get("sha256")
    ):
        return _decision("failed", "raw_identity_mismatch")
    if (
        raw.get("session_sequence_before") != _invocation_value(invocation, "session_sequence_before")
        or raw.get("session_sequence_after") != _invocation_value(invocation, "session_sequence_after")
        or type(raw.get("session_sequence_before")) is not int
        or type(raw.get("session_sequence_after")) is not int
        or raw["session_sequence_before"] <= 0
        or raw["session_sequence_after"] < raw["session_sequence_before"]
    ):
        return _decision("failed", "sequence_mismatch")
    if not _typed_equal(raw.get("facts"), expected):
        return _decision("failed", "raw_fact_mismatch")
    credential = raw.get("credential_device_id")
    if credential is not None and not _canonical_uuid(credential):
        return _decision("failed", "raw_identity_mismatch")
    if raw.get("cleanup") != {
        "status": "passed",
        "transport_resources_closed": True,
        "auxiliary_fixture_stopped": True,
        "process_exited": True,
    }:
        return _decision("failed", "cleanup_failure")
    requests = raw.get("requests")
    if not isinstance(requests, list) or any(not _request_shape(item) for item in requests):
        return _decision("failed", "request_mismatch")
    request_ids = tuple(item["request_id"] for item in requests if "request_id" in item)
    raw_request_ids = raw.get("request_ids")
    if (
        not isinstance(raw_request_ids, list)
        or any(not isinstance(request_id, str) for request_id in raw_request_ids)
        or tuple(raw_request_ids) != request_ids
        or len(request_ids) != len(set(request_ids))
        or request_ids != tuple(_invocation_value(invocation, "request_ids", ()))
    ):
        return _decision("failed", "request_mismatch")
    if not _typed_equal(case.get("request_transcript"), requests):
        return _decision("failed", "request_mismatch")
    if CASE_KINDS[case_id] == "zero_request" and requests:
        return _decision("failed", "request_mismatch")
    if case_id == "installed_service_runtime":
        return _decision("pending", "runner_owned_service_runtime")
    return _decision("passed", "accepted")


def admit_case(case: dict, context: AdmissionContext) -> AdmissionDecision:
    """Admit one case, converting every validator exception to a fixed failure."""
    try:
        if not isinstance(case, dict):
            return _decision("failed", "case_shape")
        return _admit(case, context)
    except Exception:
        return _decision("failed", "validator_error")


def validate_manifest(path: str | Path) -> Mapping[str, object]:
    """Validate the inert manifest and its source hashes for review tooling."""
    manifest_path = Path(path).resolve()
    raw = manifest_path.read_bytes()
    value = json.loads(raw.decode("utf-8", "strict"))
    required = {"schema_version", "adapter_id", "revision", "required_cases", "actors", "cases", "raw_schemas", "phases", "validator"}
    if not isinstance(value, dict) or set(value) != required:
        raise ValueError("Protocol matrix contract shape is invalid")
    if value["schema_version"] != 1 or value["adapter_id"] != ADAPTER_ID or value["revision"] != CONTRACT_REVISION:
        raise ValueError("Protocol matrix contract identity is invalid")
    if value["required_cases"] != list(REQUIRED_CASES) or [item.get("id") for item in value["cases"]] != list(REQUIRED_CASES):
        raise ValueError("Protocol matrix cases are incomplete or reordered")
    if value["actors"] != [{"id": ACTOR_IDS[0], "kind": ACTOR_KIND, "required": True}]:
        raise ValueError("Protocol matrix actor contract is invalid")
    required_case_keys = {"id", "actor_ids", "evidence_kind", "operations", "raw_schema_ids"}
    for item in value["cases"]:
        if not isinstance(item, dict) or set(item) != required_case_keys:
            raise ValueError("Protocol matrix case contract shape is invalid")
        if item["actor_ids"] != list(ACTOR_IDS) or item["operations"] != list(CASE_OPERATIONS[item["id"]]) or item["evidence_kind"] != CASE_KINDS[item["id"]] or item["raw_schema_ids"] != ["protocol-http-v1"]:
            raise ValueError("Protocol matrix case contract is inconsistent")
    if value["phases"] != []:
        raise ValueError("Protocol matrix phases must be empty")
    bindings = [item.get("schema") for item in value["raw_schemas"]] + [value["validator"]]
    for binding in bindings:
        if not isinstance(binding, dict) or set(binding) != {"path", "sha256"} or not isinstance(binding["path"], str) or "\x00" in binding["path"] or not _digest(binding["sha256"]):
            raise ValueError("Protocol matrix source binding is invalid")
        member = Path(binding["path"])
        if member.is_absolute():
            resolved = member
        elif ".." not in member.parts:
            resolved = (manifest_path.parent / member).resolve()
            try:
                resolved.relative_to(manifest_path.parent)
            except ValueError:
                raise ValueError("Protocol matrix source binding is invalid") from None
        else:
            raise ValueError("Protocol matrix source binding is invalid")
        if hashlib.sha256(resolved.read_bytes()).hexdigest() != binding["sha256"]:
            raise ValueError("Protocol matrix source binding changed")
    return MappingProxyType(value)
