"""Pure semantic admission for the installed dual-transport Swift matrix adapter."""

from __future__ import annotations

import re
import hashlib
import json
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from types import MappingProxyType
from typing import Literal
from pathlib import Path

ADAPTER_ID = "swift"
CONTRACT_REVISION = 1
PRODUCT_VERSION = "2026.36.2"
PROFILE_SHA256 = "b3914d35d28374f6423af789e9ed6a4a4c82196a068c041946e24d609db0b05b"
ACTOR_IDS = ("swift_macos", "swift_linux")
PHASE_RECIPES = (
    ("bootstrap_pair", ("verify", "pair")),
    ("revoke_and_pair", ("revoke", "pair")),
    ("restart", ("verify", "stop", "start")),
    ("outage_stop", ("stop",)),
    ("outage_start", ("start",)),
    ("final_verify", ("verify",)),
)
SOURCE_ROLE = "swift_sdk_source"
ARTIFACT_ROLE = "swift_sdk_product"
UNKNOWN_VEHICLE = "33333333-3333-4333-8333-333333333333"
UNICODE_NAME = "Interop – Árvíztűrő 🚗"
HEX64 = re.compile(r"^[0-9a-f]{64}$")
TOKEN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$")

REQUIRED_CASES = (
    "candidate_artifact_identity", "installed_service_runtime",
    "discovery_identity_profile", "unauthenticated_discovery",
    "bad_invitation", "expired_invitation", "replayed_invitation",
    "real_auth", "credential_lifecycle_reauth", "revocation",
    "unknown_vehicle", "exact_current_values", "endpoint_restart",
    "outage_recovery", "unsupported_operation_zero_requests",
    "credential_rotation_api", "drives_three_page_order",
    "drives_terminal_cursor", "drives_etag_304",
    "drives_wrong_vehicle_cursor", "drives_wrong_filter_cursor",
    "native_macos_transport", "native_linux_transport",
    "transport_cancellation", "transport_body_limit",
)

CASE_OPERATIONS = MappingProxyType({
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
    "native_macos_transport": ("transport_identity",),
    "native_linux_transport": ("transport_identity",),
    "transport_cancellation": ("transport_cancellation",),
    "transport_body_limit": ("transport_body_limit",),
})
CASE_ACTORS = MappingProxyType({case_id: ACTOR_IDS for case_id in REQUIRED_CASES})
CASE_KINDS = MappingProxyType({
    case_id: (
        "identity" if case_id in {"candidate_artifact_identity", "installed_service_runtime"}
        else "zero_request" if case_id in {
            "expired_invitation", "unsupported_operation_zero_requests",
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
    actors: Mapping
    invocations: tuple
    raw: Mapping
    controller_observations: Mapping


@dataclass(frozen=True)
class AdmissionDecision:
    status: Literal["passed", "failed", "pending"]
    code: str


def _typed_equal(left, right):
    if type(left) is not type(right):
        return False
    if isinstance(left, dict) or isinstance(left, Mapping):
        return set(left) == set(right) and all(_typed_equal(left[key], right[key]) for key in left)
    if isinstance(left, (list, tuple)):
        return len(left) == len(right) and all(_typed_equal(a, b) for a, b in zip(left, right))
    return left == right


def _canonical_uuid(value):
    try:
        parsed = uuid.UUID(value) if isinstance(value, str) else None
    except ValueError:
        return False
    return parsed is not None and str(parsed) == value


def _artifact_digest(header):
    artifacts = header.get("artifacts") if isinstance(header, Mapping) else None
    if not isinstance(artifacts, list):
        return None
    rows = [row for row in artifacts if isinstance(row, dict) and row.get("role") == ARTIFACT_ROLE]
    return rows[0].get("sha256") if len(rows) == 1 else None


def _runtime_fact(actor_id, context_values):
    """Return only independently admitted runner runtime evidence.

    There is deliberately no label-only or child-raw fallback here.  The
    normalized transport cases must byte-for-byte agree with the closed view
    constructed from the runner's launch and installation inventory.
    """
    runtime = context_values.get("actor_runtime_facts", {}).get(actor_id)
    if not isinstance(runtime, Mapping):
        return None
    transport = runtime.get("transport", {})
    platform = runtime.get("platform", {})
    os_name = "macOS" if actor_id == "swift_macos" else "Ubuntu 22.04.5"
    return {"os": os_name, "transport": transport.get("kind"), "trusted_tls": True}


def _semantic(case_id, actor_id, values):
    common = {
        "candidate_artifact_identity": {
            "hub_sha256": values.get("hub_executable_sha256"),
            "tarball_sha256": values.get("swift_sdk_product_sha256"),
            "package_version": PRODUCT_VERSION,
            "installed_members": len(values.get("actor_installed_members", {}).get(actor_id) or []),
        },
        "installed_service_runtime": {"service_mode": values.get("service_mode")},
        "discovery_identity_profile": {
            "hub_id": values.get("hub_id"), "api_versions": ["1.0"],
            "protocol": "teslatlas-sync", "protocol_major": 1,
            "pack_format": "sqlite-zstd", "version": PRODUCT_VERSION,
        },
        "unauthenticated_discovery": {"discovery": 200, "health": 200, "readiness": 200, "credential_absent": True},
        "bad_invitation": {"pairing_id": values.get("initial_pairing_id"), "typed_error": "unauthorized", "http_status": 401, "credential_created": False},
        "expired_invitation": {"pairing_id": values.get("expired_pairing_id"), "expires_at_ms": values.get("expired_expires_at_ms"), "observed_after_expiry": True, "outgoing_requests": 0, "typed_error": "invitationExpired", "credential_created": False},
        "replayed_invitation": {"pairing_id": values.get("initial_pairing_id"), "typed_error": "unauthorized", "http_status": 401, "credential_created": False},
        "real_auth": {"pairing_id": values.get("initial_pairing_id"), "claimed": 200, "vehicles": [
            {"vehicle_id": "11111111-1111-4111-8111-111111111111", "display_name": UNICODE_NAME},
            {"vehicle_id": "22222222-2222-4222-8222-222222222222", "display_name": "Interop empty"},
        ]},
        "credential_lifecycle_reauth": {"pairing_id": values.get("reauth_pairing_id"), "new_device": True, "vehicles": 200, "fresh_claim": 200},
        "revocation": {"typed_error": "unauthorized", "http_status": 401},
        "unknown_vehicle": {"typed_error": "notFound", "http_status": 404, "vehicle_id": UNKNOWN_VEHICLE},
        "exact_current_values": {
            "battery_level": 0, "inside_temp": 21.5, "outside_temp": None,
            "observed_at_ms": 1788566400000, "est_battery_range_km": 160.93,
            "odometer": 16093.44, "speed": 16,
            "scheduled_charging_start_time": 1788570000,
            "active_route_miles_to_arrival": 12.5,
            "empty_vehicle_observed_at_ms": None,
        },
        "endpoint_restart": {"same_hub": True, "new_process": True, "vehicles": 200},
        "outage_recovery": {"outage_observed": True, "vehicles": 200},
        "unsupported_operation_zero_requests": {"outgoing_requests": 0},
        "credential_rotation_api": {"rotated": True, "same_device": True, "vehicles": 200, "old_credential_error": "unauthorized", "old_credential_status": 401},
        "drives_three_page_order": {"pages": [[105, 104], [103, 102], [101]]},
        "drives_terminal_cursor": {"next_cursor": None, "ids": [101]},
        "drives_etag_304": {"kind": "notModified", "post_304_ids": [103, 102]},
        "drives_wrong_vehicle_cursor": {"typed_error": "api", "http_status": 400, "error_code": "invalid_cursor"},
        "drives_wrong_filter_cursor": {"typed_error": "api", "http_status": 400, "error_code": "invalid_cursor"},
        "native_macos_transport": _runtime_fact(actor_id, values),
        "native_linux_transport": _runtime_fact(actor_id, values),
        "transport_cancellation": {"cancelled": True, "typed_error": "CancellationError", "bounded": True},
        "transport_body_limit": {"limit_bytes": 1048576, "oversize_rejected": True, "typed_error": "invalidResponse"},
    }
    return common[case_id]


def expected_facts(case_id, values):
    if case_id not in REQUIRED_CASES:
        raise KeyError(case_id)
    return {actor_id: _semantic(case_id, actor_id, values) for actor_id in ACTOR_IDS}


def _decision(code, pending=False):
    return AdmissionDecision("pending" if pending else "failed", code)


def _expected_from_context(case_id, context):
    artifact_digest = _artifact_digest(context.header)
    values = {
        "hub_id": None,
        "swift_sdk_product_sha256": artifact_digest,
        "hub_executable_sha256": None,
        "actor_manifest_sha256s": {
            actor_id: context.actors[actor_id].installed_manifest.get("sha256")
            for actor_id in ACTOR_IDS if actor_id in context.actors
        },
        "actor_installed_members": {
            actor_id: context.actors[actor_id].runtime.get("artifact", {}).get("installed_members")
            for actor_id in ACTOR_IDS if actor_id in context.actors
        },
        "actor_runtime_facts": {
            actor_id: context.actors[actor_id].runtime
            for actor_id in ACTOR_IDS if actor_id in context.actors
        },
        "service_mode": None,
        "initial_pairing_id": None,
        "expired_pairing_id": None,
        "expired_expires_at_ms": None,
        "expired_observed_at_ms": None,
        "reauth_pairing_id": None,
    }
    if isinstance(context.header, Mapping):
        artifacts = context.header.get("artifacts", [])
        hubs = [row for row in artifacts if isinstance(row, Mapping) and row.get("role") == "hub_executable"]
        values["hub_executable_sha256"] = hubs[0].get("sha256") if len(hubs) == 1 else None
    if isinstance(context.header, Mapping):
        runtime = context.header.get("runtime", {})
        hub = runtime.get("hub", {}) if isinstance(runtime, Mapping) else {}
        values["service_mode"] = hub.get("service_mode")
    observations = [item for _, item in sorted(context.controller_observations.items())]
    for observation in observations:
        if isinstance(observation, Mapping) and observation.get("hub_id"):
            values["hub_id"] = observation["hub_id"]
            break
    expected = {}
    for actor_id in ACTOR_IDS:
        actor_values = dict(values)
        invocation = next((item for item in context.invocations if item.case_id == case_id and item.actor_id == actor_id), None)
        anchor_sequence = (
            invocation.session_sequence_after
            if invocation is not None and case_id == "credential_lifecycle_reauth"
            else invocation.session_sequence_before if invocation is not None else None
        )
        anchor = context.controller_observations.get(anchor_sequence)
        invitations = anchor.get("invitations") if isinstance(anchor, Mapping) else None
        if isinstance(invitations, Mapping):
            active = invitations.get("active")
            expired = invitations.get("expired")
            if isinstance(active, Mapping):
                actor_values["initial_pairing_id"] = active.get("pairing_id")
                actor_values["reauth_pairing_id"] = active.get("pairing_id")
            if isinstance(expired, Mapping):
                actor_values["expired_pairing_id"] = expired.get("pairing_id")
                actor_values["expired_expires_at_ms"] = expired.get("expires_at_ms")
                actor_values["expired_observed_at_ms"] = anchor.get("observed_at_ms")
        expected[actor_id] = _semantic(case_id, actor_id, actor_values)
    return expected


def _expected_normalized_from_context(case_id, context):
    actor_facts = _expected_from_context(case_id, context)
    if case_id == "native_macos_transport":
        return actor_facts["swift_macos"]
    if case_id == "native_linux_transport":
        return actor_facts["swift_linux"]
    keys = {
        "bad_invitation": {"typed_error", "http_status"},
        "expired_invitation": {"outgoing_requests", "typed_error"},
        "replayed_invitation": {"typed_error", "http_status"},
        "real_auth": {"claimed", "vehicles"},
        "credential_lifecycle_reauth": {"new_device", "vehicles"},
    }.get(case_id)
    first = actor_facts["swift_macos"]
    second = actor_facts["swift_linux"]
    if keys is not None:
        first = {key: first[key] for key in keys}
        second = {key: second[key] for key in keys}
    return first if _typed_equal(first, second) else None


def expected_normalized_facts(case_id, context):
    if case_id not in REQUIRED_CASES:
        raise KeyError(case_id)
    return _expected_normalized_from_context(case_id, context)


def _valid_binding(value):
    return (
        isinstance(value, Mapping)
        and set(value) == {"path", "sha256"}
        and isinstance(value["path"], str)
        and value["path"].startswith("/")
        and ".." not in value["path"].split("/")
        and HEX64.fullmatch(str(value["sha256"])) is not None
    )


def _valid_member_list(value):
    if not isinstance(value, list):
        return False
    paths = []
    for member in value:
        if (
            not isinstance(member, Mapping)
            or set(member) != {"path", "sha256", "bytes", "mode"}
            or not isinstance(member["path"], str)
            or not member["path"]
            or member["path"].startswith("/")
            or ".." in member["path"].split("/")
            or HEX64.fullmatch(str(member["sha256"])) is None
            or type(member["bytes"]) is not int
            or member["bytes"] < 0
            or type(member["mode"]) is not int
            or not 0 <= member["mode"] <= 0o7777
        ):
            return False
        paths.append(member["path"])
    return paths == sorted(set(paths))


def _valid_runtime(actor_id, runtime):
    required = {
        "schema_version", "runtime_ref", "runtime_kind", "execution",
        "platform", "compiler", "test_product", "transport", "artifact",
        "actor_input_manifest_sha256", "phase_admissions",
    }
    if not isinstance(runtime, Mapping) or set(runtime) != required:
        return False
    if runtime["schema_version"] != 1 or runtime["runtime_ref"] != actor_id:
        return False
    if HEX64.fullmatch(str(runtime["actor_input_manifest_sha256"])) is None:
        return False
    phases = runtime["phase_admissions"]
    if not isinstance(phases, list) or len(phases) != len(PHASE_RECIPES):
        return False
    for ordinal, ((phase_id, _), phase) in enumerate(zip(PHASE_RECIPES, phases), 1):
        if (
            not isinstance(phase, Mapping)
            or set(phase) != {
                "ordinal", "phase_id", "session_sequence_before",
                "session_sequence_after", "ready_sha256",
            }
            or phase["ordinal"] != ordinal
            or phase["phase_id"] != phase_id
            or type(phase["session_sequence_before"]) is not int
            or type(phase["session_sequence_after"]) is not int
            or phase["session_sequence_before"] <= 0
            or phase["session_sequence_after"] <= phase["session_sequence_before"]
            or HEX64.fullmatch(str(phase["ready_sha256"])) is None
        ):
            return False
    platform = runtime["platform"]
    if not isinstance(platform, Mapping) or set(platform) != {"os", "version", "architecture"}:
        return False
    if platform["architecture"] not in {"arm64", "amd64"}:
        return False
    compiler = runtime["compiler"]
    if (
        not isinstance(compiler, Mapping)
        or set(compiler) != {"swift_version", "swiftc"}
        or not isinstance(compiler["swift_version"], str)
        or not compiler["swift_version"]
        or not _valid_binding(compiler["swiftc"])
    ):
        return False
    product = runtime["test_product"]
    if (
        not isinstance(product, Mapping)
        or set(product) != {"executable", "modules", "resources", "dependencies"}
        or not _valid_binding(product["executable"])
        or not all(_valid_member_list(product[key]) for key in ("modules", "resources", "dependencies"))
    ):
        return False
    artifact = runtime["artifact"]
    if (
        not isinstance(artifact, Mapping)
        or set(artifact) != {"role", "sha256", "installed_root", "installed_manifest_sha256", "installed_members"}
        or artifact["role"] != ARTIFACT_ROLE
        or HEX64.fullmatch(str(artifact["sha256"])) is None
        or not isinstance(artifact["installed_root"], str)
        or not artifact["installed_root"].startswith("/")
        or HEX64.fullmatch(str(artifact["installed_manifest_sha256"])) is None
        or not _valid_member_list(artifact["installed_members"])
    ):
        return False
    execution = runtime["execution"]
    transport = runtime["transport"]
    if actor_id == "swift_macos":
        return (
            runtime["runtime_kind"] == "native-process"
            and platform["os"] == "macOS"
            and isinstance(execution, Mapping)
            and set(execution) == {"kind", "executable", "argv_sha256", "environment_sha256"}
            and execution["kind"] == "native-process"
            and _valid_binding(execution["executable"])
            and HEX64.fullmatch(str(execution["argv_sha256"])) is not None
            and HEX64.fullmatch(str(execution["environment_sha256"])) is not None
            and isinstance(transport, Mapping)
            and set(transport) == {"kind", "foundation"}
            and transport["kind"] == "URLSession"
            and _valid_binding(transport["foundation"])
        )
    return (
        runtime["runtime_kind"] == "docker-container"
        and platform == {"os": "Ubuntu", "version": "22.04.5", "architecture": platform["architecture"]}
        and compiler["swift_version"] == "6.0.3"
        and isinstance(execution, Mapping)
        and set(execution) == {"kind", "container_id", "image_id", "executable", "argv_sha256", "environment_sha256"}
        and execution["kind"] == "docker-exec"
        and HEX64.fullmatch(str(execution["container_id"])) is not None
        and HEX64.fullmatch(str(execution["image_id"])) is not None
        and _valid_binding(execution["executable"])
        and HEX64.fullmatch(str(execution["argv_sha256"])) is not None
        and HEX64.fullmatch(str(execution["environment_sha256"])) is not None
        and isinstance(transport, Mapping)
        and set(transport) == {"kind", "libcurl", "tls_backend", "libssl", "libcrypto"}
        and transport["kind"] == "libcurl"
        and transport["tls_backend"] == "OpenSSL"
        and isinstance(transport["libcurl"], Mapping)
        and set(transport["libcurl"]) == {"path", "sha256", "version"}
        and _valid_binding({"path": transport["libcurl"]["path"], "sha256": transport["libcurl"]["sha256"]})
        and isinstance(transport["libcurl"]["version"], str)
        and bool(transport["libcurl"]["version"])
        and _valid_binding(transport["libssl"])
        and _valid_binding(transport["libcrypto"])
    )


def _valid_actor(actor_id, actor):
    if not isinstance(actor, AdmittedActor) or actor.id != actor_id or actor.kind != "current_swift_transport":
        return "wrong_actor"
    expected_entry = "swift_current_native_test" if actor_id == "swift_macos" else "swift_current_linux_test"
    if actor.runtime_ref != actor_id or actor.entrypoint_ref != expected_entry:
        return "wrong_actor"
    if actor.source_roles != (SOURCE_ROLE,):
        return "wrong_source_role"
    if actor.artifact_roles != (ARTIFACT_ROLE,):
        return "wrong_artifact_role"
    runtime = actor.runtime
    if not _valid_runtime(actor_id, runtime):
        return "wrong_runtime"
    manifest = actor.installed_manifest
    if not isinstance(manifest, Mapping) or set(manifest) != {"path", "sha256"} or HEX64.fullmatch(str(manifest.get("sha256", ""))) is None:
        return "installed_manifest_mismatch"
    return None


def _request_requirement(case_id, requests):
    if CASE_KINDS[case_id] in {"identity", "zero_request"}:
        return len(requests) == 0
    required = {
        "discovery_identity_profile": {("GET", "/.well-known/teslatlas-hub", 200)},
        "unauthenticated_discovery": {("GET", "/.well-known/teslatlas-hub", 200), ("GET", "/healthz", 200), ("GET", "/readyz", 200)},
        "bad_invitation": {("POST", "/v1/pairings/{pairing_id}/claim", 401)},
        "replayed_invitation": {("POST", "/v1/pairings/{pairing_id}/claim", 401)},
        "unknown_vehicle": {("GET", "/v1/vehicles/{vehicle_id}/current", 404)},
        "revocation": {("GET", "/v1/vehicles", 401)},
        "drives_etag_304": {("GET", "/v1/vehicles/{vehicle_id}/drives", 304), ("GET", "/v1/vehicles/{vehicle_id}/drives", 200)},
        "drives_wrong_vehicle_cursor": {("GET", "/v1/vehicles/{vehicle_id}/drives", 400)},
        "drives_wrong_filter_cursor": {("GET", "/v1/vehicles/{vehicle_id}/drives", 400)},
    }.get(case_id)
    if required is None:
        return bool(requests)
    actual = {(item.get("method"), item.get("route"), item.get("status")) for item in requests if isinstance(item, Mapping)}
    return required <= actual


def _valid_phase_admissions(actor, observations):
    phases = actor.runtime["phase_admissions"]
    for index, ((_, recipe), phase) in enumerate(zip(PHASE_RECIPES, phases)):
        before_sequence = phase["session_sequence_before"]
        after_sequence = phase["session_sequence_after"]
        before = observations.get(before_sequence)
        after = observations.get(after_sequence)
        if before is None or after is None:
            return False
        if index > 0 and before_sequence != phases[index - 1]["session_sequence_after"]:
            return False
        interval = [
            observation for sequence, observation in sorted(observations.items())
            if before_sequence < sequence <= after_sequence
        ]
        if [observation["operation"] for observation in interval] != list(recipe):
            return False
        if not interval or interval[-1]["sequence"] != after_sequence:
            return False
        previous = before
        for observation in interval:
            if observation["operation"] != "verify":
                transition = observation["transition"]
                if transition["from_sequence"] != previous["sequence"]:
                    return False
            previous = observation
        if phase["phase_id"] == "outage_stop":
            if after["state"] != "stopped":
                return False
        elif after["state"] != "running":
            return False
    return len({phase["ready_sha256"] for phase in phases}) == len(phases)


def _controller_semantics(case_id, invocations, context):
    observations = context.controller_observations
    required = {"schema_version", "session_id", "sequence", "operation", "state", "started_monotonic_ns", "finished_monotonic_ns", "observed_at_ms", "result_sha256", "proof_sha256", "scenario_sha256", "seed_sha256", "store_id", "store_schema_version", "hub_id", "service_generation", "invitations", "transition"}
    if not isinstance(observations, Mapping) or not observations:
        return False
    for sequence, observation in observations.items():
        if not isinstance(observation, Mapping) or set(observation) != required:
            return False
        if observation["schema_version"] != 1 or observation["session_id"] != context.session_id or observation["sequence"] != sequence or type(sequence) is not int or sequence <= 0:
            return False
        if (
            type(observation["started_monotonic_ns"]) is not int
            or observation["started_monotonic_ns"] < 0
            or type(observation["finished_monotonic_ns"]) is not int
            or observation["finished_monotonic_ns"] < observation["started_monotonic_ns"]
            or type(observation["observed_at_ms"]) is not int
            or observation["observed_at_ms"] <= 0
            or type(observation["store_schema_version"]) is not int
            or observation["store_schema_version"] <= 0
            or any(HEX64.fullmatch(str(observation[key])) is None for key in ("result_sha256", "scenario_sha256", "seed_sha256"))
            or not _canonical_uuid(observation["store_id"])
            or not _canonical_uuid(observation["hub_id"])
            or not isinstance(observation["service_generation"], str)
            or TOKEN.fullmatch(observation["service_generation"]) is None
        ):
            return False
        operation = observation["operation"]
        if operation not in {"verify", "advance-once", "pair", "revoke", "stop", "start"}:
            return False
        if observation["state"] == "running":
            invitations = observation["invitations"]
            if (
                HEX64.fullmatch(str(observation["proof_sha256"])) is None
                or not isinstance(invitations, Mapping)
                or set(invitations) != {"active", "expired"}
            ):
                return False
            for invitation in invitations.values():
                if (
                    not isinstance(invitation, Mapping)
                    or set(invitation) != {"pairing_id", "expires_at_ms"}
                    or not _canonical_uuid(invitation["pairing_id"])
                    or type(invitation["expires_at_ms"]) is not int
                    or invitation["expires_at_ms"] <= 0
                ):
                    return False
            if (
                invitations["active"]["pairing_id"] == invitations["expired"]["pairing_id"]
                or invitations["active"]["expires_at_ms"] <= observation["observed_at_ms"]
                or invitations["expired"]["expires_at_ms"] >= observation["observed_at_ms"]
            ):
                return False
        elif observation["state"] == "stopped":
            if observation["proof_sha256"] is not None or observation["invitations"] is not None or observation["operation"] != "stop":
                return False
        else:
            return False
        transition = observation["transition"]
        if operation == "verify":
            if transition is not None:
                return False
        else:
            transition_keys = {
                "advance-once": {"kind", "from_sequence", "pre_advance_verify_sequence", "before_store_sha256", "after_store_sha256", "scenario_sha256", "seed_sha256"},
                "revoke": {"kind", "from_sequence", "device_id"},
                "pair": {"kind", "from_sequence"},
                "stop": {"kind", "from_sequence"},
                "start": {"kind", "from_sequence", "stopped_sequence"},
            }[operation]
            if not isinstance(transition, Mapping) or set(transition) != transition_keys or transition["kind"] != operation:
                return False
            origin = transition["from_sequence"]
            if type(origin) is not int or origin not in observations or origin >= sequence:
                return False
            if operation == "advance-once":
                if (
                    type(transition["pre_advance_verify_sequence"]) is not int
                    or transition["pre_advance_verify_sequence"] not in observations
                    or observations[transition["pre_advance_verify_sequence"]]["operation"] != "verify"
                    or any(HEX64.fullmatch(str(transition[key])) is None for key in ("before_store_sha256", "after_store_sha256", "scenario_sha256", "seed_sha256"))
                    or transition["scenario_sha256"] != observation["scenario_sha256"]
                    or transition["seed_sha256"] != observation["seed_sha256"]
                ):
                    return False
            if operation == "revoke" and (not isinstance(transition["device_id"], str) or not transition["device_id"]):
                return False
            if operation == "start":
                stopped_sequence = transition["stopped_sequence"]
                if type(stopped_sequence) is not int or stopped_sequence not in observations or observations[stopped_sequence]["state"] != "stopped" or origin != stopped_sequence:
                    return False
        if operation != "verify":
            previous = observations[observation["transition"]["from_sequence"]]
            immutable = ("scenario_sha256", "seed_sha256", "store_id", "store_schema_version", "hub_id")
            if any(observation[key] != previous[key] for key in immutable):
                return False
            if operation == "stop" and observation["service_generation"] != previous["service_generation"]:
                return False
            if operation == "start" and observation["service_generation"] == previous["service_generation"]:
                return False
    ordered_observations = [item for _, item in sorted(observations.items())]
    immutable_keys = ("scenario_sha256", "seed_sha256", "store_id", "store_schema_version", "hub_id")
    first_observation = ordered_observations[0]
    if any(
        observation[key] != first_observation[key]
        for observation in ordered_observations
        for key in immutable_keys
    ):
        return False
    for previous, observation in zip(ordered_observations, ordered_observations[1:]):
        if observation["operation"] != "start" and observation["service_generation"] != previous["service_generation"]:
            return False
    if any(not _valid_phase_admissions(context.actors[actor_id], observations) for actor_id in ACTOR_IDS):
        return False
    mac_phases = context.actors["swift_macos"].runtime["phase_admissions"]
    linux_phases = context.actors["swift_linux"].runtime["phase_admissions"]
    if mac_phases[-1]["session_sequence_after"] != linux_phases[0]["session_sequence_before"]:
        return False
    phase_by_actor = {
        actor_id: {
            phase["phase_id"]: phase
            for phase in context.actors[actor_id].runtime["phase_admissions"]
        }
        for actor_id in ACTOR_IDS
    }
    for invocation in invocations:
        before = observations.get(invocation.session_sequence_before)
        after = observations.get(invocation.session_sequence_after)
        if before is None or after is None:
            return False
        if before["session_id"] != context.session_id or after["session_id"] != context.session_id:
            return False
        phases = phase_by_actor[invocation.actor_id]
        bootstrap = phases["bootstrap_pair"]
        revoke_pair = phases["revoke_and_pair"]
        restart = phases["restart"]
        outage_stop = phases["outage_stop"]
        outage_start = phases["outage_start"]
        if case_id in {"credential_lifecycle_reauth", "revocation"}:
            expected_anchor = (
                bootstrap["session_sequence_after"],
                revoke_pair["session_sequence_after"],
            )
        elif case_id == "endpoint_restart":
            expected_anchor = (
                restart["session_sequence_before"],
                restart["session_sequence_after"],
            )
        elif case_id == "outage_recovery":
            expected_anchor = (
                outage_stop["session_sequence_before"],
                outage_start["session_sequence_after"],
            )
        elif case_id in {"transport_cancellation", "transport_body_limit"}:
            expected_anchor = (
                outage_start["session_sequence_after"],
                outage_start["session_sequence_after"],
            )
        else:
            expected_anchor = (
                bootstrap["session_sequence_after"],
                bootstrap["session_sequence_after"],
            )
        if (
            invocation.session_sequence_before,
            invocation.session_sequence_after,
        ) != expected_anchor:
            return False
    if case_id == "endpoint_restart":
        for invocation in invocations:
            before = observations[invocation.session_sequence_before]
            after = observations[invocation.session_sequence_after]
            transition = after["transition"]
            if (
                before["state"] != "running"
                or after["operation"] != "start"
                or not isinstance(transition, Mapping)
                or observations[transition["stopped_sequence"]]["transition"]["from_sequence"]
                != next(
                    item["sequence"] for sequence, item in sorted(observations.items())
                    if before["sequence"] < sequence < transition["stopped_sequence"]
                    and item["operation"] == "verify"
                )
                or before["service_generation"] == after["service_generation"]
                or before["hub_id"] != after["hub_id"]
                or before["store_id"] != after["store_id"]
                or before["store_schema_version"] != after["store_schema_version"]
            ):
                return False
    if case_id in {"bad_invitation", "replayed_invitation", "real_auth", "credential_lifecycle_reauth"}:
        active_ids = [item.get("pairing_id") for observation in observations.values() if isinstance(observation.get("invitations"), Mapping) for item in [observation["invitations"].get("active")] if isinstance(item, Mapping)]
        if not active_ids:
            return False
    if case_id == "expired_invitation":
        fixtures = [(item, observation["observed_at_ms"]) for observation in observations.values() if isinstance(observation.get("invitations"), Mapping) for item in [observation["invitations"].get("expired")] if isinstance(item, Mapping)]
        if not fixtures or type(fixtures[0][0].get("expires_at_ms")) is not int or fixtures[0][0]["expires_at_ms"] >= fixtures[0][1]:
            return False
    if case_id == "credential_lifecycle_reauth":
        for invocation in invocations:
            after = observations[invocation.session_sequence_after]
            if after["operation"] != "pair" or after["transition"]["from_sequence"] < invocation.session_sequence_before:
                return False
    if case_id == "revocation":
        for invocation in invocations:
            transitions = [
                item for sequence, item in observations.items()
                if invocation.session_sequence_before < sequence <= invocation.session_sequence_after
                and item["operation"] == "revoke"
            ]
            if len(transitions) != 1:
                return False
    if case_id == "outage_recovery":
        for invocation in invocations:
            before = observations[invocation.session_sequence_before]
            after = observations[invocation.session_sequence_after]
            if after["operation"] != "start" or after["state"] != "running" or observations[after["transition"]["stopped_sequence"]]["transition"]["from_sequence"] != before["sequence"]:
                return False
    return True


def admit_case(case, context):
    required_case_fields = {"id", "status", "expected", "actual", "evidence_kind", "request_transcript"}
    if not isinstance(case, dict) or set(case) != required_case_fields:
        return _decision("case_shape")
    case_id = case.get("id")
    if case_id not in REQUIRED_CASES:
        return _decision("unknown_case")
    if context.adapter_id != ADAPTER_ID or context.cell_id not in {
        "swift__macos_arm64", "swift__debian13_amd64", "swift__debian13_arm64"
    } or not isinstance(context.session_id, str):
        return _decision("wrong_context")
    if set(context.actors) != set(ACTOR_IDS):
        return _decision("wrong_actor")
    for actor_id in ACTOR_IDS:
        problem = _valid_actor(actor_id, context.actors[actor_id])
        if problem:
            return _decision(problem)
        actor = context.actors[actor_id]
        if (
            actor.runtime["artifact"]["sha256"] != _artifact_digest(context.header)
            or actor.runtime["actor_input_manifest_sha256"] != actor.installed_manifest["sha256"]
        ):
            return _decision("installed_manifest_mismatch")
    if case["evidence_kind"] != CASE_KINDS[case_id]:
        return _decision("case_shape")
    expected = _expected_from_context(case_id, context)
    normalized_expected = _expected_normalized_from_context(case_id, context)
    if not _typed_equal(case["expected"], case["actual"]) or not _typed_equal(case["expected"], normalized_expected):
        return _decision("literal_mismatch")
    invocations = [item for item in context.invocations if item.case_id == case_id]
    if {item.actor_id for item in invocations} != set(ACTOR_IDS) or len(invocations) != len(ACTOR_IDS):
        return _decision("wrong_actor" if invocations else "invocation_missing")
    if not _controller_semantics(case_id, invocations, context):
        return _decision("controller_mismatch")
    combined_requests = []
    for invocation in invocations:
        if invocation.operation not in CASE_OPERATIONS[case_id]:
            return _decision("operation_mismatch")
        raw = context.raw.get(invocation.evidence_id)
        if raw is None:
            return _decision("raw_missing")
        actor = context.actors[invocation.actor_id]
        required_raw = {"schema_version", "session_id", "cell_id", "session_input_sha256", "actor_id", "operation", "actor_manifest_sha256", "session_sequence_before", "session_sequence_after", "facts", "requests", "cleanup"}
        if not isinstance(raw, Mapping) or set(raw) != required_raw:
            return _decision("raw_identity_mismatch")
        if raw["session_id"] != context.session_id or raw["cell_id"] != context.cell_id or raw["actor_id"] != invocation.actor_id or raw["operation"] != invocation.operation:
            return _decision("raw_identity_mismatch")
        if raw["actor_manifest_sha256"] != actor.installed_manifest["sha256"]:
            return _decision("installed_manifest_mismatch")
        if raw["session_sequence_before"] != invocation.session_sequence_before or raw["session_sequence_after"] != invocation.session_sequence_after or type(invocation.session_sequence_before) is not int or invocation.session_sequence_before <= 0 or invocation.session_sequence_after < invocation.session_sequence_before:
            return _decision("sequence_mismatch")
        if not _typed_equal(raw["facts"], expected[invocation.actor_id]):
            return _decision("raw_fact_mismatch")
        cleanup = raw["cleanup"]
        if cleanup != {
            "status": "passed", "transport_resources_closed": True,
            "auxiliary_fixture_stopped": True, "process_exited": True,
        }:
            return _decision("cleanup_failure")
        requests = raw["requests"]
        if any(
            not isinstance(item, Mapping)
            or set(item) != {"method", "route", "status", "request_id"}
            or item["method"] not in {"GET", "POST"}
            or not isinstance(item["route"], str)
            or not item["route"].startswith("/")
            or "?" in item["route"]
            or type(item["status"]) is not int
            or not isinstance(item["request_id"], str)
            or not item["request_id"]
            for item in requests
        ):
            return _decision("request_mismatch")
        request_ids = tuple(item.get("request_id") for item in requests if isinstance(item, Mapping))
        if request_ids != invocation.request_ids or not _request_requirement(case_id, requests):
            return _decision("request_mismatch")
        combined_requests.extend(requests)
    if not _typed_equal(case["request_transcript"], combined_requests):
        return _decision("request_mismatch")
    if case_id == "installed_service_runtime":
        return _decision("runner_owned_service_runtime", pending=True)
    return AdmissionDecision("passed", "accepted")


def validate_manifest(path):
    """Validate the inert reviewed registry and every hash-bound source member."""
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON member")
            result[key] = value
        return result

    raw = Path(path).read_bytes()
    value = json.loads(raw.decode("utf-8", "strict"), object_pairs_hook=unique)
    required = {
        "schema_version", "adapter_id", "revision", "required_cases", "actors",
        "cases", "raw_schemas", "phases", "validator",
    }
    if not isinstance(value, dict) or set(value) != required:
        raise ValueError("Swift matrix contract shape is invalid")
    if value["schema_version"] != 1 or value["adapter_id"] != ADAPTER_ID or value["revision"] != CONTRACT_REVISION:
        raise ValueError("Swift matrix contract identity is invalid")
    if value["required_cases"] != list(REQUIRED_CASES) or [item.get("id") for item in value["cases"]] != list(REQUIRED_CASES):
        raise ValueError("Swift matrix cases are incomplete or reordered")
    if any(item.get("actor_ids") != list(ACTOR_IDS) for item in value["cases"]):
        raise ValueError("Swift matrix case lacks a transport actor")
    if [item.get("id") for item in value["actors"]] != list(ACTOR_IDS):
        raise ValueError("Swift matrix actors are incomplete or reordered")
    required_case_keys = {"id", "actor_ids", "evidence_kind", "operations", "raw_schema_ids"}
    if any(not isinstance(item, dict) or set(item) != required_case_keys for item in value["cases"]):
        raise ValueError("Swift matrix case contract shape is invalid")
    required_phases = [
        (actor_id, phase_id, ordinal)
        for actor_id in ACTOR_IDS
        for ordinal, phase_id in enumerate(("bootstrap_pair", "revoke_and_pair", "restart", "outage_stop", "outage_start", "final_verify"), 1)
    ]
    actual_phases = [(item.get("actor_id"), item.get("phase_id"), item.get("ordinal")) for item in value["phases"]]
    if actual_phases != required_phases or any(item.get("recipe_id") != "swift-" + item.get("phase_id", "") for item in value["phases"]):
        raise ValueError("Swift matrix phases are incomplete or reordered")
    bindings = [item.get("schema") for item in value["raw_schemas"]] + [value["validator"]]
    for binding in bindings:
        if not isinstance(binding, dict) or set(binding) != {"path", "sha256"}:
            raise ValueError("Swift matrix source binding is invalid")
        member = Path(binding["path"])
        if not member.is_absolute() or HEX64.fullmatch(str(binding["sha256"])) is None:
            raise ValueError("Swift matrix source identity is invalid")
        if hashlib.sha256(member.read_bytes()).hexdigest() != binding["sha256"]:
            raise ValueError("Swift matrix source binding changed: " + str(member))
    return MappingProxyType(value)
