#!/usr/bin/env python3
"""Pure semantic admission for the Edge v2 installed matrix lane.

The common matrix runner owns filesystem, process, broker and cleanup facts.
This module only consumes the immutable views produced by that runner.  It
deliberately has no imports that can open files, start processes, or perform
network operations.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from types import MappingProxyType
from typing import Literal
import re


ADAPTER_ID = "edge_v2"
CONTRACT_REVISION = 1
PRODUCT_VERSION = "2026.36.2"
EDGE_PROFILE_ID = "edge-delivery-v2"
EDGE_PROFILE_REVISION = "2.0.0"
EDGE_PROFILE_SHA256 = "e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224"
VIN = "5YJ3E1EA7KF000001"
VEHICLE_ID = "11111111-1111-4111-8111-111111111111"
INSTALLATION_ID = "task9-edge"
LINEAGE = "spool-2026-09-05"
HEX64 = re.compile(r"^[0-9a-f]{64}$")
UUID4 = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")

ACTOR_IDS = (
    "installed_controller",
    "edge_normal",
    "edge_producer",
    "edge_reference",
    "edge_fault",
    "edge_transport",
)

REQUIRED_CASES = (
    "candidate_artifact_identity",
    "installed_service_runtime",
    "edge_linux_runtime",
    "edge_mtls_bearer_identity",
    "edge_no_v1_downgrade",
    "edge_source_owner_exclusivity",
    "edge_uninterrupted_delivery_parity",
    "edge_restart_delivery_parity",
    "edge_duplicate_reenqueue_dedup",
    "edge_gap_interleaving_later_ack_rejected",
    "edge_unsupported_event_bounded_disposition",
    "edge_conflicting_identity_blocks_ack",
    "edge_transaction_fault_redelivery",
    "edge_lost_ack_body_redelivery",
    "edge_pending_publication_offline_recovery",
    "edge_clean_shutdown_resume",
)

RAW_SCHEMA_IDS = (
    "edge_fixture_v1",
    "edge_checkpoint_v1",
    "edge_http_v1",
    "edge_fault_v1",
    "edge_snapshot_v1",
    "edge_cleanup_v1",
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
    """Runner-created immutable views; no callbacks or authority are present."""

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


ACTOR_CONTRACT = MappingProxyType(
    {
        "installed_controller": {
            "kind": "installed_controller",
            "execution": "coordinator",
            "runtime_ref": "root_controller",
            "entrypoint_ref": "installed_controller",
            "artifact_roles": ("hub_executable",),
            "source_roles": ("hub_source", "protocol_source"),
        },
        "edge_normal": {
            "kind": "edge_normal",
            "execution": "coordinator",
            "runtime_ref": "root_python",
            "entrypoint_ref": "edge_matrix",
            "artifact_roles": (),
            "source_roles": ("hub_source", "protocol_source"),
        },
        "edge_producer": {
            "kind": "edge_normal",
            "execution": "docker_worker",
            "runtime_ref": "edge_linux",
            "entrypoint_ref": "edge_producer",
            "artifact_roles": ("edge_executable",),
            "source_roles": ("edge_source", "protocol_source"),
        },
        "edge_reference": {
            "kind": "edge_normal",
            "execution": "local_worker",
            "runtime_ref": "hub_target_aux",
            "entrypoint_ref": "edge_reference",
            "artifact_roles": ("hub_executable",),
            "source_roles": ("hub_source", "protocol_source"),
        },
        "edge_fault": {
            "kind": "edge_fault",
            "execution": "local_worker",
            "runtime_ref": "hub_target_aux",
            "entrypoint_ref": "edge_fault",
            "artifact_roles": (),
            "source_roles": ("hub_source", "protocol_source"),
        },
        "edge_transport": {
            "kind": "edge_fault",
            "execution": "docker_worker",
            "runtime_ref": "edge_linux",
            "entrypoint_ref": "edge_negative_transport",
            "artifact_roles": (),
            "source_roles": ("hub_source", "protocol_source"),
        },
    }
)


CASE_BINDINGS = MappingProxyType(
    {
        "candidate_artifact_identity": (("installed_controller", "edge_producer"), ("observe_identity",), "identity", ("edge_fixture_v1",)),
        "installed_service_runtime": (("installed_controller",), ("observe_identity",), "identity", ("edge_fixture_v1",)),
        "edge_linux_runtime": (("edge_producer",), ("observe_runtime",), "identity", ("edge_fixture_v1",)),
        "edge_mtls_bearer_identity": (("edge_producer", "edge_normal", "edge_transport"), ("deliver_positive", "negative_auth"), "http", ("edge_http_v1",)),
        "edge_no_v1_downgrade": (("edge_reference", "edge_transport"), ("negative_v1_404", "negative_v1_body", "negative_redirect"), "http", ("edge_http_v1",)),
        "edge_source_owner_exclusivity": (("installed_controller", "edge_reference"), ("observe_owner",), "identity", ("edge_fixture_v1", "edge_cleanup_v1")),
        "edge_uninterrupted_delivery_parity": (("edge_reference", "edge_producer", "edge_normal"), ("reference_delivery",), "http", ("edge_checkpoint_v1", "edge_snapshot_v1")),
        "edge_restart_delivery_parity": (("installed_controller", "edge_producer", "edge_normal"), ("restart_delivery",), "http", ("edge_checkpoint_v1", "edge_snapshot_v1")),
        "edge_duplicate_reenqueue_dedup": (("edge_producer", "edge_normal"), ("duplicate_reenqueue",), "http", ("edge_checkpoint_v1", "edge_snapshot_v1")),
        "edge_gap_interleaving_later_ack_rejected": (("edge_producer", "edge_normal"), ("gap_prepare", "gap_reject", "gap_consume"), "http", ("edge_checkpoint_v1", "edge_http_v1", "edge_snapshot_v1")),
        "edge_unsupported_event_bounded_disposition": (("edge_producer", "edge_normal"), ("unsupported_event",), "http", ("edge_checkpoint_v1", "edge_snapshot_v1")),
        "edge_conflicting_identity_blocks_ack": (("edge_reference", "edge_transport"), ("conflicting_identity",), "http", ("edge_http_v1", "edge_snapshot_v1")),
        "edge_transaction_fault_redelivery": (("edge_fault", "edge_producer"), ("fault_raw_insert", "fault_lifecycle_write", "fault_receipt_insert", "fault_frontier_update", "fault_commit", "fault_before_commit", "fault_after_commit"), "http", ("edge_fault_v1", "edge_checkpoint_v1", "edge_snapshot_v1")),
        "edge_lost_ack_body_redelivery": (("edge_fault", "edge_producer"), ("fault_lost_ack",), "http", ("edge_fault_v1", "edge_checkpoint_v1")),
        "edge_pending_publication_offline_recovery": (("edge_fault", "edge_producer"), ("fault_offline_recovery",), "http", ("edge_fault_v1", "edge_snapshot_v1", "edge_cleanup_v1")),
        "edge_clean_shutdown_resume": (("installed_controller", "edge_producer", "edge_normal"), ("resume_stop", "resume_start", "actors_closed"), "identity", ("edge_cleanup_v1", "edge_checkpoint_v1")),
    }
)


PHASE_SCHEDULE = (
    ("edge_normal", "initial", 1, "edge-initial", ("verify",)),
    ("edge_producer", "primary_1", 2, "edge-primary_1", ("stop", "start", "verify", "stop")),
    ("edge_reference", "reference_1", 3, "edge-reference_1", ("start", "verify")),
    ("edge_producer", "primary_2", 4, "edge-primary_2", ("stop", "start", "verify", "stop")),
    ("edge_reference", "reference_2", 5, "edge-reference_2", ("verify",)),
    ("edge_producer", "primary_3", 6, "edge-primary_3", ("stop", "start", "verify", "stop")),
    ("edge_reference", "reference_3", 7, "edge-reference_3", ("verify",)),
    ("edge_transport", "negative_auth", 8, "edge-negative_auth", ("verify",)),
    ("edge_transport", "negative_v1_404", 9, "edge-negative_v1_404", ("verify",)),
    ("edge_transport", "negative_v1_body", 10, "edge-negative_v1_body", ("verify",)),
    ("edge_transport", "negative_redirect", 11, "edge-negative_redirect", ("verify",)),
    ("edge_transport", "negative_conflict", 12, "edge-negative_conflict", ("verify",)),
    ("edge_fault", "fault_raw_insert", 13, "edge-fault_raw_insert", ("verify",)),
    ("edge_fault", "fault_lifecycle_write", 14, "edge-fault_lifecycle_write", ("verify",)),
    ("edge_fault", "fault_receipt_insert", 15, "edge-fault_receipt_insert", ("verify",)),
    ("edge_fault", "fault_frontier_update", 16, "edge-fault_frontier_update", ("verify",)),
    ("edge_fault", "fault_commit", 17, "edge-fault_commit", ("verify",)),
    ("edge_fault", "fault_before_commit", 18, "edge-fault_before_commit", ("verify",)),
    ("edge_fault", "fault_after_commit", 19, "edge-fault_after_commit", ("verify",)),
    ("edge_fault", "fault_lost_ack", 20, "edge-fault_lost_ack", ("verify",)),
    ("edge_fault", "fault_offline_recovery", 21, "edge-fault_offline_recovery", ("verify",)),
    ("edge_producer", "primary_6", 22, "edge-primary_6", ("stop", "start", "verify", "stop")),
    ("edge_reference", "reference_6", 23, "edge-reference_6", ("verify",)),
    ("edge_producer", "primary_8", 24, "edge-primary_8", ("stop", "start", "verify", "stop")),
    ("edge_reference", "reference_8", 25, "edge-reference_8", ("verify",)),
    ("edge_producer", "gap_prepare", 26, "edge-gap_prepare", ("stop",)),
    ("edge_normal", "gap_reject", 27, "edge-gap_reject", ()),
    ("edge_producer", "gap_consume", 28, "edge-gap_consume", ("start", "verify")),
    ("edge_normal", "resume_stop", 29, "edge-resume_stop", ("stop",)),
    ("edge_normal", "resume_start", 30, "edge-resume_start", ("start", "verify")),
    ("edge_normal", "actors_closed", 31, "edge-actors_closed", ("verify",)),
)


STATIC_FACTS = MappingProxyType(
    {
        # These are the exact normalized v1 assertion fields consumed by the
        # shared matrix. Specialized raw witnesses carry the extra Edge
        # details; the normalized receipt stays closed and small.
        "edge_mtls_bearer_identity": {"mtls_verified": True, "bearer_identity_verified": True},
        "edge_no_v1_downgrade": {"v2_attempted": True, "v1_requests": 0},
        "edge_source_owner_exclusivity": {"selected_source": "fleet", "active_ingestion_authorities": 1},
        "edge_uninterrupted_delivery_parity": {"public_current_equal": True, "history_equal": True},
        "edge_restart_delivery_parity": {"public_current_equal": True, "history_equal": True, "restart_observed": True},
        "edge_duplicate_reenqueue_dedup": {"duplicate_reenqueued": True, "duplicate_rows_added": 0},
        "edge_gap_interleaving_later_ack_rejected": {"later_ack_rejected": True, "contiguous_prefix_preserved": True},
        "edge_unsupported_event_bounded_disposition": {"unsupported_event_disposition": "projection_unsupported", "bounded": True},
        "edge_conflicting_identity_blocks_ack": {"conflict_observed": True, "ack_blocked": True},
        "edge_transaction_fault_redelivery": {"fault_injected": True, "ack_absent": True, "redelivered": True},
        "edge_lost_ack_body_redelivery": {"ack_committed": True, "body_lost": True, "redelivered_without_duplicate": True},
        "edge_pending_publication_offline_recovery": {"offline_commit_durable": True, "publication_recovered": True},
        "edge_clean_shutdown_resume": {"clean_shutdown": True, "resume_contiguous": True},
    }
)



def _decision(status: Literal["passed", "failed", "pending"], code: str) -> AdmissionDecision:
    if code not in DECISION_CODES:
        raise RuntimeError("unreviewed admission decision code")
    return AdmissionDecision(status, code)


def _value(value: object, key: str) -> object:
    if isinstance(value, Mapping):
        return value.get(key)
    return getattr(value, key, None)


def _typed_equal(left: object, right: object) -> bool:
    if type(left) is not type(right):
        return False
    if isinstance(left, Mapping):
        return set(left) == set(right) and all(_typed_equal(left[key], right[key]) for key in left)
    if isinstance(left, (tuple, list)):
        return len(left) == len(right) and all(_typed_equal(a, b) for a, b in zip(left, right, strict=True))
    return left == right


def _valid_digest(value: object) -> bool:
    return isinstance(value, str) and HEX64.fullmatch(value) is not None and value != "0" * 64


def _valid_uuid(value: object) -> bool:
    return isinstance(value, str) and UUID4.fullmatch(value) is not None


def _validate_context(context: object) -> str | None:
    if not isinstance(context, AdmissionContext):
        return "wrong_context"
    if context.adapter_id != ADAPTER_ID or not re.fullmatch(
        r"edge_v2__(macos_arm64|debian13_amd64|debian13_arm64)", context.cell_id
    ) or not _valid_uuid(context.session_id):
        return "wrong_context"
    header = context.header
    if not isinstance(header, Mapping) or header.get("product_version") != PRODUCT_VERSION or header.get("profile_id") != "hub-http-v1" or header.get("profile_revision") != "1.0.0":
        return "wrong_context"
    if not _valid_digest(header.get("profile_sha256")):
        return "wrong_context"
    if tuple(context.actors) != ACTOR_IDS or set(context.actors) != set(ACTOR_IDS):
        return "wrong_actor"
    for actor_id in ACTOR_IDS:
        actor = context.actors.get(actor_id)
        expected = ACTOR_CONTRACT[actor_id]
        if actor is None:
            return "wrong_actor"
        for key in ("kind", "runtime_ref", "entrypoint_ref"):
            if _value(actor, key) != expected[key]:
                return "wrong_actor"
        if tuple(_value(actor, "artifact_roles") or ()) != expected["artifact_roles"]:
            return "wrong_artifact_role"
        if tuple(_value(actor, "source_roles") or ()) != expected["source_roles"]:
            return "wrong_source_role"
        manifest = _value(actor, "installed_manifest")
        if not isinstance(manifest, Mapping) or not manifest:
            return "installed_manifest_mismatch"
    if not isinstance(context.invocations, tuple) or not isinstance(context.raw, Mapping) or not isinstance(context.controller_observations, Mapping):
        return "wrong_context"
    return None


def _expected_facts(case_id: str, context: AdmissionContext) -> Mapping[str, object] | None:
    if case_id == "candidate_artifact_identity":
        actor = context.actors.get("edge_producer")
        runtime = _value(actor, "runtime")
        artifact = runtime.get("artifact") if isinstance(runtime, Mapping) else None
        if not isinstance(artifact, Mapping) or not _valid_digest(artifact.get("sha256")) or artifact.get("version") != PRODUCT_VERSION or artifact.get("storage_format") != 3:
            return None
        return {"edge_sha256": artifact["sha256"], "product_version": PRODUCT_VERSION, "storage_format": 3, "normal_fault_distinct": artifact.get("normal_fault_distinct") is True}
    if case_id == "installed_service_runtime":
        runtime = context.header.get("runtime")
        hub = runtime.get("hub") if isinstance(runtime, Mapping) else None
        mode = hub.get("service_mode") if isinstance(hub, Mapping) else None
        return {"service_mode": mode} if isinstance(mode, str) and mode else None
    if case_id == "edge_linux_runtime":
        runtime = _value(context.actors.get("edge_producer"), "runtime")
        if not isinstance(runtime, Mapping) or runtime.get("process_bound") is not True or runtime.get("namespace_bound") is not True or not isinstance(runtime.get("architecture"), str):
            return None
        return {"process_bound": True, "namespace_bound": True, "architecture": runtime["architecture"]}
    return STATIC_FACTS.get(case_id)


def _validate_requests(operation: str, requests: object) -> bool:
    if not isinstance(requests, list) or len(requests) > 128:
        return False
    ids: set[str] = set()
    for request in requests:
        if not isinstance(request, Mapping) or set(request) != {"request_id", "method", "route", "status"}:
            return False
        if not isinstance(request["request_id"], str) or not request["request_id"] or request["request_id"] in ids:
            return False
        ids.add(request["request_id"])
        if request["method"] not in {"GET", "POST"} or not isinstance(request["route"], str) or not request["route"].startswith("/") or type(request["status"]) is not int or not 100 <= request["status"] <= 599:
            return False
    if operation.startswith("negative_v1") and any("/v1/" in item["route"] for item in requests):
        return False
    return True


_SPECIALIZED_PAYLOADS = {
    "edge_fixture_v1": frozenset({"seal_sha256", "http_profile_sha256", "edge_profile_sha256", "primary_context_sha256", "baseline_clone_equal", "custody_manifest_sha256", "root_runtime_observation", "root_transport_observation"}),
    "edge_checkpoint_v1": frozenset({"lane", "frontier", "applications", "sequences", "pending_publications", "batch", "dispositions", "snapshot", "public_current", "producer_generation_sha256", "hub_generation_sha256", "ack_census"}),
    "edge_http_v1": frozenset({"role", "transcript", "tls", "transport_failures", "census"}),
    "edge_fault_v1": frozenset({"point", "witness", "witness_text", "process", "before", "after", "delivery_before", "delivery_after", "publication_witness"}),
    "edge_snapshot_v1": frozenset({"lane", "store_id", "store_schema_version", "selector_sha256", "current", "retained_observations", "lifecycle", "accumulator_jcs", "applications", "dispositions", "materialised_states", "open_rows", "sync_mutations", "counts"}),
    "edge_cleanup_v1": frozenset({"resource_id", "acquired", "stop", "current_absence", "retention", "status", "errors"}),
}


def _specialized_schema(payload: Mapping[str, object]) -> str | None:
    keys = frozenset(payload)
    for schema_id, expected in _SPECIALIZED_PAYLOADS.items():
        if keys == expected:
            return schema_id
    return None


def _valid_binding(value: object) -> bool:
    return isinstance(value, Mapping) and set(value) == {"path", "sha256"} and isinstance(value.get("path"), str) and value["path"].startswith("/") and _valid_digest(value.get("sha256"))


def _validate_specialized_payload(payload: Mapping[str, object], schema_id: str, operation: str) -> str | None:
    """Validate closed raw payload shape without opening its bindings."""
    if schema_id == "edge_fixture_v1":
        if payload.get("baseline_clone_equal") is not True or any(not _valid_digest(payload.get(key)) for key in ("seal_sha256", "http_profile_sha256", "edge_profile_sha256", "primary_context_sha256", "custody_manifest_sha256")) or not _valid_binding(payload.get("root_runtime_observation")) or not _valid_binding(payload.get("root_transport_observation")):
            return "raw_fact_mismatch"
    elif schema_id == "edge_checkpoint_v1":
        if payload.get("lane") not in {"primary", "reference", "fault", "negative"} or type(payload.get("applications")) is not int or type(payload.get("sequences")) is not int or type(payload.get("pending_publications")) is not int or any(not _valid_binding(payload.get(key)) for key in ("batch", "dispositions", "snapshot", "ack_census")) or (payload.get("public_current") is not None and not _valid_binding(payload.get("public_current"))) or any(not _valid_digest(payload.get(key)) for key in ("producer_generation_sha256", "hub_generation_sha256")):
            return "raw_fact_mismatch"
    elif schema_id == "edge_http_v1":
        if payload.get("role") not in {"hub_public", "edge_delivery", "negative_responder"} or not isinstance(payload.get("transcript"), list) or len(payload["transcript"]) > 128:
            return "raw_fact_mismatch"
        requests = []
        for item in payload["transcript"]:
            if not isinstance(item, Mapping) or set(item) != {"request_id", "method", "route", "status", "request_body_sha256", "response_body"} or not _valid_binding(item.get("response_body")):
                return "raw_fact_mismatch"
            requests.append({key: item[key] for key in ("request_id", "method", "route", "status")})
        tls = payload.get("tls")
        if not isinstance(tls, Mapping) or set(tls) != {"server_der_sha256", "client_der_sha256", "chain_verified", "hostname_verified"} or not _valid_digest(tls.get("server_der_sha256")) or (tls.get("client_der_sha256") is not None and not _valid_digest(tls.get("client_der_sha256"))):
            return "raw_fact_mismatch"
        if not _validate_requests(operation, requests):
            return "request_mismatch"
        failures = payload.get("transport_failures")
        census = payload.get("census")
        if not isinstance(failures, list) or len(failures) > 32 or not isinstance(census, Mapping) or set(census) != {"v2_gets", "v2_acks", "v1_requests", "redirect_requests"} or any(type(census[key]) is not int or census[key] < 0 for key in census):
            return "raw_fact_mismatch"
        if operation.startswith("negative_v1") and census["v1_requests"] != 0:
            return "request_mismatch"
    elif schema_id == "edge_fault_v1":
        if payload.get("point") not in {"raw_insert", "lifecycle_write", "receipt_insert", "frontier_update", "commit", "before_commit", "after_commit", "after_ack_accepted_before_response"} or not isinstance(payload.get("witness_text"), str) or not 0 < len(payload["witness_text"]) <= 128 or any(not _valid_binding(payload.get(key)) for key in ("witness", "process", "before", "after", "delivery_before", "delivery_after")) or (payload.get("publication_witness") is not None and not _valid_binding(payload.get("publication_witness"))):
            return "raw_fact_mismatch"
    elif schema_id == "edge_snapshot_v1":
        if payload.get("lane") not in {"primary", "reference", "fault", "negative"} or not _valid_uuid(payload.get("store_id")) or payload.get("store_schema_version") != 59 or not _valid_digest(payload.get("selector_sha256")) or not isinstance(payload.get("accumulator_jcs"), str) or len(payload["accumulator_jcs"].encode()) > 1_048_576:
            return "raw_fact_mismatch"
        for key in ("current", "retained_observations", "applications", "dispositions", "materialised_states", "open_rows", "sync_mutations"):
            if not isinstance(payload.get(key), list) or len(payload[key]) > 256:
                return "raw_fact_mismatch"
        counts = payload.get("counts")
        if not isinstance(counts, Mapping) or set(counts) != {"applications", "sequences", "pending_publications", "ack_frontier"} or any(type(counts[key]) is not int or counts[key] < 0 for key in ("applications", "sequences", "pending_publications")) or (counts["ack_frontier"] is not None and (type(counts["ack_frontier"]) is not int or counts["ack_frontier"] < 0)):
            return "raw_fact_mismatch"
    elif schema_id == "edge_cleanup_v1":
        if not isinstance(payload.get("resource_id"), str) or not payload["resource_id"] or any(not _valid_binding(payload.get(key)) for key in ("acquired", "stop", "current_absence", "retention")) or payload.get("status") != "clean" or payload.get("errors") not in ([], ()):
            return "cleanup_failure"
    return None


def _transcript_payload(payload: Mapping[str, object]) -> list[Mapping[str, object]]:
    if set(payload) == {"case_id", "facts", "requests", "cleanup"}:
        return list(payload["requests"]) if isinstance(payload.get("requests"), list) else []
    if _specialized_schema(payload) == "edge_http_v1":
        result = []
        for item in payload["transcript"]:
            result.append({key: item[key] for key in ("request_id", "method", "route", "status")})
        return result
    return []


def _validate_raw(raw: Mapping[str, object], context: AdmissionContext, invocation: object, case_id: str) -> str | None:
    if set(raw) != {"schema_version", "session_id", "cell_id", "instance_nonce", "session_input_sha256", "actor_id", "invocation_id", "stage", "observation", "payload"}:
        return "raw_identity_mismatch"
    if raw.get("schema_version") != 1 or raw.get("session_id") != context.session_id or raw.get("cell_id") != context.cell_id or not _valid_digest(raw.get("instance_nonce")) or not _valid_digest(raw.get("session_input_sha256")) or raw.get("actor_id") != _value(invocation, "actor_id") or raw.get("invocation_id") != _value(invocation, "id"):
        return "raw_identity_mismatch"
    observation = raw.get("observation")
    if not isinstance(observation, Mapping) or set(observation) != {"session_sequence", "proof_sha256"} or type(observation.get("session_sequence")) is not int or observation["session_sequence"] <= 0 or not _valid_digest(observation.get("proof_sha256")):
        return "raw_identity_mismatch"
    if observation["session_sequence"] != _value(invocation, "session_sequence_after"):
        return "sequence_mismatch"
    payload = raw.get("payload")
    if not isinstance(payload, Mapping):
        return "raw_fact_mismatch"
    if set(payload) == {"case_id", "facts", "requests", "cleanup"}:
        if payload.get("case_id") != case_id or not _validate_requests(_value(invocation, "operation"), payload.get("requests")):
            return "request_mismatch"
        cleanup = payload.get("cleanup")
        if not isinstance(cleanup, Mapping) or cleanup.get("status") != "clean" or cleanup.get("errors") not in ([], ()):
            return "cleanup_failure"
        expected = _expected_facts(case_id, context)
        if expected is None or not _typed_equal(payload.get("facts"), expected):
            return "raw_fact_mismatch"
        return None
    schema_id = _specialized_schema(payload)
    if schema_id is None:
        return "raw_fact_mismatch"
    if schema_id == "edge_fault_v1" and _value(invocation, "actor_id") != "edge_fault":
        return "wrong_actor"
    return _validate_specialized_payload(payload, schema_id, _value(invocation, "operation"))


def admit_case(case: dict, context: AdmissionContext) -> AdmissionDecision:
    """Admit one Edge case from runner-owned immutable evidence."""
    if not isinstance(case, dict) or set(case) != {"id", "status", "expected", "actual", "evidence_kind", "request_transcript"}:
        return _decision("failed", "case_shape")
    case_id = case.get("id")
    if case_id not in REQUIRED_CASES:
        return _decision("failed", "unknown_case")
    context_problem = _validate_context(context)
    if context_problem is not None:
        return _decision("failed", context_problem)
    if case_id == "installed_service_runtime":
        if case["status"] not in {"pending", "passed"} or case["evidence_kind"] != "identity":
            return _decision("failed", "case_shape")
        expected = _expected_facts(case_id, context)
        if expected is None or not _typed_equal(case["expected"], expected) or not _typed_equal(case["actual"], expected):
            return _decision("failed", "literal_mismatch")
        return _decision("pending", "runner_owned_service_runtime")
    binding = CASE_BINDINGS[case_id]
    actor_ids, operations, evidence_kind, _schema_ids = binding
    expected = _expected_facts(case_id, context)
    if expected is None or case["status"] != "passed" or case["evidence_kind"] != evidence_kind or not _typed_equal(case["expected"], expected) or not _typed_equal(case["actual"], expected):
        return _decision("failed", "literal_mismatch" if expected is not None else "wrong_context")
    invocations = [item for item in context.invocations if _value(item, "case_id") == case_id]
    if len(invocations) != len(operations):
        return _decision("failed", "invocation_missing")
    by_operation = {_value(item, "operation"): item for item in invocations}
    if tuple(by_operation) != operations:
        return _decision("failed", "operation_mismatch")
    transcript: list[object] = []
    for operation in operations:
        invocation = by_operation[operation]
        if _value(invocation, "actor_id") not in actor_ids:
            return _decision("failed", "wrong_actor")
        before = _value(invocation, "session_sequence_before")
        after = _value(invocation, "session_sequence_after")
        if type(before) is not int or type(after) is not int or before <= 0 or after < before or before not in context.controller_observations or after not in context.controller_observations:
            return _decision("failed", "sequence_mismatch")
        raw = context.raw.get(_value(invocation, "evidence_id"))
        if not isinstance(raw, Mapping):
            return _decision("failed", "raw_missing")
        problem = _validate_raw(raw, context, invocation, case_id)
        if problem is not None:
            return _decision("failed", problem)
        payload = raw["payload"]
        transcript.extend(_transcript_payload(payload))
    if not _typed_equal(case["request_transcript"], transcript):
        return _decision("failed", "request_mismatch")
    return _decision("passed", "accepted")


def _actor(actor_id: str, *, runtime: Mapping[str, object] | None = None) -> AdmittedActor:
    spec = ACTOR_CONTRACT[actor_id]
    return AdmittedActor(actor_id, spec["kind"], spec["runtime_ref"], spec["entrypoint_ref"], tuple(spec["artifact_roles"]), tuple(spec["source_roles"]), MappingProxyType({"sha256": "a" * 64}), MappingProxyType(dict(runtime or {})))


def expected_normalized_facts(case_id: str, context: AdmissionContext) -> Mapping[str, object] | None:
    """Return the fixed facts for a runner-created immutable context."""
    return _expected_facts(case_id, context)


def case_operations(case_id: str) -> tuple[str, ...]:
    return tuple(CASE_BINDINGS[case_id][1])


def case_evidence_kind(case_id: str) -> str:
    return str(CASE_BINDINGS[case_id][2])

def synthetic_context_for_test(case_id: str, *, service_mode: str = "installed-deb-systemd") -> AdmissionContext:
    """Build a complete in-memory context for focused contract tests only."""
    runtime = {"artifact": {"sha256": "b" * 64, "version": PRODUCT_VERSION, "storage_format": 3, "normal_fault_distinct": True}, "process_bound": True, "namespace_bound": True, "architecture": "amd64"}
    actors = {actor_id: _actor(actor_id, runtime=runtime if actor_id == "edge_producer" else {}) for actor_id in ACTOR_IDS}
    header = {"product_version": PRODUCT_VERSION, "profile_id": "hub-http-v1", "profile_revision": "1.0.0", "profile_sha256": "c" * 64, "runtime": {"hub": {"service_mode": service_mode}}}
    operations = CASE_BINDINGS[case_id][1]
    invocations = []
    raw = {}
    for index, operation in enumerate(operations, 1):
        actor_id = CASE_BINDINGS[case_id][0][min(index - 1, len(CASE_BINDINGS[case_id][0]) - 1)]
        invocation_id = f"invocation-{index}"
        evidence_id = f"evidence-{index}"
        invocations.append(AdmittedInvocation(invocation_id, case_id, actor_id, operation, index, index + 1, evidence_id, ()))
        raw[evidence_id] = {"schema_version": 1, "session_id": "11111111-1111-4111-8111-111111111111", "cell_id": "edge_v2__debian13_amd64", "instance_nonce": "d" * 64, "session_input_sha256": "e" * 64, "actor_id": actor_id, "invocation_id": invocation_id, "stage": operation, "observation": {"session_sequence": index + 1, "proof_sha256": "f" * 64}, "payload": {"case_id": case_id, "facts": dict(_expected_facts(case_id, AdmissionContext("edge_v2", "edge_v2__debian13_amd64", "11111111-1111-4111-8111-111111111111", header, {}, actors, tuple(invocations), raw, {})) or {}), "requests": [], "cleanup": {"status": "clean", "errors": []}}}
    observations = {index: {"status": "observed"} for index in range(1, len(operations) + 2)}
    return AdmissionContext("edge_v2", "edge_v2__debian13_amd64", "11111111-1111-4111-8111-111111111111", header, {}, actors, tuple(invocations), raw, observations)


def synthetic_case(case_id: str, context: AdmissionContext) -> dict:
    expected = _expected_facts(case_id, context) or {}
    return {"id": case_id, "status": "pending" if case_id == "installed_service_runtime" else "passed", "expected": dict(expected), "actual": dict(expected), "evidence_kind": CASE_BINDINGS[case_id][2], "request_transcript": []}
