"""Pure admission predicates for the installed Viewer matrix lane.

The shared Hub runner owns process, broker, file and controller admission.  This
module only checks the Viewer-specific actor/case contract after those common
checks have produced immutable views.  It deliberately performs no I/O and
cannot turn a missing browser receipt into a pass.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
import re
import uuid
from typing import Literal


ADAPTER_ID = "viewer"
CONTRACT_REVISION = 1
PRODUCT_VERSION = "2026.36.2"
PROFILE_ID = "hub-http-v1"
PROFILE_REVISION = "1.0.0"
SCENARIO_VEHICLES = [
    {
        "vehicle_id": "11111111-1111-4111-8111-111111111111",
        "display_name": "Interop – Árvíztűrő 🚗",
    },
    {
        "vehicle_id": "22222222-2222-4222-8222-222222222222",
        "display_name": "Interop empty",
    },
]
SCENARIO_CURRENT = {
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
}
SCENARIO_DRIVE_PAGES = [[105, 104], [103, 102], [101]]
HEX64 = re.compile(r"^[0-9a-f]{64}$")
CELL_IDS = frozenset(
    {
        "viewer__macos_arm64",
        "viewer__debian13_amd64",
        "viewer__debian13_arm64",
    }
)
ACTOR_IDS = ("viewer_ui", "viewer_sdk_contract")
ACTOR_KINDS = {
    "viewer_ui": "built_viewer_ui",
    "viewer_sdk_contract": "packed_sdk_dependency",
}
ACTOR_ROLES = {
    "viewer_ui": ("viewer_package_tarball", "viewer_source"),
    "viewer_sdk_contract": ("typescript_sdk_tarball", "typescript_sdk_source"),
}

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
    "built_viewer_packed_sdk",
    "real_browser_cors",
    "browser_normal_tls_validation",
)

CASE_BINDINGS = {
    "candidate_artifact_identity": ("viewer_ui", ("observe_installed_runtime",)),
    "installed_service_runtime": ("viewer_ui", ("observe_installed_runtime",)),
    "discovery_identity_profile": ("viewer_ui", ("discover",)),
    "unauthenticated_discovery": ("viewer_ui", ("discover",)),
    "bad_invitation": ("viewer_sdk_contract", ("bad_invitation",)),
    "expired_invitation": ("viewer_sdk_contract", ("expired_invitation",)),
    "replayed_invitation": ("viewer_sdk_contract", ("replayed_invitation",)),
    "real_auth": ("viewer_ui", ("pair_and_render",)),
    "credential_lifecycle_reauth": ("viewer_ui", ("revoke_and_repair",)),
    "revocation": ("viewer_ui", ("revoke_and_recover",)),
    "unknown_vehicle": ("viewer_sdk_contract", ("unknown_vehicle",)),
    "exact_current_values": ("viewer_ui", ("render_current",)),
    "endpoint_restart": ("viewer_ui", ("restart_refresh",)),
    "outage_recovery": ("viewer_ui", ("outage_recovery",)),
    "unsupported_operation_zero_requests": ("viewer_ui", ("unsupported_and_logout",)),
    "credential_rotation_api": ("viewer_sdk_contract", ("credential_rotation",)),
    "drives_three_page_order": ("viewer_ui", ("drive_pages",)),
    "drives_terminal_cursor": ("viewer_ui", ("drive_terminal",)),
    "drives_etag_304": ("viewer_ui", ("drive_revalidation",)),
    "drives_wrong_vehicle_cursor": ("viewer_sdk_contract", ("wrong_vehicle_cursor",)),
    "drives_wrong_filter_cursor": ("viewer_sdk_contract", ("wrong_filter_cursor",)),
    "built_viewer_packed_sdk": ("viewer_ui", ("full_ui_journey",)),
    "real_browser_cors": ("viewer_ui", ("browser_cors",)),
    "browser_normal_tls_validation": ("viewer_ui", ("browser_trust",)),
}

CASE_KINDS = {
    "candidate_artifact_identity": "identity",
    "installed_service_runtime": "identity",
    "built_viewer_packed_sdk": "identity",
    "expired_invitation": "zero_request",
    "unsupported_operation_zero_requests": "zero_request",
    "drives_wrong_vehicle_cursor": "zero_request",
    "drives_wrong_filter_cursor": "zero_request",
}

REQUEST_RULES = {
    "discovery_identity_profile": (("GET", re.compile(r"^/\.well-known/"), {200}),),
    "unauthenticated_discovery": (
        ("GET", re.compile(r"^/\.well-known/"), {200}),
        ("GET", re.compile(r"^/healthz$"), {200}),
        ("GET", re.compile(r"^/readyz$"), {200}),
    ),
    "bad_invitation": (("POST", re.compile(r"^/v1/pairings/[^/]+/claim$"), {401}),),
    "replayed_invitation": (("POST", re.compile(r"^/v1/pairings/[^/]+/claim$"), {401}),),
    "real_auth": (
        ("POST", re.compile(r"^/v1/pairings/[^/]+/claim$"), {200}),
        ("GET", re.compile(r"^/v1/vehicles$"), {200}),
    ),
    "credential_lifecycle_reauth": (
        ("POST", re.compile(r"^/v1/pairings/[^/]+/claim$"), {200}),
        ("GET", re.compile(r"^/v1/vehicles$"), {200}),
    ),
    "revocation": (("GET", re.compile(r"^/v1/vehicles$"), {401}),),
    "unknown_vehicle": (("GET", re.compile(r"^/v1/vehicles/[^/]+/current$"), {404}),),
    "exact_current_values": (("GET", re.compile(r"^/v1/vehicles/[^/]+/current$"), {200}),),
    "endpoint_restart": (("GET", re.compile(r"^/v1/vehicles$"), {200}),),
    "outage_recovery": (("GET", re.compile(r"^/v1/vehicles$"), {200}),),
    "credential_rotation_api": (
        ("POST", re.compile(r"^/v1/device/rotate$"), {200}),
        ("GET", re.compile(r"^/v1/vehicles$"), {200}),
        ("GET", re.compile(r"^/v1/vehicles$"), {401}),
    ),
    "drives_three_page_order": (("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$"), {200}),),
    "drives_terminal_cursor": (("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$"), {200}),),
    "drives_etag_304": (
        ("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$"), {304}),
        ("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$"), {200}),
    ),
    "real_browser_cors": (("OPTIONS", re.compile(r"^/v1/"), set(range(200, 300))),),
    "browser_normal_tls_validation": (("GET", re.compile(r"^/"), {200}),),
}

DECISION_CODES = frozenset(
    {
        "accepted",
        "evidence_pending",
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
        "raw_missing",
        "raw_identity_mismatch",
        "raw_fact_mismatch",
        "request_mismatch",
        "literal_mismatch",
        "cleanup_failure",
    }
)


@dataclass(frozen=True, slots=True)
class AdmittedActor:
    id: str
    kind: str
    runtime_ref: str
    entrypoint_ref: str
    artifact_roles: tuple[str, ...]
    source_roles: tuple[str, ...]
    installed_manifest: Mapping[str, object]
    runtime: Mapping[str, object]


@dataclass(frozen=True, slots=True)
class AdmittedInvocation:
    id: str
    case_id: str
    actor_id: str
    operation: str
    session_sequence_before: int
    session_sequence_after: int
    evidence_id: str
    request_ids: tuple[str, ...]


@dataclass(frozen=True, slots=True)
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


@dataclass(frozen=True, slots=True)
class AdmissionDecision:
    status: Literal["passed", "failed", "pending"]
    code: str


def _decision(status: Literal["passed", "failed", "pending"], code: str) -> AdmissionDecision:
    if code not in DECISION_CODES:
        raise RuntimeError(f"unreviewed Viewer admission code: {code}")
    return AdmissionDecision(status, code)


def _value(item: object, key: str, default: object = None) -> object:
    if isinstance(item, Mapping):
        return item.get(key, default)
    return getattr(item, key, default)


def _typed_equal(left: object, right: object) -> bool:
    if type(left) is not type(right):
        return False
    if isinstance(left, Mapping):
        return set(left) == set(right) and all(
            _typed_equal(left[key], right[key]) for key in left
        )
    if isinstance(left, (list, tuple)):
        return len(left) == len(right) and all(
            _typed_equal(a, b) for a, b in zip(left, right, strict=True)
        )
    return left == right


def _header_value(context: AdmissionContext, key: str, default: object = None) -> object:
    value = context.header.get(key, default)
    if value is not None:
        return value
    runtime = context.header.get("runtime")
    return runtime.get(key, default) if isinstance(runtime, Mapping) else default


def _valid_scenario(scenario: Mapping[str, object]) -> bool:
    return (
        scenario.get("schema_version") == 1
        and scenario.get("name") == "two-vehicles-five-drives"
        and scenario.get("provenance") == "synthetic-only"
        and scenario.get("observed_at_ms") == 1788566400000
        and scenario.get("vehicle_ids") == [item["vehicle_id"] for item in SCENARIO_VEHICLES]
        and scenario.get("drive_pages_at_limit_2") == SCENARIO_DRIVE_PAGES
        and scenario.get("equal_start_time_ids") == [105, 104]
        and scenario.get("current") == SCENARIO_CURRENT
        and scenario.get("empty_vehicle_observed_at_ms") is None
        and scenario.get("null_distance_drive_id") == 101
        and scenario.get("vehicles") == SCENARIO_VEHICLES
        and scenario.get("later_current") == {
            "observed_at_ms": 1788566460000,
            "battery_level": 1,
            "inside_temp": 22.5,
            "outside_temp": None,
        }
    )


def _controller_hub_id(context: AdmissionContext) -> str | None:
    for observation in context.controller_observations.values():
        if isinstance(observation, Mapping) and isinstance(observation.get("hub_id"), str):
            return observation["hub_id"]
    return None


def _runtime(actor: object) -> Mapping[str, object]:
    value = _value(actor, "runtime", {})
    return value if isinstance(value, Mapping) else {}


def _context_problem(context: AdmissionContext) -> str | None:
    if context.adapter_id != ADAPTER_ID or context.cell_id not in CELL_IDS:
        return "wrong_context"
    try:
        parsed = uuid.UUID(context.session_id)
    except (ValueError, AttributeError):
        return "wrong_context"
    if parsed.version != 4 or str(parsed) != context.session_id:
        return "wrong_context"
    if not isinstance(context.scenario, Mapping) or not _valid_scenario(context.scenario):
        return "wrong_context"
    if set(context.actors) != set(ACTOR_IDS):
        return "wrong_actor"
    for actor_id in ACTOR_IDS:
        actor = context.actors.get(actor_id)
        if _value(actor, "id") != actor_id or _value(actor, "kind") != ACTOR_KINDS[actor_id]:
            return "wrong_actor"
        if tuple(_value(actor, "artifact_roles", ())) != (ACTOR_ROLES[actor_id][0],):
            return "wrong_artifact_role"
        if tuple(_value(actor, "source_roles", ())) != (ACTOR_ROLES[actor_id][1],):
            return "wrong_source_role"
        manifest = _value(actor, "installed_manifest")
        if (
            not isinstance(manifest, Mapping)
            or set(manifest) != {"path", "sha256"}
            or not isinstance(manifest.get("path"), str)
            or not manifest["path"].startswith("/")
            or not isinstance(manifest.get("sha256"), str)
            or HEX64.fullmatch(manifest["sha256"]) is None
        ):
            return "installed_manifest_mismatch"
    return None


def _artifact(context: AdmissionContext, role: str) -> Mapping[str, object] | None:
    artifacts = context.header.get("artifacts")
    if not isinstance(artifacts, (list, tuple)):
        return None
    for artifact in artifacts:
        if isinstance(artifact, Mapping) and artifact.get("role") == role:
            return artifact
    return None


def _expected_case(case_id: str, context: AdmissionContext) -> Mapping[str, object] | None:
    if case_id == "candidate_artifact_identity":
        hub = _artifact(context, "hub_executable")
        package = _artifact(context, "viewer_package_tarball")
        sdk = _artifact(context, "typescript_sdk_tarball")
        if hub is None or package is None or sdk is None:
            return None
        return {
            "hub_sha256": hub.get("sha256"),
            "tarball_sha256": sdk.get("sha256"),
            "package_version": package.get("embedded_version", PRODUCT_VERSION),
            "installed_members": 81,
        }
    if case_id == "installed_service_runtime":
        return {
            "service_mode": (
                "installed-app-launchagent"
                if context.cell_id.endswith("macos_arm64")
                else "installed-deb-systemd"
            ),
        }
    if case_id == "built_viewer_packed_sdk":
        runtime = _runtime(context.actors["viewer_ui"])
        bundle = runtime.get("bundle")
        sdk = runtime.get("sdk")
        if not isinstance(bundle, Mapping) or not isinstance(sdk, Mapping):
            return None
        return {
            "bundle_sha256": bundle.get("sha256"),
            "packed_sdk_sha256": sdk.get("sha256"),
            "rendered_exact_values": True,
        }
    if case_id == "discovery_identity_profile":
        hub_id = _controller_hub_id(context) or _header_value(context, "hub_id")
        if not isinstance(hub_id, str) or not hub_id:
            return None
        return {
            "hub_id": hub_id,
            "api_versions": ["1.0"],
            "protocol": "teslatlas-sync",
            "protocol_major": 1,
            "pack_format": "sqlite-zstd",
            "version": PRODUCT_VERSION,
        }
    static: dict[str, Mapping[str, object]] = {
        "unauthenticated_discovery": {
            "discovery": 200,
            "health": 200,
            "readiness": 200,
            "credential_absent": True,
        },
        "bad_invitation": {"typed_error": "hub_http_error", "http_status": 401},
        "expired_invitation": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
        "replayed_invitation": {"typed_error": "hub_http_error", "http_status": 401},
        "real_auth": {"claimed": 200, "vehicles": SCENARIO_VEHICLES},
        "credential_lifecycle_reauth": {"new_device": True, "vehicles": SCENARIO_VEHICLES},
        "revocation": {"typed_error": "hub_http_error", "http_status": 401},
        "unknown_vehicle": {"typed_error": "hub_http_error", "http_status": 404},
        "exact_current_values": {
            **SCENARIO_CURRENT,
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
        "drives_wrong_vehicle_cursor": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
        "drives_wrong_filter_cursor": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
        "real_browser_cors": {"preflight_succeeded": True, "cross_origin": True},
        "browser_normal_tls_validation": {
            "trusted_succeeded": True,
            "untrusted_error": "ERR_CERT_AUTHORITY_INVALID",
        },
    }
    return static.get(case_id)


def _expected_kind(case_id: str) -> str:
    return CASE_KINDS.get(case_id, "http")


def _request_shape(value: object) -> bool:
    if not isinstance(value, list):
        return False
    seen: set[str] = set()
    for request in value:
        if not isinstance(request, Mapping) or set(request) != {"method", "route", "status", "request_id"}:
            return False
        if request["method"] not in {"GET", "POST", "OPTIONS"}:
            return False
        if not isinstance(request["route"], str) or not request["route"].startswith("/"):
            return False
        if type(request["status"]) is not int or not 100 <= request["status"] <= 599:
            return False
        request_id = request["request_id"]
        if not isinstance(request_id, str) or not request_id or request_id in seen:
            return False
        seen.add(request_id)
    return True


def _transcript_proves_case(case_id: str, transcript: list[object]) -> bool:
    rules = REQUEST_RULES.get(case_id)
    if rules is None:
        return not transcript
    for method, route_pattern, statuses in rules:
        if not any(
            isinstance(request, Mapping)
            and request.get("method") == method
            and isinstance(request.get("route"), str)
            and route_pattern.search(request["route"])
            and request.get("status") in statuses
            for request in transcript
        ):
            return False
    return True


def _admit_invocations(case: Mapping[str, object], context: AdmissionContext, expected: Mapping[str, object]) -> AdmissionDecision:
    actor_id, operations = CASE_BINDINGS[case["id"]]
    invocations = [
        invocation
        for invocation in context.invocations
        if _value(invocation, "case_id") == case["id"]
    ]
    if len(invocations) != len(operations):
        return _decision("failed", "invocation_missing")
    by_operation = {_value(invocation, "operation"): invocation for invocation in invocations}
    if tuple(by_operation) != operations:
        return _decision("failed", "operation_mismatch")
    transcript: list[object] = []
    for operation in operations:
        invocation = by_operation[operation]
        if _value(invocation, "actor_id") != actor_id:
            return _decision("failed", "wrong_actor")
        before = _value(invocation, "session_sequence_before")
        after = _value(invocation, "session_sequence_after")
        if (
            type(before) is not int
            or type(after) is not int
            or before <= 0
            or after < before
            or before not in context.controller_observations
            or after not in context.controller_observations
        ):
            return _decision("failed", "sequence_mismatch")
        evidence_id = _value(invocation, "evidence_id")
        raw = context.raw.get(evidence_id)
        if not isinstance(raw, Mapping):
            return _decision("failed", "raw_missing")
        required = {
            "schema_version",
            "session_id",
            "cell_id",
            "session_input_sha256",
            "actor_id",
            "operation",
            "session_sequence_before",
            "session_sequence_after",
            "facts",
            "requests",
        }
        if set(raw) != required:
            return _decision("failed", "raw_identity_mismatch")
        if (
            raw["schema_version"] != 1
            or raw["session_id"] != context.session_id
            or raw["cell_id"] != context.cell_id
            or raw["actor_id"] != actor_id
            or raw["operation"] != operation
            or raw["session_sequence_before"] != before
            or raw["session_sequence_after"] != after
            or not isinstance(raw["session_input_sha256"], str)
            or HEX64.fullmatch(raw["session_input_sha256"]) is None
        ):
            return _decision("failed", "raw_identity_mismatch")
        if not _typed_equal(raw["facts"], expected):
            return _decision("failed", "raw_fact_mismatch")
        if not _request_shape(raw["requests"]):
            return _decision("failed", "request_mismatch")
        request_ids = tuple(request["request_id"] for request in raw["requests"])
        if request_ids != tuple(_value(invocation, "request_ids", ())):
            return _decision("failed", "request_mismatch")
        transcript.extend(raw["requests"])
    if not _typed_equal(case["request_transcript"], transcript):
        return _decision("failed", "request_mismatch")
    if _expected_kind(case["id"]) == "zero_request" and transcript:
        return _decision("failed", "request_mismatch")
    if not _transcript_proves_case(case["id"], transcript):
        return _decision("failed", "request_mismatch")
    return _decision("passed", "accepted")


def admit_case(case: dict, context: AdmissionContext) -> AdmissionDecision:
    """Admit one Viewer case using only runner-created immutable context views."""
    if not isinstance(case, dict) or set(case) != {
        "id",
        "status",
        "expected",
        "actual",
        "evidence_kind",
        "request_transcript",
    } and not (
        isinstance(case, dict)
        and set(case)
        == {
            "id",
            "status",
            "expected",
            "actual",
            "evidence_kind",
            "request_transcript",
            "process_evidence",
        }
    ):
        return _decision("failed", "case_shape")
    case_id = case.get("id")
    if case_id not in REQUIRED_CASES:
        return _decision("failed", "unknown_case")
    problem = _context_problem(context)
    if problem is not None:
        return _decision("failed", problem)
    expected = _expected_case(case_id, context)
    if expected is None:
        return _decision("pending", "evidence_pending")
    if case.get("evidence_kind") != _expected_kind(case_id):
        return _decision("failed", "case_shape")
    if case_id == "installed_service_runtime":
        if case.get("status") != "pending" or not _typed_equal(case.get("expected"), expected) or not _typed_equal(case.get("actual"), expected):
            return _decision("failed", "literal_mismatch")
        return _decision("pending", "evidence_pending")
    if case.get("status") == "pending":
        return _decision("pending", "evidence_pending")
    if case.get("status") != "passed":
        return _decision("failed", "case_shape")
    if not _typed_equal(case.get("expected"), expected) or not _typed_equal(case.get("actual"), expected):
        return _decision("failed", "literal_mismatch")
    if _expected_kind(case_id) == "identity":
        process = case.get("process_evidence")
        if not isinstance(process, Mapping) or not process:
            return _decision("failed", "raw_missing")
    return _admit_invocations(case, context, expected)
