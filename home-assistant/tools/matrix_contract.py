"""Pure semantic admission for the installed Home Assistant matrix adapter."""

from __future__ import annotations

import re
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from types import MappingProxyType
from typing import Literal

ADAPTER_ID = "home_assistant"
CONTRACT_REVISION = 1
PRODUCT_VERSION = "2026.36.2"
HOME_ASSISTANT_VERSION = "2026.8.3"
PYTHON_VERSION = "3.14.2"
UNKNOWN_VEHICLE = "33333333-3333-4333-8333-333333333333"
UNICODE_NAME = "Interop \N{EN DASH} Árvíztűrő 🚗"
ARCHIVE_ROLE = "home_assistant_integration_archive"
SOURCE_ROLE = "home_assistant_source"
HEX64 = re.compile(r"^[0-9a-f]{64}$")
REQUIRED_LOADED_MODULES = {
    "custom_components.teslatlas_hub": "__init__.py",
    "custom_components.teslatlas_hub.client": "client.py",
    "custom_components.teslatlas_hub.config_flow": "config_flow.py",
    "custom_components.teslatlas_hub.const": "const.py",
    "custom_components.teslatlas_hub.coordinator": "coordinator.py",
    "custom_components.teslatlas_hub.current_hub_client": "current_hub_client.py",
    "custom_components.teslatlas_hub.entity": "entity.py",
    "custom_components.teslatlas_hub.models": "models.py",
    "custom_components.teslatlas_hub.sensor": "sensor.py",
}

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
    }
)


@dataclass(frozen=True, slots=True)
class AdmittedActor:
    """Runner-created immutable actor view consumed by this predicate."""

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
    """Runner-created immutable invocation view consumed by this predicate."""

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
    """Read-only semantic inputs supplied after common admission checks."""

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
    """Closed result returned to the common runner."""

    status: Literal["passed", "failed", "pending"]
    code: str


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
    "credential_loss_reauthentication",
    "installed_home_assistant_runtime",
    "polling_transport_zero_sse",
)

CASE_BINDINGS = MappingProxyType(
    {
        "candidate_artifact_identity": ("ha_flow", ("observe_installed_runtime",)),
        "installed_service_runtime": ("ha_flow", ("observe_installed_runtime",)),
        "discovery_identity_profile": ("ha_client", ("probe",)),
        "unauthenticated_discovery": ("ha_client", ("probe",)),
        "bad_invitation": ("ha_client", ("bad_pair",)),
        "expired_invitation": ("ha_client", ("expired_pair",)),
        "replayed_invitation": ("ha_client", ("replay_pair",)),
        "real_auth": ("ha_flow", ("config_flow_setup",)),
        "credential_lifecycle_reauth": (
            "ha_flow",
            ("revocation_refresh", "reauthentication_flow"),
        ),
        "revocation": ("ha_flow", ("revocation_refresh",)),
        "unknown_vehicle": ("ha_client", ("unknown_current",)),
        "exact_current_values": ("ha_flow", ("initial_poll", "later_poll")),
        "endpoint_restart": ("ha_flow", ("endpoint_restart_poll",)),
        "outage_recovery": ("ha_flow", ("outage_poll",)),
        "unsupported_operation_zero_requests": (
            "ha_client",
            ("unsupported_surface",),
        ),
        "credential_loss_reauthentication": (
            "ha_flow",
            ("revocation_refresh", "reauthentication_flow"),
        ),
        "installed_home_assistant_runtime": (
            "ha_flow",
            ("observe_installed_runtime",),
        ),
        "polling_transport_zero_sse": (
            "ha_flow",
            (
                "config_flow_setup",
                "initial_poll",
                "later_poll",
                "reauthentication_flow",
                "endpoint_restart_poll",
                "outage_poll",
                "unload",
            ),
        ),
    }
)

STATIC_FACTS = MappingProxyType(
    {
        "unauthenticated_discovery": {
            "discovery_status": 200,
            "authorization_headers": 0,
            "credential_absent": True,
        },
        "bad_invitation": {
            "claim_status": 401,
            "typed_error": "HubPairingError",
            "credential_created": False,
        },
        "expired_invitation": {
            "claim_status": 401,
            "typed_error": "HubPairingError",
            "credential_created": False,
        },
        "replayed_invitation": {
            "claim_status": 401,
            "typed_error": "HubPairingError",
            "credential_created": False,
        },
        "real_auth": {
            "claim_status": 200,
            "vehicles": 2,
            "config_entry_loaded": True,
        },
        "credential_lifecycle_reauth": {
            "fresh_claim_status": 200,
            "entry_id_preserved": True,
            "registry_ids_preserved": True,
            "later_poll_status": 200,
        },
        "revocation": {
            "old_credential_status": 401,
            "typed_error": "HubAuthenticationError",
            "reauth_started": True,
        },
        "unknown_vehicle": {
            "http_status": 404,
            "vehicle_id": UNKNOWN_VEHICLE,
            "state_of_charge": None,
            "inside_temperature_c": None,
        },
        "exact_current_values": {
            "vehicles": 2,
            "sensors_per_vehicle": 13,
            "initial_battery": 0,
            "initial_inside_temperature_c": 21.5,
            "later_battery": 1,
            "later_inside_temperature_c": 22.5,
            "empty_state_of_charge": None,
            "unicode_name": UNICODE_NAME,
        },
        "endpoint_restart": {
            "same_hub": True,
            "new_service_generation": True,
            "poll_status": 200,
        },
        "outage_recovery": {
            "unavailable_observed": True,
            "recovered": True,
            "registry_ids_preserved": True,
        },
        "unsupported_operation_zero_requests": {
            "operations": ["commands", "drives", "sse"],
            "outgoing_requests": 0,
        },
        "credential_loss_reauthentication": {
            "credential_loss_observed": True,
            "reauth_started": True,
            "reauth_completed": True,
            "entry_id_preserved": True,
            "registry_ids_preserved": True,
        },
        "installed_home_assistant_runtime": {
            "home_assistant_version": HOME_ASSISTANT_VERSION,
            "python_version": PYTHON_VERSION,
            "integration_version": PRODUCT_VERSION,
            "iot_class": "local_poll",
            "actors": ["ha_client", "ha_flow"],
        },
        "polling_transport_zero_sse": {
            "polling_requests_positive": True,
            "vehicle_requests_positive": True,
            "current_requests_positive": True,
            "sse_requests": 0,
            "last_event_id_requests": 0,
            "clean_unload": True,
            "post_unload_requests": 0,
        },
    }
)

OPTIONAL_DEFAULTS = MappingProxyType(
    {
        "access_state": None,
        "activity_state": None,
        "charge_limit_percent": None,
        "charging_power_kw": None,
        "charging_state": None,
        "data_quality": None,
        "estimated_range_km": None,
        "inside_temperature_c": None,
        "odometer_km": None,
        "outside_temperature_c": None,
        "software_update_state": None,
        "software_version": None,
        "state_of_charge": None,
        "telemetry_age_seconds": None,
    }
)

REQUEST_SHAPES = MappingProxyType(
    {
        "observe_installed_runtime": (),
        "probe": (("GET", "/.well-known/teslatlas-hub", 200),),
        "bad_pair": (
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("POST", "/v1/pairings/{pairing_id}/claim", 401),
        ),
        "expired_pair": (
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("POST", "/v1/pairings/{pairing_id}/claim", 401),
        ),
        "replay_pair": (
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("POST", "/v1/pairings/{pairing_id}/claim", 401),
        ),
        "config_flow_setup": (
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("POST", "/v1/pairings/{pairing_id}/claim", 200),
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("GET", "/v1/vehicles", 200),
            ("GET", "/v1/vehicles/{vehicle_id}/current", 200),
            ("GET", "/v1/vehicles/{vehicle_id}/current", 200),
        ),
        "unknown_current": (("GET", "/v1/vehicles/{vehicle_id}/current", 404),),
        "initial_poll": (),
        "later_poll": (
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("GET", "/v1/vehicles", 200),
            ("GET", "/v1/vehicles/{vehicle_id}/current", 200),
            ("GET", "/v1/vehicles/{vehicle_id}/current", 200),
        ),
        "unsupported_surface": (),
        "revocation_refresh": (
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("GET", "/v1/vehicles", 401),
        ),
        "reauthentication_flow": (
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("POST", "/v1/pairings/{pairing_id}/claim", 200),
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("GET", "/v1/vehicles", 200),
            ("GET", "/v1/vehicles/{vehicle_id}/current", 200),
            ("GET", "/v1/vehicles/{vehicle_id}/current", 200),
        ),
        "endpoint_restart_poll": (
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("GET", "/v1/vehicles", 200),
            ("GET", "/v1/vehicles/{vehicle_id}/current", 200),
            ("GET", "/v1/vehicles/{vehicle_id}/current", 200),
        ),
        "outage_poll": (
            ("GET", "/.well-known/teslatlas-hub", 200),
            ("GET", "/v1/vehicles", 200),
            ("GET", "/v1/vehicles/{vehicle_id}/current", 200),
            ("GET", "/v1/vehicles/{vehicle_id}/current", 200),
        ),
        "unload": (),
    }
)


def _decision(
    status: Literal["passed", "failed", "pending"], code: str
) -> AdmissionDecision:
    if code not in DECISION_CODES:
        raise RuntimeError("unreviewed admission decision code")
    return AdmissionDecision(status=status, code=code)


def _typed_equal(left: object, right: object) -> bool:
    if type(left) is not type(right):
        return False
    if isinstance(left, Mapping):
        return set(left) == set(right) and all(
            _typed_equal(left[key], right[key]) for key in left
        )
    if isinstance(left, list | tuple):
        return len(left) == len(right) and all(
            _typed_equal(a, b) for a, b in zip(left, right, strict=True)
        )
    return left == right


def _actor_value(actor: object, name: str) -> object:
    if isinstance(actor, Mapping):
        return actor.get(name)
    return getattr(actor, name, None)


def _invocation_value(invocation: object, name: str) -> object:
    if isinstance(invocation, Mapping):
        return invocation.get(name)
    return getattr(invocation, name, None)


def _runtime_raw(context: AdmissionContext) -> Mapping[str, object] | None:
    for value in context.raw.values():
        if value.get("operation") == "observe_installed_runtime":
            return value
    return None


def _operation_sequences(
    context: AdmissionContext, operation: str
) -> tuple[int, int] | None:
    values = [
        value
        for value in context.raw.values()
        if value.get("operation") == operation
    ]
    if len(values) != 1:
        return None
    before = values[0].get("session_sequence_before")
    after = values[0].get("session_sequence_after")
    if type(before) is not int or type(after) is not int:
        return None
    return before, after


def _independent_artifact(
    context: AdmissionContext,
) -> Mapping[str, object] | None:
    actor = context.actors.get("ha_flow")
    runtime = _actor_value(actor, "runtime")
    artifact = runtime.get("artifact") if isinstance(runtime, Mapping) else None
    return artifact if isinstance(artifact, Mapping) else None


def _same_observed_identity(*observations: Mapping[str, object]) -> bool:
    keys = (
        "scenario_sha256",
        "seed_sha256",
        "store_id",
        "store_schema_version",
        "hub_id",
    )
    return bool(observations) and all(
        all(value.get(key) == observations[0].get(key) for key in keys)
        for value in observations[1:]
    )


def _missing_independent_observation(
    case_id: str, context: AdmissionContext
) -> bool:
    binding = CASE_BINDINGS.get(case_id)
    if binding is None:
        return False
    _actor_id, operations = binding
    for operation in operations:
        sequences = _operation_sequences(context, operation)
        if sequences is None:
            continue
        before, after = sequences
        if (
            before not in context.controller_observations
            or after not in context.controller_observations
        ):
            return True
        observation = context.controller_observations.get(after)
        transition = (
            observation.get("transition")
            if isinstance(observation, Mapping)
            else None
        )
        if operation == "later_poll" and isinstance(transition, Mapping):
            for key in ("from_sequence", "pre_advance_verify_sequence"):
                sequence = transition.get(key)
                if (
                    type(sequence) is int
                    and sequence not in context.controller_observations
                ):
                    return True
        if operation in {"endpoint_restart_poll", "outage_poll"} and isinstance(
            transition, Mapping
        ):
            stopped_sequence = transition.get("stopped_sequence")
            if (
                type(stopped_sequence) is int
                and stopped_sequence not in context.controller_observations
            ):
                return True
    return False


OBSERVATION_KEYS = frozenset(
    {
        "schema_version",
        "state",
        "session_id",
        "sequence",
        "operation",
        "started_monotonic_ns",
        "finished_monotonic_ns",
        "observed_at_ms",
        "proof_sha256",
        "result_sha256",
        "scenario_sha256",
        "seed_sha256",
        "store_id",
        "store_schema_version",
        "service_generation",
        "hub_id",
        "invitations",
        "transition",
    }
)


def _valid_scenario(scenario: Mapping[str, object]) -> bool:
    vehicles = scenario.get("vehicles")
    current = scenario.get("current")
    later = scenario.get("later_current")
    return (
        scenario.get("schema_version") == 1
        and scenario.get("name") == "two-vehicles-five-drives"
        and scenario.get("provenance") == "synthetic-only"
        and scenario.get("observed_at_ms") == 1788566400000
        and scenario.get("vehicle_ids")
        == [
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
        ]
        and scenario.get("empty_vehicle_observed_at_ms") is None
        and vehicles
        == [
            {
                "vehicle_id": "11111111-1111-4111-8111-111111111111",
                "display_name": UNICODE_NAME,
            },
            {
                "vehicle_id": "22222222-2222-4222-8222-222222222222",
                "display_name": "Interop empty",
            },
        ]
        and isinstance(current, Mapping)
        and current.get("battery_level") == 0
        and current.get("inside_temp") == 21.5
        and current.get("outside_temp") is None
        and current.get("observed_at_ms") == 1788566400000
        and isinstance(later, Mapping)
        and later
        == {
            "observed_at_ms": 1788566460000,
            "battery_level": 1,
            "inside_temp": 22.5,
            "outside_temp": None,
        }
    )


def _running_observation(
    context: AdmissionContext, sequence: object
) -> Mapping[str, object] | None:
    value = context.controller_observations.get(sequence)
    if not isinstance(value, Mapping) or set(value) != OBSERVATION_KEYS:
        return None
    if (
        value.get("schema_version") != 1
        or value.get("state") != "running"
        or value.get("session_id") != context.session_id
        or value.get("sequence") != sequence
        or not isinstance(value.get("operation"), str)
        or type(value.get("started_monotonic_ns")) is not int
        or value["started_monotonic_ns"] < 0
        or type(value.get("finished_monotonic_ns")) is not int
        or value["finished_monotonic_ns"] < value["started_monotonic_ns"]
        or type(value.get("observed_at_ms")) is not int
        or value["observed_at_ms"] <= 0
        or any(
            not isinstance(value.get(key), str)
            or HEX64.fullmatch(str(value[key])) is None
            for key in (
                "proof_sha256",
                "result_sha256",
                "scenario_sha256",
                "seed_sha256",
            )
        )
        or not isinstance(value.get("store_id"), str)
        or type(value.get("store_schema_version")) is not int
        or value["store_schema_version"] <= 0
        or not isinstance(value.get("service_generation"), str)
        or not value["service_generation"]
        or not isinstance(value.get("hub_id"), str)
    ):
        return None
    for identity in (value["store_id"], value["hub_id"]):
        try:
            parsed_identity = uuid.UUID(identity)
        except (ValueError, AttributeError):
            return None
        if str(parsed_identity) != identity:
            return None
    invitations = value.get("invitations")
    if not isinstance(invitations, Mapping) or set(invitations) != {
        "active",
        "expired",
    }:
        return None
    for invitation in invitations.values():
        if (
            not isinstance(invitation, Mapping)
            or set(invitation) != {"pairing_id", "expires_at_ms"}
            or type(invitation.get("expires_at_ms")) is not int
            or invitation["expires_at_ms"] <= 0
        ):
            return None
        try:
            parsed = uuid.UUID(invitation.get("pairing_id"))
        except (ValueError, AttributeError):
            return None
        if str(parsed) != invitation["pairing_id"]:
            return None
    operation = value["operation"]
    if operation == "verify":
        if value.get("transition") is not None:
            return None
    elif operation not in {"advance-once", "revoke", "pair", "start"}:
        return None
    else:
        transition = value.get("transition")
        from_sequence = (
            transition.get("from_sequence")
            if isinstance(transition, Mapping)
            else None
        )
        if (
            type(from_sequence) is not int
            or from_sequence <= 0
            or not _transition_matches(value, operation, from_sequence)
        ):
            return None
        if operation == "advance-once" and (
            type(transition.get("pre_advance_verify_sequence")) is not int
            or transition["pre_advance_verify_sequence"] <= 0
            or any(
                not isinstance(transition.get(key), str)
                or HEX64.fullmatch(transition[key]) is None
                for key in (
                    "before_store_sha256",
                    "after_store_sha256",
                    "scenario_sha256",
                    "seed_sha256",
                )
            )
        ):
            return None
        if operation == "revoke":
            try:
                device_id = uuid.UUID(transition.get("device_id"))
            except (ValueError, AttributeError):
                return None
            if str(device_id) != transition["device_id"]:
                return None
        if operation == "start" and (
            type(transition.get("stopped_sequence")) is not int
            or transition["stopped_sequence"] <= 0
        ):
            return None
    return value


def _stopped_observation(
    context: AdmissionContext, sequence: object, from_sequence: int
) -> Mapping[str, object] | None:
    value = context.controller_observations.get(sequence)
    transition = value.get("transition") if isinstance(value, Mapping) else None
    if (
        not isinstance(value, Mapping)
        or set(value) != OBSERVATION_KEYS
        or value.get("schema_version") != 1
        or value.get("state") != "stopped"
        or value.get("session_id") != context.session_id
        or value.get("sequence") != sequence
        or value.get("operation") != "stop"
        or type(value.get("started_monotonic_ns")) is not int
        or value["started_monotonic_ns"] < 0
        or type(value.get("finished_monotonic_ns")) is not int
        or value["finished_monotonic_ns"] < value["started_monotonic_ns"]
        or type(value.get("observed_at_ms")) is not int
        or value["observed_at_ms"] <= 0
        or value.get("proof_sha256") is not None
        or value.get("invitations") is not None
        or not isinstance(transition, Mapping)
        or set(transition) != {"kind", "from_sequence"}
        or transition
        != {"kind": "stop", "from_sequence": from_sequence}
        or any(
            not isinstance(value.get(key), str)
            or HEX64.fullmatch(str(value[key])) is None
            for key in ("result_sha256", "scenario_sha256", "seed_sha256")
        )
        or type(value.get("store_schema_version")) is not int
        or value["store_schema_version"] <= 0
        or not isinstance(value.get("service_generation"), str)
        or not value["service_generation"]
    ):
        return None
    prior = _running_observation(context, from_sequence)
    if prior is None or any(
        value.get(key) != prior.get(key)
        for key in (
            "scenario_sha256",
            "seed_sha256",
            "store_id",
            "store_schema_version",
            "hub_id",
            "service_generation",
        )
    ):
        return None
    return value


def _transition_matches(
    observation: Mapping[str, object],
    kind: str,
    from_sequence: int,
) -> bool:
    transition = observation.get("transition")
    keys = {
        "advance-once": {
            "kind",
            "from_sequence",
            "pre_advance_verify_sequence",
            "before_store_sha256",
            "after_store_sha256",
            "scenario_sha256",
            "seed_sha256",
        },
        "revoke": {"kind", "from_sequence", "device_id"},
        "pair": {"kind", "from_sequence"},
        "start": {"kind", "from_sequence", "stopped_sequence"},
    }
    return (
        isinstance(transition, Mapping)
        and set(transition) == keys.get(kind)
        and transition.get("kind") == kind
        and transition.get("from_sequence") == from_sequence
    )


def _validate_context(context: AdmissionContext) -> str | None:
    if (
        context.adapter_id != ADAPTER_ID
        or context.cell_id
        not in {
            "home_assistant__macos_arm64",
            "home_assistant__debian13_arm64",
            "home_assistant__debian13_amd64",
        }
    ):
        return "wrong_context"
    try:
        parsed = uuid.UUID(context.session_id)
    except (ValueError, AttributeError):
        return "wrong_context"
    if parsed.version != 4 or str(parsed) != context.session_id:
        return "wrong_context"
    if not _valid_scenario(context.scenario):
        return "wrong_context"
    if set(context.actors) != {"ha_flow", "ha_client"}:
        return "wrong_actor"
    expected = {
        "ha_flow": ("installed_ha_flow", "ha_matrix_flow"),
        "ha_client": ("installed_ha_client", "ha_matrix_client"),
    }
    runtime_raw = _runtime_raw(context)
    if runtime_raw is None:
        return "raw_missing"
    facts = runtime_raw.get("facts")
    if not isinstance(facts, Mapping):
        return "raw_fact_mismatch"
    manifest_hashes = facts.get("actor_manifest_sha256s")
    if (
        not isinstance(manifest_hashes, Mapping)
        or set(manifest_hashes) != set(expected)
    ):
        return "installed_manifest_mismatch"
    shared_runtime: dict[str, object] | None = None
    for actor_id, (kind, entrypoint) in expected.items():
        actor = context.actors[actor_id]
        if (
            _actor_value(actor, "id") != actor_id
            or _actor_value(actor, "kind") != kind
            or _actor_value(actor, "runtime_ref") != "ha_container"
            or _actor_value(actor, "entrypoint_ref") != entrypoint
        ):
            return "wrong_actor"
        if tuple(_actor_value(actor, "source_roles") or ()) != (SOURCE_ROLE,):
            return "wrong_source_role"
        if tuple(_actor_value(actor, "artifact_roles") or ()) != (ARCHIVE_ROLE,):
            return "wrong_artifact_role"
        manifest = _actor_value(actor, "installed_manifest")
        if (
            not isinstance(manifest, Mapping)
            or set(manifest) != {"path", "sha256"}
            or manifest.get("sha256") != manifest_hashes.get(actor_id)
            or not isinstance(manifest.get("path"), str)
            or not str(manifest["path"]).startswith("/")
            or HEX64.fullmatch(str(manifest.get("sha256"))) is None
        ):
            return "installed_manifest_mismatch"
        runtime = _actor_value(actor, "runtime")
        if not isinstance(runtime, Mapping) or set(runtime) != {
            "schema_version",
            "runtime_ref",
            "runtime_kind",
            "container_id",
            "python",
            "home_assistant",
            "artifact",
            "actor_input_manifest_sha256",
        }:
            return "wrong_context"
        python = runtime.get("python")
        home_assistant = runtime.get("home_assistant")
        artifact = runtime.get("artifact")
        if (
            runtime.get("schema_version") != 1
            or runtime.get("runtime_ref") != "ha_container"
            or runtime.get("runtime_kind") != "docker-container"
            or not isinstance(runtime.get("container_id"), str)
            or HEX64.fullmatch(runtime["container_id"]) is None
            or not isinstance(python, Mapping)
            or set(python) != {"executable", "sha256", "version"}
            or python.get("executable") != facts.get("python_executable")
            or python.get("version") != PYTHON_VERSION
            or HEX64.fullmatch(str(python.get("sha256"))) is None
            or not isinstance(home_assistant, Mapping)
            or home_assistant != {"version": HOME_ASSISTANT_VERSION}
            or not isinstance(artifact, Mapping)
            or set(artifact)
            != {
                "role",
                "sha256",
                "installed_root",
                "installed_manifest_sha256",
                "installed_members",
            }
            or artifact.get("role") != ARCHIVE_ROLE
            or artifact.get("sha256") != facts.get("archive_sha256")
            or artifact.get("installed_manifest_sha256")
            != facts.get("installed_manifest_sha256")
            or not isinstance(artifact.get("installed_members"), list)
            or len(artifact["installed_members"]) != 31
            or any(
                not isinstance(row, Mapping)
                or set(row) != {"path", "bytes", "mode", "sha256"}
                or not isinstance(row.get("path"), str)
                or not row["path"]
                or row["path"].startswith("/")
                or ".." in row["path"].split("/")
                or type(row.get("bytes")) is not int
                or row["bytes"] < 0
                or type(row.get("mode")) is not int
                or HEX64.fullmatch(str(row.get("sha256"))) is None
                for row in artifact["installed_members"]
            )
            or [row["path"] for row in artifact["installed_members"]]
            != sorted(row["path"] for row in artifact["installed_members"])
            or len({row["path"] for row in artifact["installed_members"]}) != 31
            or not isinstance(artifact.get("installed_root"), str)
            or not str(artifact["installed_root"]).startswith("/")
            or runtime.get("actor_input_manifest_sha256")
            != manifest_hashes.get(actor_id)
        ):
            return "installed_manifest_mismatch"
        candidate_shared = {
            key: runtime[key]
            for key in runtime
            if key != "actor_input_manifest_sha256"
        }
        if shared_runtime is None:
            shared_runtime = candidate_shared
        elif not _typed_equal(shared_runtime, candidate_shared):
            return "wrong_context"
    return None


def _expected_case(case_id: str, context: AdmissionContext) -> dict | None:
    if case_id in STATIC_FACTS:
        return dict(STATIC_FACTS[case_id])
    runtime = _runtime_raw(context)
    if runtime is None or not isinstance(runtime.get("facts"), Mapping):
        return None
    runtime_facts = runtime["facts"]
    if case_id == "candidate_artifact_identity":
        artifacts = context.header.get("artifacts")
        if not isinstance(artifacts, list):
            return None
        archive = next(
            (
                item
                for item in artifacts
                if isinstance(item, Mapping) and item.get("role") == ARCHIVE_ROLE
            ),
            None,
        )
        if archive is None:
            return None
        return {
            "archive_sha256": archive.get("sha256"),
            "integration_version": PRODUCT_VERSION,
            "installed_manifest_sha256": runtime_facts.get(
                "installed_manifest_sha256"
            ),
            "installed_members": 31,
        }
    if case_id == "installed_service_runtime":
        service_mode = (
            "installed-app-launchagent"
            if context.cell_id.endswith("macos_arm64")
            else "installed-deb-systemd"
        )
        return {"service_mode": service_mode, "status": "pending"}
    if case_id == "discovery_identity_profile":
        sequences = _operation_sequences(context, "probe")
        observation = (
            _running_observation(context, sequences[0])
            if sequences is not None and sequences[0] == sequences[1]
            else None
        )
        if observation is None:
            return None
        hub_id = observation["hub_id"]
        return {
            "hub_id": hub_id,
            "api_versions": ["1.0"],
            "protocol": "teslatlas-sync",
            "protocol_major": 1,
            "version": PRODUCT_VERSION,
        }
    return None


def _expected_operation_facts(
    operation: str, context: AdmissionContext
) -> Mapping[str, object] | None:
    runtime = _runtime_raw(context)
    if runtime is None or not isinstance(runtime.get("facts"), Mapping):
        return None
    runtime_facts = runtime["facts"]
    if operation == "observe_installed_runtime":
        expected = _expected_case("candidate_artifact_identity", context)
        artifact = _independent_artifact(context)
        sequences = _operation_sequences(context, operation)
        observation = (
            _running_observation(context, sequences[0])
            if sequences is not None and sequences[0] == sequences[1]
            else None
        )
        if expected is None or artifact is None or observation is None:
            return None
        module_path = runtime_facts.get("module_path")
        module_sha256 = runtime_facts.get("module_sha256")
        python_executable = runtime_facts.get("python_executable")
        loaded_modules = runtime_facts.get("loaded_modules")
        members = artifact.get("installed_members")
        if not isinstance(members, list):
            return None
        members_by_path = {row["path"]: row for row in members}
        if (
            module_path != "__init__.py"
            or module_sha256 != members_by_path.get("__init__.py", {}).get("sha256")
            or not isinstance(python_executable, str)
            or not python_executable.startswith("/")
            or runtime_facts.get("manifest_domain") != "teslatlas_hub"
            or runtime_facts.get("config_entry_loaded") is not True
            or runtime_facts.get("entity_registry_count") != 26
            or not isinstance(loaded_modules, Mapping)
            or not set(REQUIRED_LOADED_MODULES).issubset(loaded_modules)
            or any(
                not isinstance(name, str)
                or not isinstance(path, str)
                or (
                    "__init__.py"
                    if name == "custom_components.teslatlas_hub"
                    else name.removeprefix("custom_components.teslatlas_hub.")
                    .replace(".", "/")
                    + ".py"
                )
                != path
                or path not in members_by_path
                for name, path in loaded_modules.items()
            )
        ):
            return None
        return {
            **expected,
            **STATIC_FACTS["installed_home_assistant_runtime"],
            "service_mode": (
                "installed-app-launchagent"
                if context.cell_id.endswith("macos_arm64")
                else "installed-deb-systemd"
            ),
            "hub_id": observation["hub_id"],
            "actor_manifest_sha256s": runtime_facts.get("actor_manifest_sha256s"),
            "expired_fixture": runtime_facts.get("expired_fixture"),
            "entry_id": runtime_facts.get("entry_id"),
            "registry_ids": runtime_facts.get("registry_ids"),
            "python_executable": python_executable,
            "module_path": module_path,
            "module_sha256": module_sha256,
            "loaded_modules": runtime_facts.get("loaded_modules"),
            "manifest_domain": "teslatlas_hub",
            "config_entry_loaded": True,
            "entity_registry_count": 26,
        }
    if operation == "probe":
        discovery = _expected_case("discovery_identity_profile", context)
        return (
            {**discovery, **STATIC_FACTS["unauthenticated_discovery"]}
            if discovery is not None
            else None
        )
    if operation in {"bad_pair", "replay_pair"}:
        case_id = "bad_invitation" if operation == "bad_pair" else "replayed_invitation"
        sequences = _operation_sequences(context, operation)
        initial = (
            _running_observation(context, sequences[0])
            if sequences is not None
            else None
        )
        if initial is None:
            return None
        return {
            **STATIC_FACTS[case_id],
            "pairing_id": initial["invitations"]["active"]["pairing_id"],
        }
    if operation == "expired_pair":
        facts = runtime_facts.get("expired_fixture")
        if (
            not isinstance(facts, Mapping)
            or set(facts)
            != {
                "fixture_expires_at_ms",
                "observed_after_expiry_ms",
                "fixture_kind",
                "pairing_id",
            }
            or type(facts["fixture_expires_at_ms"]) is not int
            or type(facts["observed_after_expiry_ms"]) is not int
            or facts["fixture_expires_at_ms"] >= facts["observed_after_expiry_ms"]
            or facts["fixture_kind"] != "persisted-one-second"
        ):
            return None
        sequences = _operation_sequences(context, operation)
        observation = (
            _running_observation(context, sequences[0])
            if sequences is not None
            else None
        )
        if observation is None:
            return None
        expired = observation["invitations"]["expired"]
        if (
            facts["pairing_id"] != expired["pairing_id"]
            or facts["fixture_expires_at_ms"] != expired["expires_at_ms"]
            or observation["observed_at_ms"] <= expired["expires_at_ms"]
        ):
            return None
        return {**STATIC_FACTS["expired_invitation"], **facts}
    if operation == "config_flow_setup":
        return {
            **STATIC_FACTS["real_auth"],
            "entry_id": runtime_facts.get("entry_id"),
            "registry_ids": runtime_facts.get("registry_ids"),
        }
    if operation == "unknown_current":
        return {
            **STATIC_FACTS["unknown_vehicle"],
            "optional_fields": dict(OPTIONAL_DEFAULTS),
        }
    if operation == "initial_poll":
        return {
            "vehicles": 2,
            "sensors_per_vehicle": 13,
            "initial_battery": 0,
            "initial_inside_temperature_c": 21.5,
            "empty_state_of_charge": None,
            "unicode_name": UNICODE_NAME,
        }
    if operation == "later_poll":
        sequences = _operation_sequences(context, operation)
        if sequences is None:
            return None
        before, _after = sequences
        observation = _running_observation(context, before)
        transition = observation.get("transition") if observation else None
        initial_sequences = _operation_sequences(context, "initial_poll")
        from_sequence = (
            transition.get("from_sequence")
            if isinstance(transition, Mapping)
            else None
        )
        verify_sequence = (
            transition.get("pre_advance_verify_sequence")
            if isinstance(transition, Mapping)
            else None
        )
        predecessor = _running_observation(context, from_sequence)
        pre_advance_verify = _running_observation(context, verify_sequence)
        if (
            observation is None
            or observation.get("operation") != "advance-once"
            or not isinstance(transition, Mapping)
            or not _transition_matches(
                observation, "advance-once", transition.get("from_sequence")
            )
            or type(from_sequence) is not int
            or type(verify_sequence) is not int
            or not from_sequence < verify_sequence < before
            or initial_sequences != (from_sequence, from_sequence)
            or predecessor is None
            or predecessor.get("operation") != "verify"
            or pre_advance_verify is None
            or pre_advance_verify.get("operation") != "verify"
            or not _same_observed_identity(
                predecessor, pre_advance_verify, observation
            )
            or predecessor.get("service_generation")
            != pre_advance_verify.get("service_generation")
            or observation.get("service_generation")
            == pre_advance_verify.get("service_generation")
            or transition.get("before_store_sha256")
            == transition.get("after_store_sha256")
            or transition.get("scenario_sha256")
            != observation.get("scenario_sha256")
            or transition.get("seed_sha256") != observation.get("seed_sha256")
        ):
            return None
        return {
            "later_battery": 1,
            "later_inside_temperature_c": 22.5,
            "advanced_once": True,
            "advance_from_sequence": from_sequence,
        }
    if operation == "unsupported_surface":
        return STATIC_FACTS["unsupported_operation_zero_requests"]
    if operation == "revocation_refresh":
        sequences = _operation_sequences(context, operation)
        if sequences is None:
            return None
        before, after = sequences
        observation = _running_observation(context, after)
        transition = observation.get("transition") if observation else None
        if (
            observation is None
            or observation.get("operation") != "revoke"
            or not _transition_matches(observation, "revoke", before)
            or not isinstance(transition.get("device_id"), str)
        ):
            return None
        return {
            **STATIC_FACTS["revocation"],
            "credential_loss_observed": True,
            "device_id": transition["device_id"],
        }
    if operation == "reauthentication_flow":
        sequences = _operation_sequences(context, operation)
        if sequences is None:
            return None
        before, after = sequences
        observation = _running_observation(context, after)
        if (
            observation is None
            or observation.get("operation") != "pair"
            or not _transition_matches(observation, "pair", before)
        ):
            return None
        return {
            **STATIC_FACTS["credential_lifecycle_reauth"],
            "reauth_completed": True,
            "pairing_id": observation["invitations"]["active"]["pairing_id"],
        }
    if operation == "endpoint_restart_poll":
        sequences = _operation_sequences(context, operation)
        if sequences is None:
            return None
        before, after = sequences
        prior = _running_observation(context, before)
        restarted = _running_observation(context, after)
        transition = restarted.get("transition") if restarted else None
        if (
            prior is None
            or restarted is None
            or restarted.get("operation") != "start"
            or not isinstance(transition, Mapping)
            or not isinstance(transition.get("stopped_sequence"), int)
            or transition.get("from_sequence") != transition["stopped_sequence"]
            or not _transition_matches(
                restarted, "start", transition["stopped_sequence"]
            )
            or not before < transition["stopped_sequence"] < after
            or _stopped_observation(
                context, transition["stopped_sequence"], before
            )
            is None
            or prior["hub_id"] != restarted["hub_id"]
            or prior["service_generation"] == restarted["service_generation"]
        ):
            return None
        return {
            **STATIC_FACTS["endpoint_restart"],
            "before_service_generation": prior["service_generation"],
            "after_service_generation": restarted["service_generation"],
        }
    if operation == "outage_poll":
        sequences = _operation_sequences(context, operation)
        if sequences is None:
            return None
        before, after = sequences
        prior = _running_observation(context, before)
        restarted = _running_observation(context, after)
        transition = restarted.get("transition") if restarted else None
        if (
            prior is None
            or restarted is None
            or restarted.get("operation") != "start"
            or not isinstance(transition, Mapping)
            or not isinstance(transition.get("stopped_sequence"), int)
            or transition.get("from_sequence") != transition["stopped_sequence"]
            or not _transition_matches(
                restarted, "start", transition["stopped_sequence"]
            )
            or not before < transition["stopped_sequence"] < after
            or _stopped_observation(
                context, transition["stopped_sequence"], before
            )
            is None
            or prior["service_generation"] == restarted["service_generation"]
        ):
            return None
        return {
            **STATIC_FACTS["outage_recovery"],
            "before_service_generation": prior["service_generation"],
            "after_service_generation": restarted["service_generation"],
        }
    if operation == "unload":
        values = [
            value.get("facts")
            for value in context.raw.values()
            if value.get("operation") == "unload"
        ]
        if len(values) != 1 or not isinstance(values[0], Mapping):
            return None
        facts = values[0]
        counts = (
            "attempted_requests",
            "completed_requests",
            "failed_requests",
            "cancelled_requests",
            "pending_requests",
        )
        attempts = facts.get("attempts")
        outcome_counts = {
            outcome: len(
                [
                    attempt
                    for attempt in attempts
                    if isinstance(attempt, Mapping)
                    and attempt.get("outcome") == outcome
                ]
            )
            if isinstance(attempts, list)
            else -1
            for outcome in ("completed", "failed", "cancelled", "pending")
        }
        transcript_attempts = sorted(
            (request.get("method"), request.get("route"))
            for value in context.raw.values()
            for request in (
                value.get("requests", []) if isinstance(value, Mapping) else []
            )
            if isinstance(request, Mapping)
        )
        completed_attempts = sorted(
            (attempt.get("method"), attempt.get("route"))
            for attempt in attempts
            if isinstance(attempt, Mapping)
            and attempt.get("outcome") == "completed"
        ) if isinstance(attempts, list) else []
        if (
            not all(type(facts.get(key)) is int and facts[key] >= 0 for key in counts)
            or facts.get("attempted_requests")
            != sum(facts[key] for key in counts[1:])
            or facts.get("pending_requests") != 0
            or not isinstance(attempts, list)
            or len(attempts) > 1024
            or len(attempts) != facts.get("attempted_requests")
            or any(
                not isinstance(attempt, Mapping)
                or set(attempt) != {"method", "route", "outcome", "phase"}
                or attempt.get("method") not in {"GET", "POST"}
                or not isinstance(attempt.get("route"), str)
                or not attempt["route"].startswith("/")
                or len(attempt["route"]) > 256
                or attempt.get("outcome")
                not in {"completed", "failed", "cancelled", "pending"}
                or attempt.get("phase") not in {"before_unload", "after_unload"}
                for attempt in attempts
            )
            or any(
                outcome_counts[outcome] != facts.get(f"{outcome}_requests")
                for outcome in outcome_counts
            )
            or completed_attempts != transcript_attempts
            or len(
                [
                    attempt
                    for attempt in attempts
                    if attempt["phase"] == "after_unload"
                ]
            )
            != facts.get("post_unload_attempts")
            or type(facts.get("scheduled_callback_deadline_ns")) is not int
            or facts["scheduled_callback_deadline_ns"] <= 0
            or type(facts.get("observed_after_deadline_ns")) is not int
            or facts["observed_after_deadline_ns"]
            <= facts["scheduled_callback_deadline_ns"]
            or facts.get("scheduled_callback_cancelled") is not True
            or facts.get("sse_requests") != 0
            or any("event" in attempt["route"].lower() for attempt in attempts)
            or facts.get("last_event_id_requests") != 0
            or facts.get("post_unload_requests") != 0
            or facts.get("post_unload_attempts") != 0
            or facts.get("clean_unload") is not True
        ):
            return None
        return dict(facts)
    return None


def _validate_request_shape(operation: str, requests: object) -> bool:
    expected = REQUEST_SHAPES.get(operation)
    if (
        expected is None
        or not isinstance(requests, list)
        or len(requests) != len(expected)
    ):
        return False
    actual_shapes = []
    request_ids: set[str] = set()
    for request in requests:
        if not isinstance(request, Mapping) or set(request) != {
            "method",
            "route",
            "status",
            "request_id",
        }:
            return False
        request_id = request["request_id"]
        if (
            not isinstance(request_id, str)
            or not request_id
            or request_id in request_ids
        ):
            return False
        request_ids.add(request_id)
        if type(request["status"]) is not int:
            return False
        actual_shapes.append((request["method"], request["route"], request["status"]))
    return tuple(actual_shapes) == expected


def admit_case(case: dict, context: AdmissionContext) -> AdmissionDecision:
    """Admit one HA case only when reviewed actor and raw facts prove it."""
    if not isinstance(case, dict) or set(case) != {
        "id",
        "status",
        "expected",
        "actual",
        "evidence_kind",
        "request_transcript",
    }:
        return _decision("failed", "case_shape")
    case_id = case.get("id")
    if case_id not in REQUIRED_CASES:
        return _decision("failed", "unknown_case")
    context_problem = _validate_context(context)
    if context_problem is not None:
        return _decision("failed", context_problem)
    if _missing_independent_observation(case_id, context):
        return _decision("pending", "independent_observation_pending")
    expected = _expected_case(case_id, context)
    if (
        expected is None
        or not _typed_equal(case["expected"], expected)
        or not _typed_equal(case["actual"], expected)
    ):
        return _decision("failed", "literal_mismatch")
    expected_kind = (
        "zero_request"
        if case_id == "unsupported_operation_zero_requests"
        else "identity"
        if case_id
        in {
            "candidate_artifact_identity",
            "installed_service_runtime",
            "installed_home_assistant_runtime",
        }
        else "http"
    )
    expected_status = "pending" if case_id == "installed_service_runtime" else "passed"
    if case["evidence_kind"] != expected_kind or case["status"] != expected_status:
        return _decision("failed", "case_shape")
    actor_id, operations = CASE_BINDINGS[case_id]
    candidates = [
        invocation
        for invocation in context.invocations
        if _invocation_value(invocation, "case_id") == case_id
    ]
    if len(candidates) != len(operations):
        return _decision("failed", "invocation_missing")
    by_operation = {
        _invocation_value(invocation, "operation"): invocation
        for invocation in candidates
    }
    if tuple(by_operation) != operations:
        return _decision("failed", "operation_mismatch")
    case_requests: list[object] = []
    for operation in operations:
        invocation = by_operation[operation]
        if _invocation_value(invocation, "actor_id") != actor_id:
            return _decision("failed", "wrong_actor")
        before = _invocation_value(invocation, "session_sequence_before")
        after = _invocation_value(invocation, "session_sequence_after")
        if (
            type(before) is not int
            or type(after) is not int
            or before <= 0
            or after < before
            or before not in context.controller_observations
            or after not in context.controller_observations
        ):
            return _decision("failed", "sequence_mismatch")
        before_observation = _running_observation(context, before)
        after_observation = _running_observation(context, after)
        if (
            before_observation is None
            or after_observation is None
            or before_observation["scenario_sha256"]
            != after_observation["scenario_sha256"]
            or before_observation["seed_sha256"]
            != after_observation["seed_sha256"]
            or before_observation["store_id"] != after_observation["store_id"]
            or before_observation["store_schema_version"]
            != after_observation["store_schema_version"]
            or before_observation["hub_id"] != after_observation["hub_id"]
        ):
            return _decision("failed", "sequence_mismatch")
        evidence_id = _invocation_value(invocation, "evidence_id")
        raw = context.raw.get(evidence_id)
        if raw is None:
            return _decision("failed", "raw_missing")
        if not isinstance(raw, Mapping) or set(raw) != {
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
        }:
            return _decision("failed", "raw_identity_mismatch")
        if (
            raw["schema_version"] != 1
            or raw["session_id"] != context.session_id
            or raw["cell_id"] != context.cell_id
            or raw["actor_id"] != actor_id
            or raw["operation"] != operation
            or not isinstance(raw["session_input_sha256"], str)
            or HEX64.fullmatch(raw["session_input_sha256"]) is None
        ):
            return _decision("failed", "raw_identity_mismatch")
        if (
            raw["session_sequence_before"] != before
            or raw["session_sequence_after"] != after
        ):
            return _decision("failed", "sequence_mismatch")
        expected_facts = _expected_operation_facts(operation, context)
        if expected_facts is None or not _typed_equal(raw["facts"], expected_facts):
            if (
                operation == "unload"
                and isinstance(raw["facts"], Mapping)
                and raw["facts"].get("clean_unload") is False
            ):
                return _decision("failed", "cleanup_failure")
            return _decision("failed", "raw_fact_mismatch")
        if not _validate_request_shape(operation, raw["requests"]):
            return _decision("failed", "request_mismatch")
        request_ids = tuple(item["request_id"] for item in raw["requests"])
        if request_ids != tuple(_invocation_value(invocation, "request_ids") or ()):
            return _decision("failed", "request_mismatch")
        case_requests.extend(raw["requests"])
    if not _typed_equal(case["request_transcript"], case_requests):
        return _decision("failed", "request_mismatch")
    if case_id == "installed_service_runtime":
        return _decision("pending", "runner_owned_service_runtime")
    return _decision("passed", "accepted")
