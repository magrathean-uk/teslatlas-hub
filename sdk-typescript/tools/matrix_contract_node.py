from __future__ import annotations

"""Pure semantic admission for one packed TypeScript SDK matrix adapter.

This module intentionally has no repository or network dependencies.  Hub loads
its exact bytes from the hash-bound contract manifest and supplies immutable
runner observations, actor claims, invocations, and raw evidence.
"""

import hashlib
import json
import re
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from typing import Literal

ADAPTER_ID = "typescript_node"
CONTRACT_REVISION = 1
PRODUCT_VERSION = "2026.36.2"
PROFILE_SHA256 = "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"
ACTOR_ID = "sdk_node"
ACTOR_KIND = "packed_sdk_node"
RUNTIME_REF = "root_node"
ENTRYPOINT_REF = "sdk_node_worker"
SOURCE_ROLE = "typescript_sdk_source"
ARTIFACT_ROLE = "typescript_sdk_tarball"
UNKNOWN_VEHICLE = "33333333-3333-4333-8333-333333333333"
HEX64 = re.compile(r"^[0-9a-f]{64}$")
TOKEN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$")

REQUIRED_CASES = 'candidate_artifact_identity', 'installed_service_runtime', 'discovery_identity_profile', 'unauthenticated_discovery', 'bad_invitation', 'expired_invitation', 'replayed_invitation', 'real_auth', 'credential_lifecycle_reauth', 'revocation', 'unknown_vehicle', 'exact_current_values', 'endpoint_restart', 'outage_recovery', 'unsupported_operation_zero_requests', 'credential_rotation_api', 'drives_three_page_order', 'drives_terminal_cursor', 'drives_etag_304', 'drives_wrong_vehicle_cursor', 'drives_wrong_filter_cursor', 
CASE_OPERATIONS = MappingProxyType({'candidate_artifact_identity': ('observe_identity',), 'installed_service_runtime': ('observe_identity',), 'discovery_identity_profile': ('discovery',), 'unauthenticated_discovery': ('unauthenticated_probes',), 'bad_invitation': ('bad_invitation',), 'expired_invitation': ('expired_invitation',), 'replayed_invitation': ('replayed_invitation',), 'real_auth': ('real_auth',), 'credential_lifecycle_reauth': ('reauthentication',), 'revocation': ('revoked_credential',), 'unknown_vehicle': ('unknown_vehicle',), 'exact_current_values': ('exact_current',), 'endpoint_restart': ('endpoint_restart',), 'outage_recovery': ('outage_recovery',), 'unsupported_operation_zero_requests': ('unsupported_operations',), 'credential_rotation_api': ('credential_rotation',), 'drives_three_page_order': ('drives_three_pages',), 'drives_terminal_cursor': ('drives_terminal_cursor',), 'drives_etag_304': ('drives_etag',), 'drives_wrong_vehicle_cursor': ('wrong_vehicle_cursor',), 'drives_wrong_filter_cursor': ('wrong_filter_cursor',)})
CASE_KINDS = MappingProxyType({
    case_id: (
        "identity" if case_id in {"candidate_artifact_identity", "installed_service_runtime"}
        else "zero_request" if case_id in {
            "expired_invitation", "unsupported_operation_zero_requests",
            "drives_wrong_vehicle_cursor", "drives_wrong_filter_cursor",
        }
        else "http"
    )
    for case_id in REQUIRED_CASES
})

DECISION_CODES = frozenset({
    "accepted", "runner_owned_service_runtime", "case_shape", "unknown_case",
    "wrong_context", "wrong_actor", "wrong_runtime", "wrong_source_role",
    "wrong_artifact_role", "installed_manifest_mismatch", "invocation_missing",
    "operation_mismatch", "sequence_mismatch", "controller_mismatch",
    "raw_missing", "raw_identity_mismatch", "raw_fact_mismatch",
    "request_mismatch", "literal_mismatch", "cleanup_failure",
})


@dataclass(frozen=True)
class AdmittedActor:
    id: str
    kind: str
    runtime_ref: str
    entrypoint_ref: str
    artifact_roles: tuple
    source_roles: tuple
    installed_manifest: Mapping
    runtime: Mapping


@dataclass(frozen=True)
class AdmittedInvocation:
    id: str
    case_id: str
    actor_id: str
    operation: str
    session_sequence_before: int
    session_sequence_after: int
    evidence_id: str
    request_ids: tuple


@dataclass(frozen=True)
class AdmissionContext:
    adapter_id: str
    cell_id: str
    session_id: str
    header: Mapping
    scenario: Mapping
    actors: Mapping[str, AdmittedActor]
    invocations: tuple
    raw: Mapping[str, Mapping]
    controller_observations: Mapping[int, Mapping]


@dataclass(frozen=True)
class AdmissionDecision:
    status: Literal["passed", "failed", "pending"]
    code: str


def _typed_equal(left, right):
    if type(left) is not type(right):
        return False
    if isinstance(left, Mapping):
        return set(left) == set(right) and all(_typed_equal(left[key], right[key]) for key in left)
    if isinstance(left, (list, tuple)):
        return len(left) == len(right) and all(_typed_equal(a, b) for a, b in zip(left, right))
    return left == right


def _canonical_uuid(value):
    try:
        parsed = uuid.UUID(value) if isinstance(value, str) else None
    except (ValueError, AttributeError):
        return False
    return parsed is not None and str(parsed) == value


def _artifact_digest(header):
    artifacts = header.get("artifacts") if isinstance(header, Mapping) else None
    if not isinstance(artifacts, list):
        return None
    rows = [row for row in artifacts if isinstance(row, Mapping) and row.get("role") == ARTIFACT_ROLE]
    return rows[0].get("sha256") if len(rows) == 1 else None


def _hub_digest(header):
    artifacts = header.get("artifacts") if isinstance(header, Mapping) else None
    if not isinstance(artifacts, list):
        return None
    rows = [row for row in artifacts if isinstance(row, Mapping) and row.get("role") == "hub_executable"]
    return rows[0].get("sha256") if len(rows) == 1 else None


def _runtime_members(runtime):
    artifact = runtime.get("artifact") if isinstance(runtime, Mapping) else None
    members = artifact.get("installed_members") if isinstance(artifact, Mapping) else None
    return len(members) if isinstance(members, list) else members if type(members) is int else None


def _pairing_values(context, case_id):
    sequence = next((i.session_sequence_after if case_id == "credential_lifecycle_reauth" else i.session_sequence_before for i in context.invocations if i.case_id == case_id), None)
    observation = context.controller_observations.get(sequence) if sequence is not None else None
    invitations = observation.get("invitations") if isinstance(observation, Mapping) else None
    active = invitations.get("active") if isinstance(invitations, Mapping) else None
    expired = invitations.get("expired") if isinstance(invitations, Mapping) else None
    return (
        active.get("pairing_id") if isinstance(active, Mapping) else None,
        expired.get("pairing_id") if isinstance(expired, Mapping) else None,
        expired.get("expires_at_ms") if isinstance(expired, Mapping) else None,
    )


def _scenario_vehicles(context):
    vehicles = context.scenario.get("vehicles") if isinstance(context.scenario, Mapping) else None
    if not isinstance(vehicles, list):
        return [
            {"vehicle_id": "11111111-1111-4111-8111-111111111111", "display_name": "Interop – Árvíztűrő 🚗"},
            {"vehicle_id": "22222222-2222-4222-8222-222222222222", "display_name": "Interop empty"},
        ]
    result = []
    for vehicle in vehicles:
        if isinstance(vehicle, Mapping) and isinstance(vehicle.get("vehicle_id"), str) and isinstance(vehicle.get("display_name"), str):
            result.append({"vehicle_id": vehicle["vehicle_id"], "display_name": vehicle["display_name"]})
    return result


def _scenario_pages(context):
    pages = context.scenario.get("drive_pages_at_limit_2") if isinstance(context.scenario, Mapping) else None
    if isinstance(pages, list) and all(isinstance(page, list) for page in pages):
        return pages
    return [[105, 104], [103, 102], [101]]


def _scenario_current(context):
    current = context.scenario.get("current") if isinstance(context.scenario, Mapping) else None
    if not isinstance(current, Mapping):
        current = {
            "battery_level": 0, "inside_temp": 21.5, "outside_temp": None,
            "observed_at_ms": 1788566400000, "est_battery_range_km": 160.93,
            "odometer": 16093.44, "speed": 16,
            "scheduled_charging_start_time": 1788570000,
            "active_route_miles_to_arrival": 12.5,
        }
    return dict(current, empty_vehicle_observed_at_ms=None)


def _semantic(case_id, context):
    active_pairing, expired_pairing, expired_at = _pairing_values(context, case_id)
    hub_id = None
    for observation in context.controller_observations.values() if isinstance(context.controller_observations, Mapping) else ():
        if isinstance(observation, Mapping) and isinstance(observation.get("hub_id"), str):
            hub_id = observation["hub_id"]
            break
    service_mode = None
    runtime = context.header.get("runtime") if isinstance(context.header, Mapping) else None
    if isinstance(runtime, Mapping) and isinstance(runtime.get("hub"), Mapping):
        service_mode = runtime["hub"].get("service_mode")
    base = {
        "candidate_artifact_identity": {
            "hub_sha256": _hub_digest(context.header), "tarball_sha256": _artifact_digest(context.header),
            "package_version": PRODUCT_VERSION, "installed_members": _runtime_members(context.actors[ACTOR_ID].runtime),
        },
        "installed_service_runtime": {"service_mode": service_mode},
        "discovery_identity_profile": {
            "hub_id": hub_id, "api_versions": ["1.0"], "protocol": "teslatlas-sync",
            "protocol_major": 1, "pack_format": "sqlite-zstd", "version": PRODUCT_VERSION,
        },
        "unauthenticated_discovery": {"discovery": 200, "health": 200, "readiness": 200, "credential_absent": True},
        "bad_invitation": {"typed_error": "hub_http_error", "http_status": 401},
        "expired_invitation": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
        "replayed_invitation": {"typed_error": "hub_http_error", "http_status": 401},
        "real_auth": {"claimed": 200, "vehicles": _scenario_vehicles(context)},
        "credential_lifecycle_reauth": {"new_device": True, "vehicles": 200},
        "revocation": {"typed_error": "hub_http_error", "http_status": 401},
        "unknown_vehicle": {"typed_error": "hub_http_error", "http_status": 404},
        "exact_current_values": _scenario_current(context),
        "endpoint_restart": {"same_hub": True, "new_process": True, "vehicles": 200},
        "outage_recovery": {"outage_observed": True, "vehicles": 200},
        "unsupported_operation_zero_requests": {"outgoing_requests": 0},
        "credential_rotation_api": {"rotated": True, "same_device": True, "vehicles": 200, "old_credential_error": "hub_http_error", "old_credential_status": 401},
        "drives_three_page_order": {"pages": _scenario_pages(context)},
        "drives_terminal_cursor": {"next_cursor": None, "ids": _scenario_pages(context)[-1]},
        "drives_etag_304": {"kind": "notModified", "post_304_ids": _scenario_pages(context)[1]},
        "drives_wrong_vehicle_cursor": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
        "drives_wrong_filter_cursor": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
        "real_browser_cors": {"preflight_succeeded": True, "cross_origin": True},
        "browser_normal_tls_validation": {"trusted_succeeded": True, "untrusted_error": "ERR_CERT_AUTHORITY_INVALID"},
    }
    value = base[case_id]
    if case_id in {"bad_invitation", "replayed_invitation"}:
        # Pairing identifiers are intentionally omitted from the public Node/browser
        # normalized facts; they remain private broker evidence.
        return value
    if case_id == "expired_invitation":
        return value
    return value


def _decision(code, pending=False):
    return AdmissionDecision("pending" if pending else "failed", code)


def _valid_binding(value):
    return isinstance(value, Mapping) and set(value) == {"path", "sha256"} and isinstance(value.get("path"), str) and value["path"].startswith("/") and HEX64.fullmatch(str(value.get("sha256"))) is not None


def _valid_member_list(value):
    if not isinstance(value, list) or not value:
        return False
    paths = []
    for member in value:
        if not isinstance(member, Mapping) or set(member) != {"path", "bytes", "mode", "sha256"} or not isinstance(member.get("path"), str) or member["path"].startswith("/") or ".." in Path(member["path"]).parts or type(member.get("bytes")) is not int or member["bytes"] < 0 or type(member.get("mode")) is not int or not 0 <= member["mode"] <= 0o7777 or HEX64.fullmatch(str(member.get("sha256"))) is None:
            return False
        paths.append(member["path"])
    return paths == sorted(set(paths))


def _valid_runtime(runtime):
    required = {"schema_version", "runtime_ref", "runtime_kind", "platform", "tool_versions", "transport", "artifact", "identity_sha256"}
    if not isinstance(runtime, Mapping) or set(runtime) != required or runtime.get("schema_version") != 1 or runtime.get("runtime_ref") != RUNTIME_REF or not HEX64.fullmatch(str(runtime.get("identity_sha256"))):
        return False
    platform = runtime.get("platform")
    if not isinstance(platform, Mapping) or set(platform) != {"os", "architecture"} or platform.get("os") not in {"macOS", "Debian 13", "Linux"} or platform.get("architecture") not in {"arm64", "amd64"}:
        return False
    versions = runtime.get("tool_versions")
    if not isinstance(versions, Mapping) or set(versions) - {"node", "chromium"} or not all(isinstance(item, str) and item for item in versions.values()):
        return False
    transport = runtime.get("transport")
    expected_transport = "node-undici-diagnostics-channel" if ADAPTER_ID.endswith("node") else "Chromium CDP Network"
    if not isinstance(transport, Mapping) or set(transport) != {"kind"} or transport.get("kind") != expected_transport:
        return False
    artifact = runtime.get("artifact")
    return isinstance(artifact, Mapping) and set(artifact) == {"role", "sha256", "installed_root", "installed_manifest_sha256", "installed_members"} and artifact.get("role") == ARTIFACT_ROLE and HEX64.fullmatch(str(artifact.get("sha256"))) is not None and isinstance(artifact.get("installed_root"), str) and artifact["installed_root"].startswith("/") and HEX64.fullmatch(str(artifact.get("installed_manifest_sha256"))) is not None and _valid_member_list(artifact.get("installed_members"))


def _valid_actor(actor):
    if not isinstance(actor, AdmittedActor) or actor.id != ACTOR_ID or actor.kind != ACTOR_KIND or actor.runtime_ref != RUNTIME_REF or actor.entrypoint_ref != ENTRYPOINT_REF:
        return "wrong_actor"
    if actor.source_roles != (SOURCE_ROLE,):
        return "wrong_source_role"
    if actor.artifact_roles != (ARTIFACT_ROLE,):
        return "wrong_artifact_role"
    if not _valid_runtime(actor.runtime):
        return "wrong_runtime"
    manifest = actor.installed_manifest
    if not _valid_binding(manifest):
        return "installed_manifest_mismatch"
    return None


def _request_requirement(case_id, requests):
    if CASE_KINDS[case_id] in {"identity", "zero_request"}:
        return len(requests) == 0
    discovery = ("GET", "/.well-known/teslatlas-hub", 200)
    vehicles = ("GET", "/v1/vehicles", 200)
    current = ("GET", "/v1/vehicles/{vehicle_id}/current", 200)
    drives = ("GET", "/v1/vehicles/{vehicle_id}/drives", 200)
    requirements = {
        "discovery_identity_profile": (discovery,),
        "unauthenticated_discovery": (discovery, ("GET", "/healthz", 200), ("GET", "/readyz", 200)),
        "bad_invitation": (("POST", "/v1/pairings/{pairing_id}/claim", 401),),
        "replayed_invitation": (("POST", "/v1/pairings/{pairing_id}/claim", 401),),
        "real_auth": (("POST", "/v1/pairings/{pairing_id}/claim", 200), vehicles),
        "credential_lifecycle_reauth": (("POST", "/v1/pairings/{pairing_id}/claim", 200), vehicles),
        "revocation": (("GET", "/v1/vehicles", 401),),
        "unknown_vehicle": (("GET", "/v1/vehicles/{vehicle_id}/current", 404),),
        "exact_current_values": (current, current),
        "endpoint_restart": (vehicles,),
        "outage_recovery": (("GET", "/v1/vehicles", 0), vehicles),
        "credential_rotation_api": (discovery, ("POST", "/v1/device/rotate", 200), discovery, ("GET", "/v1/vehicles", 401), vehicles),
        "drives_three_page_order": (drives, drives, drives),
        "drives_terminal_cursor": (drives,),
        "drives_etag_304": (("GET", "/v1/vehicles/{vehicle_id}/drives", 304), drives),
        "real_browser_cors": (("OPTIONS", "/.well-known/teslatlas-hub", 204),),
        "browser_normal_tls_validation": (discovery,),
    }.get(case_id)
    if case_id == "real_browser_cors":
        return bool(requests) and all(
            item.get("method") == "OPTIONS" and type(item.get("status")) is int
            and 200 <= item["status"] < 300
            for item in requests
        )
    if requirements is None or len(requests) != len(requirements):
        return False
    actual = tuple((item.get("method"), item.get("route"), item.get("status")) for item in requests)
    if actual != requirements:
        return False
    for item in requests:
        if not isinstance(item.get("request_id"), str) or not item["request_id"] or not isinstance(item.get("scope"), str) or not item["scope"].startswith("/"):
            return False
        if item["route"] == "/v1/pairings/{pairing_id}/claim" and re.fullmatch(r"/v1/pairings/[0-9a-f-]{36}/claim", item["scope"]) is None:
            return False
        if item["route"].startswith("/v1/vehicles/{vehicle_id}"):
            suffix = item["route"].removeprefix("/v1/vehicles/{vehicle_id}")
            if re.fullmatch(r"/v1/vehicles/[0-9a-f-]{36}" + re.escape(suffix), item["scope"]) is None:
                return False
    if case_id == "unknown_vehicle" and requests[0]["scope"] != f"/v1/vehicles/{UNKNOWN_VEHICLE}/current":
        return False
    return True


def _controller_semantics(case_id, invocation, context):
    observations = context.controller_observations
    if not isinstance(observations, Mapping) or not observations:
        return False
    before = observations.get(invocation.session_sequence_before)
    after = observations.get(invocation.session_sequence_after)
    if not isinstance(before, Mapping) or not isinstance(after, Mapping):
        return False
    if case_id in {"credential_lifecycle_reauth", "real_auth", "bad_invitation", "replayed_invitation"}:
        if case_id == "credential_lifecycle_reauth" and not any(item.get("operation") == "pair" for item in observations.values() if isinstance(item, Mapping)):
            return False
    if case_id == "revocation" and not any(item.get("operation") == "revoke" for item in observations.values() if isinstance(item, Mapping)):
        return False
    if case_id == "endpoint_restart" and not any(item.get("operation") == "start" and item.get("state") == "running" for item in observations.values() if isinstance(item, Mapping)):
        return False
    if case_id == "outage_recovery":
        has_stop = any(item.get("operation") == "stop" for item in observations.values() if isinstance(item, Mapping))
        has_running_start = any(item.get("operation") == "start" and item.get("state") == "running" for item in observations.values() if isinstance(item, Mapping))
        if not (has_stop and has_running_start):
            return False
    return True


def admit_case(case: dict, context: AdmissionContext) -> AdmissionDecision:
    required = {"id", "status", "expected", "actual", "evidence_kind", "request_transcript"}
    allowed = required | {"process_evidence"}
    if not isinstance(case, Mapping) or not set(case).issubset(allowed) or not required.issubset(case):
        return _decision("case_shape")
    case_id = case["id"]
    if case_id not in REQUIRED_CASES:
        return _decision("unknown_case")
    if not isinstance(context, AdmissionContext) or context.adapter_id != ADAPTER_ID or not isinstance(context.cell_id, str) or not context.cell_id.startswith(ADAPTER_ID + "__") or not _canonical_uuid(context.session_id):
        return _decision("wrong_context")
    if case["status"] != ("pending" if case_id == "installed_service_runtime" else "passed"):
        return _decision("case_shape")
    if set(context.actors) != {ACTOR_ID}:
        return _decision("wrong_actor")
    actor = context.actors[ACTOR_ID]
    problem = _valid_actor(actor)
    if problem:
        return _decision(problem)
    if actor.runtime.get("artifact", {}).get("sha256") != _artifact_digest(context.header) or actor.runtime.get("artifact", {}).get("installed_manifest_sha256") != actor.installed_manifest.get("sha256"):
        return _decision("installed_manifest_mismatch")
    if case["evidence_kind"] != CASE_KINDS[case_id] or not isinstance(case["expected"], Mapping) or not case["expected"] or not _typed_equal(case["expected"], case["actual"]):
        return _decision("literal_mismatch" if _typed_equal(case.get("expected"), case.get("actual")) is False else "case_shape")
    if not _typed_equal(case["expected"], _semantic(case_id, context)):
        return _decision("literal_mismatch")
    invocations = [item for item in context.invocations if isinstance(item, AdmittedInvocation) and item.case_id == case_id]
    if len(invocations) != 1 or invocations[0].actor_id != ACTOR_ID:
        return _decision("invocation_missing" if not invocations else "wrong_actor")
    invocation = invocations[0]
    if invocation.operation not in CASE_OPERATIONS[case_id]:
        return _decision("operation_mismatch")
    if type(invocation.session_sequence_before) is not int or type(invocation.session_sequence_after) is not int or invocation.session_sequence_before <= 0 or invocation.session_sequence_after < invocation.session_sequence_before:
        return _decision("sequence_mismatch")
    raw = context.raw.get(invocation.evidence_id)
    if not isinstance(raw, Mapping):
        return _decision("raw_missing")
    raw_required = {"schema_version", "session_id", "cell_id", "session_input_sha256", "actor_id", "operation", "actor_manifest_sha256", "session_sequence_before", "session_sequence_after", "credential_device_id", "facts", "requests", "cleanup"}
    if set(raw) != raw_required or raw.get("schema_version") != 1 or raw.get("session_id") != context.session_id or raw.get("cell_id") != context.cell_id or raw.get("actor_id") != ACTOR_ID or raw.get("operation") != invocation.operation or not HEX64.fullmatch(str(raw.get("session_input_sha256"))) or raw.get("actor_manifest_sha256") != actor.installed_manifest.get("sha256"):
        return _decision("raw_identity_mismatch")
    if raw.get("session_sequence_before") != invocation.session_sequence_before or raw.get("session_sequence_after") != invocation.session_sequence_after:
        return _decision("sequence_mismatch")
    if not _typed_equal(raw.get("facts"), case["expected"]):
        return _decision("raw_fact_mismatch")
    cleanup = raw.get("cleanup")
    if cleanup != {"status": "passed", "transport_resources_closed": True, "auxiliary_fixture_stopped": True, "process_exited": True}:
        return _decision("cleanup_failure")
    requests = raw.get("requests")
    if not isinstance(requests, list) or not _request_requirement(case_id, requests):
        return _decision("request_mismatch")
    if tuple(item.get("request_id") for item in requests) != invocation.request_ids or not _typed_equal(case["request_transcript"], requests):
        return _decision("request_mismatch")
    if not _controller_semantics(case_id, invocation, context):
        return _decision("controller_mismatch")
    if case_id == "installed_service_runtime":
        return _decision("runner_owned_service_runtime", pending=True)
    return AdmissionDecision("passed", "accepted")


def validate_manifest(path):
    """Validate this inert manifest and every hash-bound member it names."""
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON member")
            result[key] = value
        return result
    raw = Path(path).read_bytes()
    value = json.loads(raw.decode("utf-8", "strict"), object_pairs_hook=unique)
    required = {"schema_version", "adapter_id", "revision", "required_cases", "actors", "cases", "raw_schemas", "phases", "validator"}
    if not isinstance(value, dict) or set(value) != required or value["schema_version"] != 1 or value["adapter_id"] != ADAPTER_ID or value["revision"] != CONTRACT_REVISION:
        raise ValueError("TypeScript matrix contract shape or identity is invalid")
    if value["required_cases"] != list(REQUIRED_CASES) or [item.get("id") for item in value["cases"]] != list(REQUIRED_CASES):
        raise ValueError("TypeScript matrix cases are incomplete or reordered")
    if len(value["actors"]) != 1 or value["actors"][0] != {"id": ACTOR_ID, "kind": ACTOR_KIND, "required": True}:
        raise ValueError("TypeScript matrix actor declaration is invalid")
    required_case_keys = {"id", "actor_ids", "evidence_kind", "operations", "raw_schema_ids"}
    if any(not isinstance(item, dict) or set(item) != required_case_keys or item.get("actor_ids") != [ACTOR_ID] or item.get("raw_schema_ids") != ["typescript-node-raw-v1"] or item.get("evidence_kind") != CASE_KINDS.get(item.get("id")) for item in value["cases"]):
        raise ValueError("TypeScript matrix case contract shape is invalid")
    if value["phases"] != []:
        raise ValueError("TypeScript adapter has no worker phases")
    bindings = [item.get("schema") for item in value["raw_schemas"]] + [value["validator"]]
    if len(value["raw_schemas"]) != 1 or value["raw_schemas"][0].get("id") != "typescript-node-raw-v1":
        raise ValueError("TypeScript raw schema declaration is invalid")
    for binding in bindings:
        if not isinstance(binding, dict) or set(binding) != {"path", "sha256"} or not isinstance(binding["path"], str) or not binding["path"].startswith("/") or HEX64.fullmatch(str(binding["sha256"])) is None:
            raise ValueError("TypeScript matrix source binding is invalid")
        member = Path(binding["path"])
        if hashlib.sha256(member.read_bytes()).hexdigest() != binding["sha256"]:
            raise ValueError("TypeScript matrix source binding changed: " + str(member))
    return MappingProxyType(value)
