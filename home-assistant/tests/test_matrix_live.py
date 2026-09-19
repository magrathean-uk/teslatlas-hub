"""Contract tests for the installed Home Assistant matrix adapter."""

from __future__ import annotations

import asyncio
import copy
import hashlib
import json
import os
import shutil
import ssl
import subprocess
import sys
import threading
import time
import types
import uuid
from dataclasses import replace
from pathlib import Path
from types import MappingProxyType

import pytest

from tools.matrix_contract import (
    ADAPTER_ID,
    CONTRACT_REVISION,
    REQUIRED_CASES,
    AdmissionContext,
    AdmittedActor,
    AdmittedInvocation,
    admit_case,
)
from tools.matrix_contract import CASE_BINDINGS as CONTRACT_CASE_BINDINGS
from tools.matrix_live import (
    MatrixRuntime,
    _MatrixRequestCensus,
    _pytest_arguments,
    _require_framework_success,
)
from tools.matrix_wire import (
    CoordinationChannel,
    MatrixWireError,
    StdioBroker,
    canonical_json_bytes,
    file_binding,
    load_session_input,
)

SHA = "a" * 64
SESSION_ID = "12345678-1234-4234-8234-123456789abc"
TEST_CERTIFICATE = b"""-----BEGIN CERTIFICATE-----
MIICpDCCAYwCCQClFbs0/OXqcTANBgkqhkiG9w0BAQsFADAUMRIwEAYDVQQDDAls
b2NhbGhvc3QwHhcNMjYwOTA1MjM0MjAyWhcNMjYwOTA2MjM0MjAyWjAUMRIwEAYD
VQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDt
/VxGUeSO7gxCK4aCkWu6IeEDGJ5yuQzBf3gA8K2mxpxC8CsGR3hQFpByqq3Tlpjv
vuf1TlcJJZMkKGjxsJNJsylwvzC/CJ5Nk6nZk+rWB7S8CVoFuohamKfdB2v6VtDc
HszJr9NiDBzG5ATxfsR9yfHRPsFWfgP6mDRLJ+5EK9P40TL0jmrC6lT9Z7+ZZJh9
RlH3n+eyjPG4vTXsap+WRCN3l4KSzywos232uLQiJWcd0AYTOwKBEgQ9MqTk1AnN
KKzqL2qz5J1Cx7O+imEQvA+ToeyLJM91f3Wm9mM+piMXmgR4C1xPWUqLnXyBLHgZ
YF4G/NfPDhO71td1mpATAgMBAAEwDQYJKoZIhvcNAQELBQADggEBAIHarI6WTl2Q
F0CjxyOzFd4mQL7qDIEHwVG+B2EguDin+AYyB1wc6XRpwOn0DacNsnxQAzYdQtRW
vaI9p+xIC29/g+NaK1ddU6o/kbgYasUkbXvJL/Ymkd3vVrutoUj6RZOguKm3wdog
dRuyaY8UfsRzz/RVjYSDeiyOnSrkPS4t1zEoxV78xNYjDJTTa370wt+SpOTkX8tK
f2f3Z9vzs8j+HPgzdL+lK2D+KO9SMl4PwaYogXw67Y3Y7WbjYHUG5Sps5+Jw7yF/
G8Phk0GwmkT0izAEmmCNGRW/UuDHO5DfdokzInP+w0EmYKFcHUJg2cbp7lUEgLrr
OKbIz/1pkPw=
-----END CERTIFICATE-----
"""


def _binding(path: Path) -> dict:
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    return {"path": str(path), "sha256": digest}


def _staged(identifier: str, local: Path, *, root: Path | None = None) -> dict:
    local_binding = _binding(local)
    root_binding = dict(local_binding, path=str(root or local))
    return {"id": identifier, "root": root_binding, "local": local_binding}


def _write_private(path: Path, value: object) -> None:
    path.write_bytes(canonical_json_bytes(value) + b"\n")
    path.chmod(0o600)


def _session(tmp_path: Path) -> tuple[Path, dict]:
    tmp_path.mkdir(mode=0o700, parents=True, exist_ok=True)
    inputs = tmp_path / "inputs"
    outputs = tmp_path / "outputs"
    inputs.mkdir(mode=0o700)
    outputs.mkdir(mode=0o700)
    header_path = inputs / "header.json"
    contract_path = inputs / "contract.json"
    profile_path = inputs / "SHA256SUMS"
    scenario_path = inputs / "scenario.json"
    certificate_path = inputs / "certificate.pem"
    archive_path = inputs / "integration.tar"
    installed_path = inputs / "installed.json"
    actor_flow_path = inputs / "actor-flow.json"
    actor_client_path = inputs / "actor-client.json"
    for path, payload in (
        (header_path, {"header": True}),
        (contract_path, {"contract": True}),
        (scenario_path, {"scenario": True}),
        (installed_path, {"installed": True}),
        (actor_flow_path, {"actor": "flow"}),
        (actor_client_path, {"actor": "client"}),
    ):
        _write_private(path, payload)
    profile_source = (
        Path(__file__).parents[1]
        / "custom_components/teslatlas_hub/profile/hub-http-v1/1.0.0"
    )
    certificate_path.write_bytes(TEST_CERTIFICATE)
    certificate_path.chmod(0o600)
    archive_path.write_bytes(b"archive\n")
    archive_path.chmod(0o600)
    profile_members = []
    profile_local_root = inputs / "profile/hub-http-v1/1.0.0"
    for source in sorted(profile_source.rglob("*")):
        if not source.is_file():
            continue
        relative = source.relative_to(profile_source)
        member = profile_local_root / relative
        member.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        shutil.copyfile(source, member)
        member.chmod(0o600)
        profile_members.append(
            _staged(
                "profile-" + str(relative).replace("/", "-"),
                member,
                root=Path("/root/profile/hub-http-v1/1.0.0") / relative,
            )
        )
    profile_path = profile_local_root / "SHA256SUMS"
    _write_private(
        header_path,
        {"profile_sha256": hashlib.sha256(profile_path.read_bytes()).hexdigest()},
    )
    value = {
        "schema_version": 1,
        "kind": "matrix-adapter-session",
        "run_id": "final-b",
        "cell_id": "home_assistant__debian13_arm64",
        "adapter_id": "home_assistant",
        "client_id": "home_assistant",
        "session_id": SESSION_ID,
        "instance_nonce": "b" * 64,
        "header": _staged("header", header_path, root=Path("/root/header.json")),
        "case_contract": _staged(
            "contract", contract_path, root=Path("/root/contract.json")
        ),
        "host_session": {
            "schema_version": 1,
            "kind": "installed-host",
            "broker_socket": "/root/run/broker.sock",
            "session_id": SESSION_ID,
            "registration_sha256": "c" * 64,
        },
        "broker": {"kind": "stdio", "socket_path": None},
        "inputs": {
            "profile_manifest": _staged(
                "profile-manifest",
                profile_path,
                root=Path("/root/profile/hub-http-v1/1.0.0/SHA256SUMS"),
            ),
            "profile_members": profile_members,
            "scenario": _staged(
                "scenario", scenario_path, root=Path("/root/scenario.json")
            ),
            "certificate": _staged(
                "certificate", certificate_path, root=Path("/root/certificate.pem")
            ),
            "certificate_der_sha256": hashlib.sha256(
                ssl.PEM_cert_to_DER_cert(TEST_CERTIFICATE.decode("ascii"))
            ).hexdigest(),
            "product_inputs": [
                {
                    "artifact_role": "home_assistant_integration_archive",
                    "staged": _staged(
                        "integration-archive",
                        archive_path,
                        root=Path("/root/integration.tar"),
                    ),
                    "installed_manifest": _staged(
                        "installed-manifest",
                        installed_path,
                        root=Path("/root/installed.json"),
                    ),
                    "local_root": str(inputs),
                }
            ],
        },
        "actors": [
            {
                "id": "ha_flow",
                "kind": "installed_ha_flow",
                "execution": "coordinator",
                "runtime_ref": "ha_container",
                "artifact_roles": ["home_assistant_integration_archive"],
                "source_roles": ["home_assistant_source"],
                "entrypoint_ref": "ha_matrix_flow",
                "input_manifest": _staged(
                    "ha-flow-inputs", actor_flow_path, root=Path("/root/flow.json")
                ),
                "phase_contract": None,
            },
            {
                "id": "ha_client",
                "kind": "installed_ha_client",
                "execution": "coordinator",
                "runtime_ref": "ha_container",
                "artifact_roles": ["home_assistant_integration_archive"],
                "source_roles": ["home_assistant_source"],
                "entrypoint_ref": "ha_matrix_client",
                "input_manifest": _staged(
                    "ha-client-inputs",
                    actor_client_path,
                    root=Path("/root/client.json"),
                ),
                "phase_contract": None,
            },
        ],
        "outputs": {
            "normalized": str(outputs / "normalized.json"),
            "actor_evidence": str(outputs / "actors.json"),
            "coordination_dir": str(outputs / "coordination"),
            "framework_log": str(outputs / "framework.log"),
        },
        "bounds": {
            "cell_timeout_ms": 3_600_000,
            "cleanup_timeout_ms": 45_000,
            "frame_bytes": 1_048_576,
            "evidence_bytes": 8_388_608,
            "framework_log_bytes": 8_388_608,
        },
    }
    path = inputs / "session.json"
    _write_private(path, value)
    return path, value


def test_session_input_uses_local_staged_paths_without_opening_root_paths(
    tmp_path: Path,
) -> None:
    """A missing Docker-host root path must remain an identity reference."""
    path, value = _session(tmp_path)

    parsed = load_session_input(path)

    assert parsed == value
    assert parsed["inputs"]["profile_manifest"]["local"]["sha256"] == (
        "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"
    )


def test_session_input_rejects_extra_fields_and_output_alias(tmp_path: Path) -> None:
    """A relaxed object or input/output alias would break the closed wire."""
    path, value = _session(tmp_path)
    value["unexpected"] = True
    _write_private(path, value)
    with pytest.raises(MatrixWireError, match="session input shape"):
        load_session_input(path)

    path, value = _session(tmp_path / "second")
    value["outputs"]["normalized"] = value["header"]["local"]["path"]
    _write_private(path, value)
    with pytest.raises(MatrixWireError, match="aliases an input"):
        load_session_input(path)


@pytest.mark.parametrize(
    ("mutation", "message"),
    [
        ("duplicate_member", "staged input path is duplicated"),
        ("bad_layout", "profile member set is invalid"),
        ("wrong_der", "certificate DER digest mismatch"),
        ("manifest_substitution", "profile manifest differs from SHA256SUMS member"),
    ],
)
def test_session_input_rejects_profile_and_certificate_substitutions(
    tmp_path: Path, mutation: str, message: str
) -> None:
    """Exact layout, member bytes, aliases and PEM DER are admission inputs."""
    path, value = _session(tmp_path)
    if mutation == "duplicate_member":
        value["inputs"]["profile_members"][1]["local"] = dict(
            value["inputs"]["profile_members"][0]["local"]
        )
        value["inputs"]["profile_members"][1]["root"]["sha256"] = value[
            "inputs"
        ]["profile_members"][0]["local"]["sha256"]
    elif mutation == "bad_layout":
        value["inputs"]["profile_members"][0]["root"]["path"] = (
            "/root/wrong/SHA256SUMS"
        )
    elif mutation == "wrong_der":
        value["inputs"]["certificate_der_sha256"] = "0" * 64
    else:
        replacement = tmp_path / "inputs/replacement-SHA256SUMS"
        replacement.write_bytes(
            Path(value["inputs"]["profile_manifest"]["local"]["path"])
            .read_bytes()
            + b"0" * 64
            + b"  substituted.json\n"
        )
        replacement.chmod(0o600)
        value["inputs"]["profile_manifest"] = _staged(
            "profile-manifest",
            replacement,
            root=Path("/root/profile/hub-http-v1/1.0.0/SHA256SUMS"),
        )
    _write_private(path, value)

    with pytest.raises(MatrixWireError, match=message):
        load_session_input(path)


def test_stdio_broker_uses_strict_frames_and_advances_challenge() -> None:
    """The client must preserve one attachment, sequence, and fresh challenge."""
    challenge = "1" * 64
    next_challenge = "2" * 64
    inbound_read, inbound_write = os.pipe()
    outbound_read, outbound_write = os.pipe()
    close_peer = threading.Event()

    def peer() -> None:
        os.write(
            inbound_write,
            canonical_json_bytes(
                {
                    "schema_version": 1,
                    "type": "challenge",
                    "session_id": SESSION_ID,
                    "sequence": 0,
                    "challenge": challenge,
                }
            )
            + b"\n",
        )
        request = json.loads(os.read(outbound_read, 4096))
        assert request["op"] == "verify"
        assert request["sequence"] == 1
        os.write(
            inbound_write,
            canonical_json_bytes(
                {
                    "schema_version": 1,
                    "type": "reply",
                    "session_id": SESSION_ID,
                    "sequence": 1,
                    "challenge": next_challenge,
                    "result": {
                        "descriptor": {},
                        "proof": {},
                        "invitation": {},
                        "expired_invitation": {},
                        "events": [],
                    },
                }
            )
            + b"\n",
        )
        assert close_peer.wait(timeout=2)
        os.close(inbound_write)
        os.close(outbound_read)

    thread = threading.Thread(target=peer)
    thread.start()
    broker = StdioBroker.attach(
        os.dup(inbound_read),
        os.dup(outbound_write),
        {
            "schema_version": 1,
            "kind": "installed-host",
            "broker_socket": "/unopened/root/socket",
            "session_id": SESSION_ID,
            "registration_sha256": SHA,
        },
    )
    result = broker.request("verify")
    broker.assert_attached()
    close_peer.set()
    thread.join(timeout=2)
    with pytest.raises(MatrixWireError, match="closed or sent data"):
        broker.assert_attached()
    os.close(inbound_read)
    os.close(outbound_write)

    assert set(result) == {
        "descriptor",
        "proof",
        "invitation",
        "expired_invitation",
        "events",
    }
    assert not thread.is_alive()


def test_session_id_must_be_canonical_uuid_v4(tmp_path: Path) -> None:
    """A stale/non-v4 session identity must never attach to a matrix cell."""
    path, value = _session(tmp_path)
    value["session_id"] = str(uuid.uuid1())
    _write_private(path, value)

    with pytest.raises(MatrixWireError, match="session_id"):
        load_session_input(path)


def test_coordination_barrier_binds_ready_ack_and_result(tmp_path: Path) -> None:
    """A barrier must accept only the matching immutable ready exchange."""
    coordination = tmp_path / "coordination"
    evidence = tmp_path / "initial.json"
    result = tmp_path / "advance-result.json"
    _write_private(evidence, {"initial_battery": 0})
    _write_private(result, {"updated": True})
    channel = CoordinationChannel(
        coordination,
        session_id=SESSION_ID,
        cell_id="home_assistant__debian13_arm64",
        session_input_sha256=SHA,
        instance_nonce="b" * 64,
        cleanup_timeout_ms=45_000,
    )

    def acknowledge() -> None:
        ready_path = coordination / "ready-000001.json"
        while not ready_path.exists():
            time.sleep(0.01)
        ready_sha256 = hashlib.sha256(ready_path.read_bytes()).hexdigest()
        ack = {
            "schema_version": 1,
            "type": "ack",
            "session_id": SESSION_ID,
            "cell_id": "home_assistant__debian13_arm64",
            "session_input_sha256": SHA,
            "instance_nonce": "b" * 64,
            "sequence": 1,
            "ready_sha256": ready_sha256,
            "phase": "ha_initial_observation",
            "status": "accepted",
            "action": "advance_once",
            "result": file_binding(result),
        }
        _write_private(coordination / "ack-000001.json", ack)

    import time

    thread = threading.Thread(target=acknowledge)
    thread.start()
    ack = channel.barrier(
        phase="ha_initial_observation",
        observation={"session_sequence": 1, "proof_sha256": "c" * 64},
        evidence=file_binding(evidence),
    )
    thread.join(timeout=2)

    assert ack["action"] == "advance_once"
    assert channel.read_ack_result(ack) == {"updated": True}
    assert not thread.is_alive()


def test_coordination_rejects_changed_evidence_after_ready(tmp_path: Path) -> None:
    """Mutating a raw witness after readiness must poison completion."""
    coordination = tmp_path / "coordination"
    evidence = tmp_path / "evidence.json"
    _write_private(evidence, {"value": 1})
    channel = CoordinationChannel(
        coordination,
        session_id=SESSION_ID,
        cell_id="home_assistant__debian13_arm64",
        session_input_sha256=SHA,
        instance_nonce="b" * 64,
        cleanup_timeout_ms=45_000,
    )
    frozen = file_binding(evidence)
    _write_private(evidence, {"value": 2})

    with pytest.raises(MatrixWireError, match="changed after readiness"):
        channel.assert_frozen([frozen])


def test_coordination_rejects_wrong_final_ack(tmp_path: Path) -> None:
    """Final readiness accepts only close_completed for the exact ready file."""
    coordination = tmp_path / "coordination"
    evidence = tmp_path / "completion.json"
    result = tmp_path / "result.json"
    _write_private(evidence, {"complete": True})
    _write_private(result, {"closed": True})
    channel = CoordinationChannel(
        coordination,
        session_id=SESSION_ID,
        cell_id="home_assistant__debian13_arm64",
        session_input_sha256=SHA,
        instance_nonce="b" * 64,
        cleanup_timeout_ms=45_000,
    )

    def acknowledge_wrongly() -> None:
        ready_path = coordination / "ready-000001.json"
        while not ready_path.exists():
            time.sleep(0.01)
        _write_private(
            coordination / "ack-000001.json",
            {
                "schema_version": 1,
                "type": "ack",
                "session_id": SESSION_ID,
                "cell_id": "home_assistant__debian13_arm64",
                "session_input_sha256": SHA,
                "instance_nonce": "b" * 64,
                "sequence": 1,
                "ready_sha256": hashlib.sha256(ready_path.read_bytes()).hexdigest(),
                "phase": "evidence_ready",
                "status": "accepted",
                "action": "advance_once",
                "result": file_binding(result),
            },
        )

    thread = threading.Thread(target=acknowledge_wrongly)
    thread.start()
    with pytest.raises(MatrixWireError, match="accepted coordination ack"):
        channel.barrier(
            phase="evidence_ready",
            observation={"session_sequence": 3, "proof_sha256": "c" * 64},
            evidence=file_binding(evidence),
        )
    thread.join(timeout=2)
    assert not thread.is_alive()


HUB_ID = "44444444-4444-4444-8444-444444444444"
UNKNOWN_VEHICLE = "33333333-3333-4333-8333-333333333333"
UNICODE_NAME = "Interop \N{EN DASH} Árvíztűrő 🚗"
ACTIVE_PAIRING_ID = "55555555-5555-4555-8555-555555555555"
EXPIRED_PAIRING_ID = "66666666-6666-4666-8666-666666666666"
FRESH_PAIRING_ID = "77777777-7777-4777-8777-777777777777"
DEVICE_ID = "88888888-8888-4888-8888-888888888888"
FLOW_MANIFEST = {"path": "/inputs/ha-flow.json", "sha256": "e" * 64}
CLIENT_MANIFEST = {"path": "/inputs/ha-client.json", "sha256": "f" * 64}
INSTALLED_MEMBER_PATHS = (
    "__init__.py",
    "client.py",
    "config_flow.py",
    "const.py",
    "coordinator.py",
    "current_hub_client.py",
    "diagnostics.py",
    "entity.py",
    "manifest.json",
    "models.py",
    "profile/hub-http-v1/1.0.0/SHA256SUMS",
    "profile/hub-http-v1/1.0.0/auth.schema.json",
    "profile/hub-http-v1/1.0.0/cases.json",
    "profile/hub-http-v1/1.0.0/discovery.schema.json",
    "profile/hub-http-v1/1.0.0/errors.schema.json",
    "profile/hub-http-v1/1.0.0/examples/claim.json",
    "profile/hub-http-v1/1.0.0/examples/current.json",
    "profile/hub-http-v1/1.0.0/examples/discovery.json",
    "profile/hub-http-v1/1.0.0/examples/drives.json",
    "profile/hub-http-v1/1.0.0/examples/health.json",
    "profile/hub-http-v1/1.0.0/examples/invitation.json",
    "profile/hub-http-v1/1.0.0/examples/ready.json",
    "profile/hub-http-v1/1.0.0/examples/vehicles.json",
    "profile/hub-http-v1/1.0.0/field-semantics.json",
    "profile/hub-http-v1/1.0.0/openapi.json",
    "profile/hub-http-v1/1.0.0/profile.json",
    "profile/hub-http-v1/1.0.0/resources.schema.json",
    "profile/hub-http-v1/1.0.0/sync-regression.json",
    "sensor.py",
    "strings.json",
    "translations/en.json",
)
INSTALLED_MEMBERS = [
    {
        "path": path,
        "bytes": index,
        "mode": 0o644,
        "sha256": f"{index + 1:064x}",
    }
    for index, path in enumerate(INSTALLED_MEMBER_PATHS)
]
INITIALIZER_SHA256 = INSTALLED_MEMBERS[0]["sha256"]

CASE_FACTS = {
    "candidate_artifact_identity": {
        "archive_sha256": "d" * 64,
        "integration_version": "2026.36.2",
        "installed_manifest_sha256": "9" * 64,
        "installed_members": 31,
    },
    "installed_service_runtime": {
        "service_mode": "installed-deb-systemd",
        "status": "pending",
    },
    "discovery_identity_profile": {
        "hub_id": HUB_ID,
        "api_versions": ["1.0"],
        "protocol": "teslatlas-sync",
        "protocol_major": 1,
        "version": "2026.36.2",
    },
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
        "home_assistant_version": "2026.8.3",
        "python_version": "3.14.2",
        "integration_version": "2026.36.2",
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

CASE_BINDINGS = {
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


def _request(identifier: str, method: str, route: str, status: int) -> dict:
    return {
        "method": method,
        "route": route,
        "status": status,
        "request_id": identifier,
    }


OPERATION_REQUESTS = {
    "observe_installed_runtime": [],
    "probe": [_request("probe-1", "GET", "/.well-known/teslatlas-hub", 200)],
    "bad_pair": [
        _request("bad-0", "GET", "/.well-known/teslatlas-hub", 200),
        _request("bad-1", "POST", "/v1/pairings/{pairing_id}/claim", 401),
    ],
    "expired_pair": [
        _request("expired-0", "GET", "/.well-known/teslatlas-hub", 200),
        _request("expired-1", "POST", "/v1/pairings/{pairing_id}/claim", 401),
    ],
    "replay_pair": [
        _request("replay-0", "GET", "/.well-known/teslatlas-hub", 200),
        _request("replay-1", "POST", "/v1/pairings/{pairing_id}/claim", 401),
    ],
    "config_flow_setup": [
        _request("setup-1", "GET", "/.well-known/teslatlas-hub", 200),
        _request("setup-2", "GET", "/.well-known/teslatlas-hub", 200),
        _request("setup-3", "POST", "/v1/pairings/{pairing_id}/claim", 200),
        _request("setup-4", "GET", "/.well-known/teslatlas-hub", 200),
        _request("setup-5", "GET", "/v1/vehicles", 200),
        _request("setup-6", "GET", "/v1/vehicles/{vehicle_id}/current", 200),
        _request("setup-7", "GET", "/v1/vehicles/{vehicle_id}/current", 200),
    ],
    "unknown_current": [
        _request("unknown-1", "GET", "/v1/vehicles/{vehicle_id}/current", 404)
    ],
    "initial_poll": [],
    "later_poll": [
        _request("later-0", "GET", "/.well-known/teslatlas-hub", 200),
        _request("later-1", "GET", "/v1/vehicles", 200),
        _request("later-2", "GET", "/v1/vehicles/{vehicle_id}/current", 200),
        _request("later-3", "GET", "/v1/vehicles/{vehicle_id}/current", 200),
    ],
    "unsupported_surface": [],
    "revocation_refresh": [
        _request("revoked-0", "GET", "/.well-known/teslatlas-hub", 200),
        _request("revoked-1", "GET", "/v1/vehicles", 401),
    ],
    "reauthentication_flow": [
        _request("reauth-0", "GET", "/.well-known/teslatlas-hub", 200),
        _request("reauth-1", "POST", "/v1/pairings/{pairing_id}/claim", 200),
        _request("reauth-2", "GET", "/.well-known/teslatlas-hub", 200),
        _request("reauth-3", "GET", "/v1/vehicles", 200),
        _request("reauth-4", "GET", "/v1/vehicles/{vehicle_id}/current", 200),
        _request("reauth-5", "GET", "/v1/vehicles/{vehicle_id}/current", 200),
    ],
    "endpoint_restart_poll": [
        _request("restart-0", "GET", "/.well-known/teslatlas-hub", 200),
        _request("restart-1", "GET", "/v1/vehicles", 200),
        _request("restart-2", "GET", "/v1/vehicles/{vehicle_id}/current", 200),
        _request("restart-3", "GET", "/v1/vehicles/{vehicle_id}/current", 200),
    ],
    "outage_poll": [
        _request("outage-0", "GET", "/.well-known/teslatlas-hub", 200),
        _request("outage-1", "GET", "/v1/vehicles", 200),
        _request("outage-2", "GET", "/v1/vehicles/{vehicle_id}/current", 200),
        _request("outage-3", "GET", "/v1/vehicles/{vehicle_id}/current", 200),
    ],
    "unload": [],
}

OPERATION_FACTS = {
    "observe_installed_runtime": {
        **CASE_FACTS["candidate_artifact_identity"],
        **CASE_FACTS["installed_home_assistant_runtime"],
        "service_mode": "installed-deb-systemd",
        "hub_id": HUB_ID,
        "actor_manifest_sha256s": {"ha_flow": "e" * 64, "ha_client": "f" * 64},
        "expired_fixture": {
            "fixture_expires_at_ms": 1,
            "observed_after_expiry_ms": 2,
            "fixture_kind": "persisted-one-second",
            "pairing_id": EXPIRED_PAIRING_ID,
        },
        "entry_id": "entry-1",
        "registry_ids": [f"sensor.entity_{index}" for index in range(26)],
        "python_executable": "/work/ha/.venv/bin/python",
        "module_path": "__init__.py",
        "module_sha256": INITIALIZER_SHA256,
        "loaded_modules": {
            "custom_components.teslatlas_hub": "__init__.py",
            "custom_components.teslatlas_hub.client": "client.py",
            "custom_components.teslatlas_hub.config_flow": "config_flow.py",
            "custom_components.teslatlas_hub.const": "const.py",
            "custom_components.teslatlas_hub.coordinator": "coordinator.py",
            "custom_components.teslatlas_hub.current_hub_client": (
                "current_hub_client.py"
            ),
            "custom_components.teslatlas_hub.entity": "entity.py",
            "custom_components.teslatlas_hub.models": "models.py",
            "custom_components.teslatlas_hub.sensor": "sensor.py",
        },
        "manifest_domain": "teslatlas_hub",
        "config_entry_loaded": True,
        "entity_registry_count": 26,
    },
    "probe": {
        **CASE_FACTS["discovery_identity_profile"],
        **CASE_FACTS["unauthenticated_discovery"],
    },
    "bad_pair": {**CASE_FACTS["bad_invitation"], "pairing_id": ACTIVE_PAIRING_ID},
    "expired_pair": {
        **CASE_FACTS["expired_invitation"],
        "fixture_expires_at_ms": 1,
        "observed_after_expiry_ms": 2,
        "fixture_kind": "persisted-one-second",
        "pairing_id": EXPIRED_PAIRING_ID,
    },
    "replay_pair": {
        **CASE_FACTS["replayed_invitation"],
        "pairing_id": ACTIVE_PAIRING_ID,
    },
    "config_flow_setup": {
        **CASE_FACTS["real_auth"],
        "entry_id": "entry-1",
        "registry_ids": [f"sensor.entity_{index}" for index in range(26)],
    },
    "unknown_current": {
        **CASE_FACTS["unknown_vehicle"],
        "optional_fields": {
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
        },
    },
    "initial_poll": {
        "vehicles": 2,
        "sensors_per_vehicle": 13,
        "initial_battery": 0,
        "initial_inside_temperature_c": 21.5,
        "empty_state_of_charge": None,
        "unicode_name": UNICODE_NAME,
    },
    "later_poll": {
        "later_battery": 1,
        "later_inside_temperature_c": 22.5,
        "advanced_once": True,
        "advance_from_sequence": 1,
    },
    "unsupported_surface": CASE_FACTS["unsupported_operation_zero_requests"],
    "revocation_refresh": {
        **CASE_FACTS["revocation"],
        "credential_loss_observed": True,
        "device_id": DEVICE_ID,
    },
    "reauthentication_flow": {
        **CASE_FACTS["credential_lifecycle_reauth"],
        "reauth_completed": True,
        "pairing_id": FRESH_PAIRING_ID,
    },
    "endpoint_restart_poll": {
        **CASE_FACTS["endpoint_restart"],
        "before_service_generation": "generation-3",
        "after_service_generation": "generation-4",
    },
    "outage_poll": {
        **CASE_FACTS["outage_recovery"],
        "before_service_generation": "generation-4",
        "after_service_generation": "generation-5",
    },
    "unload": {
        **CASE_FACTS["polling_transport_zero_sse"],
        "attempted_requests": 35,
        "completed_requests": 35,
        "failed_requests": 0,
        "cancelled_requests": 0,
        "pending_requests": 0,
        "post_unload_attempts": 0,
        "scheduled_callback_deadline_ns": 1_000_000,
        "observed_after_deadline_ns": 1_000_001,
        "scheduled_callback_cancelled": True,
        "attempts": [
            {
                "method": request["method"],
                "route": request["route"],
                "outcome": "completed",
                "phase": "before_unload",
            }
            for requests in OPERATION_REQUESTS.values()
            for request in requests
        ],
    },
}


def _controller_observations() -> dict[int, MappingProxyType]:
    def observation(
        sequence: int,
        operation: str,
        generation: str,
        transition: dict | None,
        active_pairing: str = ACTIVE_PAIRING_ID,
    ) -> MappingProxyType:
        return MappingProxyType(
            {
                "schema_version": 1,
                "state": "running",
                "session_id": SESSION_ID,
                "sequence": sequence,
                "operation": operation,
                "started_monotonic_ns": sequence * 10,
                "finished_monotonic_ns": sequence * 10 + 1,
                "observed_at_ms": 10,
                "proof_sha256": f"{sequence:x}" * 64,
                "result_sha256": f"{sequence:x}" * 64,
                "scenario_sha256": "a" * 64,
                "seed_sha256": "b" * 64,
                "store_id": HUB_ID,
                "store_schema_version": 4,
                "service_generation": generation,
                "hub_id": HUB_ID,
                "invitations": {
                    "active": {
                        "pairing_id": active_pairing,
                        "expires_at_ms": 1000,
                    },
                    "expired": {
                        "pairing_id": EXPIRED_PAIRING_ID,
                        "expires_at_ms": 1,
                    },
                },
                "transition": transition,
            }
        )

    def stopped(sequence: int, from_sequence: int, generation: str) -> MappingProxyType:
        prior = observation(from_sequence, "verify", generation, None)
        value = dict(prior)
        value.update(
            sequence=sequence,
            operation="stop",
            state="stopped",
            started_monotonic_ns=sequence * 10,
            finished_monotonic_ns=sequence * 10 + 1,
            result_sha256=f"{sequence:x}" * 64,
            proof_sha256=None,
            invitations=None,
            transition={"kind": "stop", "from_sequence": from_sequence},
        )
        return MappingProxyType(value)

    return {
        1: observation(1, "verify", "generation-1", None),
        2: observation(2, "verify", "generation-1", None),
        3: observation(
            3,
            "advance-once",
            "generation-2",
            {
                "kind": "advance-once",
                "from_sequence": 1,
                "pre_advance_verify_sequence": 2,
                "before_store_sha256": "c" * 64,
                "after_store_sha256": "d" * 64,
                "scenario_sha256": "a" * 64,
                "seed_sha256": "b" * 64,
            },
        ),
        4: observation(
            4,
            "revoke",
            "generation-2",
            {"kind": "revoke", "from_sequence": 3, "device_id": DEVICE_ID},
        ),
        5: observation(
            5,
            "pair",
            "generation-3",
            {"kind": "pair", "from_sequence": 4},
            FRESH_PAIRING_ID,
        ),
        6: stopped(6, 5, "generation-3"),
        7: observation(
            7,
            "start",
            "generation-4",
            {"kind": "start", "from_sequence": 6, "stopped_sequence": 6},
            FRESH_PAIRING_ID,
        ),
        8: stopped(8, 7, "generation-4"),
        9: observation(
            9,
            "start",
            "generation-5",
            {"kind": "start", "from_sequence": 8, "stopped_sequence": 8},
            FRESH_PAIRING_ID,
        ),
    }


def _context() -> tuple[AdmissionContext, dict[str, dict]]:
    sequence_for = {
        "observe_installed_runtime": (1, 1),
        "probe": (1, 1),
        "bad_pair": (1, 1),
        "expired_pair": (1, 1),
        "replay_pair": (1, 1),
        "config_flow_setup": (1, 1),
        "unknown_current": (1, 1),
        "initial_poll": (1, 1),
        "later_poll": (3, 3),
        "unsupported_surface": (3, 3),
        "revocation_refresh": (3, 4),
        "reauthentication_flow": (4, 5),
        "endpoint_restart_poll": (5, 7),
        "outage_poll": (7, 9),
        "unload": (9, 9),
    }
    def actor_runtime(manifest_sha256: str) -> MappingProxyType:
        return MappingProxyType(
            {
                "schema_version": 1,
                "runtime_ref": "ha_container",
                "runtime_kind": "docker-container",
                "container_id": "2" * 64,
                "python": {
                    "executable": "/work/ha/.venv/bin/python",
                    "sha256": "3" * 64,
                    "version": "3.14.2",
                },
                "home_assistant": {"version": "2026.8.3"},
                "artifact": {
                    "role": "home_assistant_integration_archive",
                    "sha256": "d" * 64,
                    "installed_root": "/work/product/custom_components/teslatlas_hub",
                    "installed_manifest_sha256": "9" * 64,
                    "installed_members": copy.deepcopy(INSTALLED_MEMBERS),
                },
                "actor_input_manifest_sha256": manifest_sha256,
            }
        )

    actors = {
        "ha_flow": AdmittedActor(
            id="ha_flow",
            kind="installed_ha_flow",
            runtime_ref="ha_container",
            entrypoint_ref="ha_matrix_flow",
            artifact_roles=("home_assistant_integration_archive",),
            source_roles=("home_assistant_source",),
            installed_manifest=MappingProxyType(FLOW_MANIFEST),
            runtime=actor_runtime("e" * 64),
        ),
        "ha_client": AdmittedActor(
            id="ha_client",
            kind="installed_ha_client",
            runtime_ref="ha_container",
            entrypoint_ref="ha_matrix_client",
            artifact_roles=("home_assistant_integration_archive",),
            source_roles=("home_assistant_source",),
            installed_manifest=MappingProxyType(CLIENT_MANIFEST),
            runtime=actor_runtime("f" * 64),
        ),
    }
    raw = {}
    for operation, requests in OPERATION_REQUESTS.items():
        actor_id = (
            "ha_client"
            if operation
            in {
                "probe",
                "bad_pair",
                "expired_pair",
                "replay_pair",
                "unknown_current",
                "unsupported_surface",
            }
            else "ha_flow"
        )
        before, after = sequence_for[operation]
        evidence_id = f"raw-{operation}"
        raw[evidence_id] = {
            "schema_version": 1,
            "session_id": SESSION_ID,
            "cell_id": "home_assistant__debian13_arm64",
            "session_input_sha256": SHA,
            "actor_id": actor_id,
            "operation": operation,
            "session_sequence_before": before,
            "session_sequence_after": after,
            "facts": copy.deepcopy(OPERATION_FACTS[operation]),
            "requests": copy.deepcopy(requests),
        }
    invocations = []
    for case_id, (actor_id, operations) in CASE_BINDINGS.items():
        for operation in operations:
            before, after = sequence_for[operation]
            invocations.append(
                AdmittedInvocation(
                    id=f"invoke-{case_id}-{operation}",
                    case_id=case_id,
                    actor_id=actor_id,
                    operation=operation,
                    session_sequence_before=before,
                    session_sequence_after=after,
                    evidence_id=f"raw-{operation}",
                    request_ids=tuple(
                        item["request_id"] for item in OPERATION_REQUESTS[operation]
                    ),
                )
            )
    context = AdmissionContext(
        adapter_id="home_assistant",
        cell_id="home_assistant__debian13_arm64",
        session_id=SESSION_ID,
        header=MappingProxyType(
            {
                "product_version": "2026.36.2",
                "artifacts": [
                    {
                        "role": "home_assistant_integration_archive",
                        "sha256": "d" * 64,
                    }
                ],
            }
        ),
        scenario=MappingProxyType(
            {
                "schema_version": 1,
                "name": "two-vehicles-five-drives",
                "provenance": "synthetic-only",
                "observed_at_ms": 1788566400000,
                "vehicle_ids": [
                    "11111111-1111-4111-8111-111111111111",
                    "22222222-2222-4222-8222-222222222222",
                ],
                "vehicles": [
                    {
                        "vehicle_id": "11111111-1111-4111-8111-111111111111",
                        "display_name": UNICODE_NAME,
                    },
                    {
                        "vehicle_id": "22222222-2222-4222-8222-222222222222",
                        "display_name": "Interop empty",
                    },
                ],
                "empty_vehicle_observed_at_ms": None,
                "current": {
                    "battery_level": 0,
                    "inside_temp": 21.5,
                    "outside_temp": None,
                    "observed_at_ms": 1788566400000,
                },
                "later_current": {
                    "observed_at_ms": 1788566460000,
                    "battery_level": 1,
                    "inside_temp": 22.5,
                    "outside_temp": None,
                },
            }
        ),
        actors=MappingProxyType(actors),
        invocations=tuple(invocations),
        raw=MappingProxyType(raw),
        controller_observations=MappingProxyType(_controller_observations()),
    )
    return context, raw


def _case(case_id: str, context: AdmissionContext) -> dict:
    _actor, operations = CASE_BINDINGS[case_id]
    requests = []
    for operation in operations:
        requests.extend(OPERATION_REQUESTS[operation])
    status = "pending" if case_id == "installed_service_runtime" else "passed"
    return {
        "id": case_id,
        "status": status,
        "expected": CASE_FACTS[case_id],
        "actual": CASE_FACTS[case_id],
        "evidence_kind": (
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
        ),
        "request_transcript": requests,
    }


def _advance_admission_context(
    post_advance_generation: str,
) -> tuple[AdmissionContext, dict[str, dict]]:
    context, raw = _context()

    def remap(sequence: int, source_sequence: int) -> MappingProxyType:
        value = dict(context.controller_observations[source_sequence])
        value.update(
            sequence=sequence,
            started_monotonic_ns=sequence * 10,
            finished_monotonic_ns=sequence * 10 + 1,
            proof_sha256=hashlib.sha256(f"proof-{sequence}".encode()).hexdigest(),
            result_sha256=hashlib.sha256(f"result-{sequence}".encode()).hexdigest(),
        )
        return MappingProxyType(value)

    predecessor = remap(41, 1)
    pre_advance_verify = remap(42, 2)
    advance = dict(remap(43, 3))
    advance["service_generation"] = post_advance_generation
    advance["transition"] = {
        "kind": "advance-once",
        "from_sequence": 41,
        "pre_advance_verify_sequence": 42,
        "before_store_sha256": "c" * 64,
        "after_store_sha256": "d" * 64,
        "scenario_sha256": "a" * 64,
        "seed_sha256": "b" * 64,
    }
    for operation, sequence in (("initial_poll", 41), ("later_poll", 43)):
        raw[f"raw-{operation}"]["session_sequence_before"] = sequence
        raw[f"raw-{operation}"]["session_sequence_after"] = sequence
    raw["raw-later_poll"]["facts"]["advance_from_sequence"] = 41
    observations = dict(context.controller_observations)
    observations.update(
        {
            41: predecessor,
            42: pre_advance_verify,
            43: MappingProxyType(advance),
        }
    )
    invocations = tuple(
        replace(
            invocation,
            session_sequence_before=(
                41 if invocation.operation == "initial_poll" else 43
            ),
            session_sequence_after=(
                41 if invocation.operation == "initial_poll" else 43
            ),
        )
        if invocation.operation in {"initial_poll", "later_poll"}
        else invocation
        for invocation in context.invocations
    )
    return (
        replace(
            context,
            invocations=invocations,
            raw=MappingProxyType(raw),
            controller_observations=MappingProxyType(observations),
        ),
        raw,
    )


def test_real_projection_admits_only_changed_advance_generation() -> None:
    """An authentic advance must move G1/G1 to G2 while identity stays stable."""
    context, raw = _advance_admission_context("generation-2")
    runtime = object.__new__(MatrixRuntime)
    runtime.raw = raw
    runtime.case_facts = None
    runtime.set_case_facts_from_raw()
    admitted_case = _case("exact_current_values", context)
    admitted_case["expected"] = runtime.case_facts["exact_current_values"]
    admitted_case["actual"] = runtime.case_facts["exact_current_values"]

    admitted = admit_case(admitted_case, context)

    unchanged_context, unchanged_raw = _advance_admission_context("generation-1")
    unchanged_runtime = object.__new__(MatrixRuntime)
    unchanged_runtime.raw = unchanged_raw
    unchanged_runtime.case_facts = None
    unchanged_runtime.set_case_facts_from_raw()
    unchanged_case = _case("exact_current_values", unchanged_context)
    unchanged_case["expected"] = unchanged_runtime.case_facts["exact_current_values"]
    unchanged_case["actual"] = unchanged_runtime.case_facts["exact_current_values"]
    unchanged = admit_case(unchanged_case, unchanged_context)

    assert admitted.status == "passed"
    assert admitted.code == "accepted"
    assert unchanged.status == "failed"
    assert unchanged.code == "raw_fact_mismatch"


def test_advance_admission_is_pending_without_post_advance_observation() -> None:
    """Missing root post-advance evidence remains a distinct pending context."""
    context, _raw = _advance_admission_context("generation-2")
    observations = dict(context.controller_observations)
    observations.pop(43)
    pending_context = replace(
        context, controller_observations=MappingProxyType(observations)
    )

    decision = admit_case(
        _case("exact_current_values", pending_context), pending_context
    )

    assert decision.status == "pending"
    assert decision.code == "independent_observation_pending"


@pytest.mark.parametrize(
    ("post_advance_generation", "status"),
    [("generation-2", "passed"), ("generation-1", "failed")],
)
def test_polling_transport_admission_requires_changed_advance_generation(
    post_advance_generation: str,
    status: str,
) -> None:
    """Apply the G1/G1/G2 rule to the later-poll transport case as well."""
    context, _raw = _advance_admission_context(post_advance_generation)

    decision = admit_case(_case("polling_transport_zero_sse", context), context)

    assert decision.status == status


def test_contract_admits_all_18_ha_cases_from_independent_literals() -> None:
    """Removing or misbinding any declared HA behavior must fail admission."""
    context, _raw = _context()

    decisions = [admit_case(_case(case_id, context), context) for case_id in CASE_FACTS]

    assert ADAPTER_ID == "home_assistant"
    assert CONTRACT_REVISION == 1
    assert [decision.status for decision in decisions] == [
        "pending" if case_id == "installed_service_runtime" else "passed"
        for case_id in CASE_FACTS
    ]


def test_manifest_binds_validator_schemas_actors_and_all_cases() -> None:
    """The inert publication manifest must bind only the reviewed local bytes."""
    tools_root = Path(__file__).parents[1] / "tools"
    manifest = json.loads((tools_root / "matrix-contract.json").read_text())

    assert manifest["schema_version"] == 1
    assert manifest["adapter_id"] == ADAPTER_ID
    assert manifest["revision"] == CONTRACT_REVISION
    assert manifest["required_cases"] == list(REQUIRED_CASES)
    assert manifest["actors"] == [
        {"id": "ha_flow", "kind": "installed_ha_flow", "required": True},
        {"id": "ha_client", "kind": "installed_ha_client", "required": True},
    ]
    assert manifest["phases"] == []
    assert [item["id"] for item in manifest["cases"]] == list(REQUIRED_CASES)
    for item in manifest["cases"]:
        actor_id, operations = CONTRACT_CASE_BINDINGS[item["id"]]
        assert item["actor_ids"] == [actor_id]
        assert item["operations"] == list(operations)

    bindings = [
        item["schema"] for item in manifest["raw_schemas"]
    ] + [manifest["validator"]]
    for binding in bindings:
        path = Path(binding["path"])
        assert path.is_absolute() and path.is_file()
        assert hashlib.sha256(path.read_bytes()).hexdigest() == binding["sha256"]


def test_raw_schemas_are_valid_json_and_close_every_declared_object() -> None:
    """Every schema-defined object rejects unspecified fields."""
    tools_root = Path(__file__).parents[1] / "tools"

    def inspect(value: object) -> None:
        if isinstance(value, dict):
            if value.get("type") == "object":
                assert value.get("additionalProperties") is False
            for child in value.values():
                inspect(child)
        elif isinstance(value, list):
            for child in value:
                inspect(child)

    schemas = sorted(tools_root.glob("ha-*-v1.schema.json"))
    assert [path.name for path in schemas] == [
        "ha-client-v1.schema.json",
        "ha-flow-v1.schema.json",
        "ha-initial-v1.schema.json",
        "ha-runtime-v1.schema.json",
    ]
    for path in schemas:
        schema = json.loads(path.read_text())
        assert schema["$schema"] == "https://json-schema.org/draft/2020-12/schema"
        inspect(schema)


def test_framework_log_isolates_all_standard_descriptors(tmp_path: Path) -> None:
    """The subprocess must not leave pytest output or input on broker stdio."""
    log_path = tmp_path / "framework.log"
    program = "\n".join(
        (
            "import os, sys",
            "from tools.matrix_live import FrameworkLog",
            "log = FrameworkLog(sys.argv[1], 1024)",
            "assert os.read(0, 1) == b''",
            "os.write(1, b'framework-only\\n')",
            "os.write(2, b'stderr-only\\n')",
            "log.finish()",
            "assert not log.overflowed",
        )
    )

    completed = subprocess.run(
        [sys.executable, "-c", program, str(log_path)],
        cwd=Path(__file__).parents[1],
        input=b"broker-input-must-not-reach-framework",
        capture_output=True,
        check=False,
    )

    assert completed.returncode == 0
    assert completed.stdout == b""
    assert completed.stderr == b""
    assert log_path.read_bytes() == b"framework-only\nstderr-only\n"


def test_fixed_launcher_owns_asyncio_fixture_configuration() -> None:
    """The isolated five-file harness cannot inherit repository pytest config."""
    arguments = _pytest_arguments(Path("/isolated/harness"))

    assert arguments == [
        "-q",
        "--no-showlocals",
        "-o",
        "asyncio_mode=auto",
        "/isolated/harness/tests/integration/test_live_hub.py"
        "::test_installed_matrix_all_cases_against_real_hub",
    ]
    assert sys.dont_write_bytecode is True


def test_failed_or_overflowed_framework_blocks_finalization() -> None:
    """The main seam rejects a failed pytest run before completion evidence."""
    with pytest.raises(MatrixWireError, match="framework execution failed"):
        _require_framework_success(1, 0, False)
    with pytest.raises(MatrixWireError, match="framework execution failed"):
        _require_framework_success(0, 0, True)


def test_secret_rotation_failure_never_enters_rewritten_pytest_stream(
    tmp_path: Path,
) -> None:
    """A sentinel bearer remains absent from the actual retained failure log."""
    sentinel = "sentinel-bearer-must-never-appear"
    probe = tmp_path / "test_secret_probe.py"
    probe.write_text(
        "import os\n"
        "from tools.matrix_live import MatrixRuntime\n"
        "def test_secret_rotation():\n"
        "    credential = os.environ['MATRIX_SENTINEL']\n"
        "    MatrixRuntime.require_secret_changed(credential, credential)\n"
    )
    log_path = tmp_path / "framework-secret.log"
    program = "\n".join(
        (
            "import pytest, sys",
            "from tools.matrix_live import FrameworkLog",
            "log = FrameworkLog(sys.argv[1], 65536)",
            "code = pytest.main(['-q', '--no-showlocals', '-o', "
            "'asyncio_mode=auto', sys.argv[2]])",
            "log.finish()",
            "raise SystemExit(0 if code == pytest.ExitCode.TESTS_FAILED else 3)",
        )
    )
    environment = dict(os.environ, MATRIX_SENTINEL=sentinel)
    completed = subprocess.run(
        [sys.executable, "-c", program, str(log_path), str(probe)],
        cwd=Path(__file__).parents[1],
        env=environment,
        capture_output=True,
        check=False,
    )

    assert completed.returncode == 0
    assert sentinel.encode() not in completed.stdout
    assert sentinel.encode() not in completed.stderr
    framework_bytes = log_path.read_bytes()
    assert sentinel.encode() not in framework_bytes
    assert b"Home Assistant credential did not rotate" in framework_bytes


def test_running_admission_accepts_nonliteral_initial_guest_sequence() -> None:
    """The root may verify before attachment, so the first HA proof may exceed one."""
    runtime = object.__new__(MatrixRuntime)
    runtime.session = {
        "session_id": SESSION_ID,
        "inputs": {
            "certificate_der_sha256": "c" * 64,
            "scenario": {"local": {"sha256": "a" * 64}},
        },
    }
    runtime.header = {"profile_sha256": "d" * 64}
    runtime.observations = {}
    runtime.sequence = None
    descriptor = {
        "status": "ready",
        "provenance": "installed-package-service",
        "endpoint": "https://localhost:18480",
        "hub_id": HUB_ID,
        "hub_pid": 42,
        "hub_started_at": "start",
        "service_generation": "generation-1",
        "binary_sha256": "1" * 64,
        "seed_binary_sha256": "b" * 64,
        "profile_id": "hub-http-v1@1.0.0",
        "profile_path": "/root/profile",
        "profile_sha256": "d" * 64,
        "scenario_path": "/root/scenario.json",
        "scenario_sha256": "a" * 64,
        "certificate_path": "/inputs/certificate.pem",
    }
    proof = {
        "status": "verified",
        "session_id": SESSION_ID,
        "sequence": 41,
        "config": {
            "scenario_sha256": "a" * 64,
            "seed_sha256": "b" * 64,
            "store_id": HUB_ID,
            "store_schema_version": 4,
        },
        "service": {"generation": "generation-1"},
        "discovery": {"hub_id": HUB_ID, "product_version": "2026.36.2"},
        "tls": {
            "endpoint": descriptor["endpoint"],
            "certificate_der_sha256": "c" * 64,
        },
    }
    result = {
        "descriptor": descriptor,
        "proof": proof,
        "invitation": {"pairingId": ACTIVE_PAIRING_ID, "expiresAtMs": 1000},
        "expired_invitation": {
            "pairingId": EXPIRED_PAIRING_ID,
            "expiresAtMs": 1,
        },
        "events": [],
    }

    runtime._admit_running(
        result,
        operation="verify",
        started_monotonic_ns=1,
        prior_sequence=None,
    )

    assert runtime.sequence == 41
    assert runtime.observations[41]["transition"] is None

    for invalid_sequence in (41, 40):
        invalid = copy.deepcopy(result)
        invalid["proof"]["sequence"] = invalid_sequence
        with pytest.raises(MatrixWireError, match="did not advance"):
            runtime._admit_running(
                invalid,
                operation="verify",
                started_monotonic_ns=2,
                prior_sequence=41,
            )
        assert runtime.sequence == 41
        assert tuple(runtime.observations) == (41,)

    advanced = copy.deepcopy(result)
    advanced["proof"]["sequence"] = 42
    runtime._admit_running(
        advanced,
        operation="verify",
        started_monotonic_ns=3,
        prior_sequence=41,
    )
    assert runtime.sequence == 42
    assert tuple(runtime.observations) == (41, 42)


@pytest.mark.asyncio
async def test_initial_barrier_does_not_invent_internal_verify_observation() -> None:
    """Only root can supply the controller's private pre-advance verify sequence."""
    result = {
        "proof": {
            "status": "verified",
            "session_id": SESSION_ID,
            "sequence": 43,
            "config": {
                "scenario_sha256": "a" * 64,
                "seed_sha256": "b" * 64,
                "store_id": HUB_ID,
                "store_schema_version": 4,
            },
            "service": {"generation": "generation-1"},
            "discovery": {"hub_id": HUB_ID},
        },
        "advance": {
            "before_store_sha256": "c" * 64,
            "after_store_sha256": "d" * 64,
            "scenario_sha256": "a" * 64,
            "seed_sha256": "b" * 64,
        },
        "invitation": {"pairingId": ACTIVE_PAIRING_ID, "expiresAtMs": 1000},
        "expired_invitation": {
            "pairingId": EXPIRED_PAIRING_ID,
            "expiresAtMs": 1,
        },
        "events": [],
    }

    class Channel:
        returned = copy.deepcopy(result)

        def barrier(self, **_kwargs):
            return {"result": "bound"}

        def read_ack_result(self, _ack):
            return self.returned

    runtime = object.__new__(MatrixRuntime)
    runtime.session = {"session_id": SESSION_ID}
    runtime.sequence = 41
    runtime.observations = {41: {"proof_sha256": "e" * 64}}
    runtime.raw_bindings = {"raw-initial_poll": {"path": "/raw", "sha256": SHA}}
    runtime.channel = Channel()

    runtime.channel.returned["advance"]["scenario_sha256"] = "0" * 64
    with pytest.raises(MatrixWireError, match="advance result identity"):
        await runtime.initial_barrier()
    assert runtime.sequence == 41

    runtime.channel.returned = result
    await runtime.initial_barrier()

    assert runtime.sequence == 43
    assert tuple(runtime.observations) == (41,)


def _installed_runtime(tmp_path: Path) -> MatrixRuntime:
    root = tmp_path / "product/custom_components/teslatlas_hub"
    root.mkdir(parents=True)
    module_paths = [
        "__init__.py",
        "client.py",
        "config_flow.py",
        "const.py",
        "coordinator.py",
        "current_hub_client.py",
        "entity.py",
        "models.py",
        "sensor.py",
    ] + [f"resource-{index:02d}.json" for index in range(22)]
    rows = []
    for relative in sorted(module_paths):
        path = root / relative
        path.write_text(f"# {relative}\n")
        raw = path.read_bytes()
        rows.append(
            {
                "path": relative,
                "bytes": len(raw),
                "mode": path.stat().st_mode & 0o777,
                "sha256": hashlib.sha256(raw).hexdigest(),
            }
        )
    runtime = object.__new__(MatrixRuntime)
    runtime.installed_root = root
    runtime.installed_manifest = {"files": rows}
    return runtime


def test_installed_inventory_rejects_valid_looking_cached_code(tmp_path: Path) -> None:
    """Unmanifested executable bytecode cannot hide beside valid source files."""
    runtime = _installed_runtime(tmp_path)
    cache = runtime.installed_root / "__pycache__"
    cache.mkdir()
    (cache / "client.cpython-314.pyc").write_bytes(b"valid-looking-bytecode")

    with pytest.raises(MatrixWireError, match="members differ"):
        runtime._validate_installed_members()


def test_loaded_submodule_origin_must_match_installed_inventory(tmp_path: Path) -> None:
    """Every executed integration module is bound, not only package __init__."""
    runtime = _installed_runtime(tmp_path)
    package = "custom_components.teslatlas_hub"
    suffixes = (
        "",
        ".client",
        ".config_flow",
        ".const",
        ".coordinator",
        ".current_hub_client",
        ".entity",
        ".models",
        ".sensor",
    )
    previous = {name: sys.modules.get(name) for name in (package + s for s in suffixes)}
    outside = tmp_path / "substituted-client.py"
    outside.write_text("# substituted\n")
    try:
        for suffix in suffixes:
            name = package + suffix
            module = types.ModuleType(name)
            relative = "__init__.py" if not suffix else suffix[1:] + ".py"
            module.__file__ = str(runtime.installed_root / relative)
            sys.modules[name] = module
        sys.modules[package + ".client"].__file__ = str(outside)

        with pytest.raises(MatrixWireError, match="outside installed root"):
            runtime.validate_loaded_modules()
    finally:
        for name, value in previous.items():
            if value is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = value


@pytest.mark.asyncio
async def test_request_census_retains_failed_pending_and_cancelled_dispatches() -> None:
    """SSE and unload dispatches count before an HTTP response can exist."""
    release = asyncio.Event()

    async def pending(_session, _method, _url, **_kwargs):
        await release.wait()

    census = _MatrixRequestCensus(pending)
    task = asyncio.create_task(
        census.request(None, "GET", "https://localhost/v1/events")
    )
    await asyncio.sleep(0)
    assert census.attempts[0]["outcome"] == "pending"
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert census.attempts[0]["outcome"] == "cancelled"

    async def failed(_session, _method, _url, **_kwargs):
        raise OSError("synthetic transport failure")

    census = _MatrixRequestCensus(failed)
    with pytest.raises(OSError, match="synthetic transport failure"):
        await census.request(None, "GET", "https://localhost/v1/events")
    assert census.attempts[0]["outcome"] == "failed"
    assert census.since(0) == []


@pytest.mark.asyncio
async def test_request_census_retains_failed_dispatch_after_exact_deadline() -> None:
    """A leaked cancelled callback remains visible after its actual loop deadline."""
    loop = asyncio.get_running_loop()
    attempted = asyncio.Event()

    async def failed(_session, _method, _url, **_kwargs):
        attempted.set()
        raise OSError("delayed synthetic transport failure")

    census = _MatrixRequestCensus(failed)
    mark = census.mark()
    deadline = loop.time() + 0.01

    def dispatch() -> None:
        task = loop.create_task(
            census.request(None, "GET", "https://localhost/v1/events")
        )
        task.add_done_callback(lambda completed: completed.exception())

    loop.call_at(deadline, dispatch)
    await asyncio.wait_for(attempted.wait(), timeout=1)
    await asyncio.sleep(0)

    assert loop.time() > deadline
    assert census.retained_attempts(mark, unload_mark=mark) == [
        {
            "method": "GET",
            "route": "/v1/events",
            "outcome": "failed",
            "phase": "after_unload",
        }
    ]


@pytest.mark.parametrize(
    ("mutation", "code"),
    [
        ("wrong_actor", "wrong_actor"),
        ("wrong_source", "wrong_source_role"),
        ("wrong_artifact", "wrong_artifact_role"),
        ("wrong_installed_manifest", "installed_manifest_mismatch"),
        ("unrelated_request", "request_mismatch"),
        ("stale_sequence", "sequence_mismatch"),
        ("changed_raw", "raw_fact_mismatch"),
        ("bad_null_zero", "literal_mismatch"),
        ("cleanup_failure", "cleanup_failure"),
    ],
)
def test_contract_rejects_identity_request_sequence_literal_and_cleanup_mutations(
    mutation: str, code: str
) -> None:
    """Each named mutation models a concrete broken installed-evidence boundary."""
    context, raw = _context()
    case_id = "exact_current_values"
    if mutation == "wrong_actor":
        case_id = "bad_invitation"
        context = replace(
            context,
            invocations=tuple(
                replace(item, actor_id="ha_flow")
                if item.case_id == case_id and item.operation == "bad_pair"
                else item
                for item in context.invocations
            ),
        )
    elif mutation == "wrong_source":
        actors = dict(context.actors)
        actors["ha_flow"] = replace(actors["ha_flow"], source_roles=("hub_source",))
        context = replace(context, actors=MappingProxyType(actors))
    elif mutation == "wrong_artifact":
        actors = dict(context.actors)
        actors["ha_flow"] = replace(
            actors["ha_flow"], artifact_roles=("hub_executable",)
        )
        context = replace(context, actors=MappingProxyType(actors))
    elif mutation == "wrong_installed_manifest":
        actors = dict(context.actors)
        actors["ha_flow"] = replace(
            actors["ha_flow"],
            installed_manifest=MappingProxyType(
                {"path": "/inputs/ha-flow.json", "sha256": "0" * 64}
            ),
        )
        context = replace(context, actors=MappingProxyType(actors))
    elif mutation == "unrelated_request":
        case_id = "bad_invitation"
        raw["raw-bad_pair"]["requests"][0]["route"] = "/healthz"
    elif mutation == "stale_sequence":
        raw["raw-later_poll"]["session_sequence_after"] = 2
    elif mutation == "changed_raw":
        raw["raw-later_poll"]["facts"]["later_battery"] = 2
    elif mutation == "bad_null_zero":
        case = _case(case_id, context)
        case["actual"] = dict(case["actual"], empty_state_of_charge=0)
        decision = admit_case(case, context)
        assert decision.code == code
        return
    elif mutation == "cleanup_failure":
        case_id = "polling_transport_zero_sse"
        raw["raw-unload"]["facts"]["clean_unload"] = False

    decision = admit_case(_case(case_id, context), context)

    assert decision.status == "failed"
    assert decision.code == code


@pytest.mark.parametrize(
    "mutation",
    [
        "replay_as_expiry",
        "edited_expiry",
        "wrong_expired_fixture",
        "unchanged_restart_generation",
        "unchanged_outage_generation",
        "later_poll_before_advance",
    ],
)
def test_contract_rejects_coherent_child_lifecycle_substitutions(
    mutation: str,
) -> None:
    """Child raw/invocation agreement cannot replace independent root facts."""
    context, raw = _context()
    case_id = "expired_invitation"
    if mutation in {"replay_as_expiry", "edited_expiry", "wrong_expired_fixture"}:
        fixture = raw["raw-observe_installed_runtime"]["facts"]["expired_fixture"]
        expired = raw["raw-expired_pair"]["facts"]
        if mutation == "replay_as_expiry":
            fixture.update(
                pairing_id=ACTIVE_PAIRING_ID,
                fixture_expires_at_ms=1000,
                observed_after_expiry_ms=2000,
            )
            expired.update(fixture)
        elif mutation == "edited_expiry":
            fixture["fixture_expires_at_ms"] = 2
            expired["fixture_expires_at_ms"] = 2
        else:
            wrong = "99999999-9999-4999-8999-999999999999"
            fixture["pairing_id"] = wrong
            expired["pairing_id"] = wrong
    elif mutation in {"unchanged_restart_generation", "unchanged_outage_generation"}:
        case_id = (
            "endpoint_restart"
            if mutation == "unchanged_restart_generation"
            else "outage_recovery"
        )
        observation_sequence = 7 if case_id == "endpoint_restart" else 9
        prior_generation = (
            "generation-3" if case_id == "endpoint_restart" else "generation-4"
        )
        observation = dict(context.controller_observations[observation_sequence])
        observation["service_generation"] = prior_generation
        observations = dict(context.controller_observations)
        observations[observation_sequence] = MappingProxyType(observation)
        context = replace(
            context, controller_observations=MappingProxyType(observations)
        )
        raw_key = (
            "raw-endpoint_restart_poll"
            if case_id == "endpoint_restart"
            else "raw-outage_poll"
        )
        raw[raw_key]["facts"]["after_service_generation"] = prior_generation
    else:
        case_id = "exact_current_values"
        raw["raw-later_poll"]["session_sequence_before"] = 1
        raw["raw-later_poll"]["session_sequence_after"] = 1
        context = replace(
            context,
            invocations=tuple(
                replace(
                    item,
                    session_sequence_before=1,
                    session_sequence_after=1,
                )
                if item.operation == "later_poll"
                else item
                for item in context.invocations
            ),
        )

    decision = admit_case(_case(case_id, context), context)

    assert decision.status == "failed"


def test_contract_rejects_missing_verify_and_substituted_initial_anchor() -> None:
    """The actual initial invocation must precede the resolved internal verify."""
    context, raw = _context()
    observations = dict(context.controller_observations)
    observations.pop(2)
    missing = replace(
        context, controller_observations=MappingProxyType(observations)
    )
    assert (
        admit_case(_case("exact_current_values", missing), missing).status
        != "passed"
    )

    raw["raw-initial_poll"]["session_sequence_before"] = 2
    raw["raw-initial_poll"]["session_sequence_after"] = 2
    substituted = replace(
        context,
        invocations=tuple(
            replace(
                item,
                session_sequence_before=2,
                session_sequence_after=2,
            )
            if item.operation == "initial_poll"
            else item
            for item in context.invocations
        ),
    )
    assert (
        admit_case(_case("exact_current_values", substituted), substituted).status
        == "failed"
    )


def test_contract_rejects_coherent_child_hub_and_module_substitutions() -> None:
    """Child facts cannot replace independent Hub or installed-member identity."""
    context, raw = _context()
    other_hub = "99999999-9999-4999-8999-999999999999"
    raw["raw-observe_installed_runtime"]["facts"]["hub_id"] = other_hub
    raw["raw-probe"]["facts"]["hub_id"] = other_hub
    discovery = _case("discovery_identity_profile", context)
    discovery["expected"] = dict(discovery["expected"], hub_id=other_hub)
    discovery["actual"] = dict(discovery["actual"], hub_id=other_hub)
    assert admit_case(discovery, context).status == "failed"

    context, raw = _context()
    raw["raw-observe_installed_runtime"]["facts"]["loaded_modules"][
        "custom_components.teslatlas_hub.client"
    ] = "config_flow.py"
    assert (
        admit_case(_case("candidate_artifact_identity", context), context).status
        == "failed"
    )


def test_contract_rejects_pending_requests_even_when_counts_are_coherent() -> None:
    """Clean unload requires every attempted request to reach a final outcome."""
    context, raw = _context()
    facts = raw["raw-unload"]["facts"]
    facts["attempted_requests"] += 1
    facts["pending_requests"] = 1
    facts["attempts"].append(
        {
            "method": "GET",
            "route": "/v1/vehicles",
            "outcome": "pending",
            "phase": "before_unload",
        }
    )

    decision = admit_case(_case("polling_transport_zero_sse", context), context)

    assert decision.status == "failed"

    context, raw = _context()
    facts = raw["raw-unload"]["facts"]
    facts["attempted_requests"] += 1
    facts["failed_requests"] += 1
    facts["post_unload_attempts"] = 1
    facts["post_unload_requests"] = 1
    facts["attempts"].append(
        {
            "method": "GET",
            "route": "/v1/vehicles",
            "outcome": "failed",
            "phase": "after_unload",
        }
    )
    leaked = admit_case(_case("polling_transport_zero_sse", context), context)
    assert leaked.status == "failed"

    context, raw = _context()
    facts = raw["raw-unload"]["facts"]
    facts["completed_requests"] -= 1
    facts["failed_requests"] += 1
    facts["attempts"][0]["outcome"] = "failed"
    unbound_census = admit_case(
        _case("polling_transport_zero_sse", context), context
    )
    assert unbound_census.status == "failed"


def test_real_case_projection_admits_all_enriched_operation_facts() -> None:
    """Production projection builds every normalized case from its sealed raw facts."""
    context, raw = _context()
    runtime = object.__new__(MatrixRuntime)
    runtime.raw = raw
    runtime.case_facts = None

    runtime.set_case_facts_from_raw()
    cases = runtime._cases(context)

    assert [case["id"] for case in cases] == list(REQUIRED_CASES)
    assert [case["status"] for case in cases] == [
        "pending" if case_id == "installed_service_runtime" else "passed"
        for case_id in REQUIRED_CASES
    ]
    assert cases[4]["actual"] == CASE_FACTS["bad_invitation"]
    assert cases[12]["actual"] == CASE_FACTS["endpoint_restart"]
    assert cases[13]["actual"] == CASE_FACTS["outage_recovery"]


def test_main_composes_authentic_loading_all_cases_and_close_lifetime(
    tmp_path: Path,
) -> None:
    """The launcher joins real loading, case admission, readiness, close and exit."""
    session_path, session = _session(tmp_path)
    inputs = session_path.parent
    outputs = Path(session["outputs"]["normalized"]).parent
    source_root = Path(__file__).parents[1]
    installed_root = inputs / "product/custom_components/teslatlas_hub"
    for relative in INSTALLED_MEMBER_PATHS:
        source = source_root / "custom_components/teslatlas_hub" / relative
        destination = installed_root / relative
        destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        destination.chmod(0o644)
    archive = Path(session["inputs"]["product_inputs"][0]["staged"]["local"]["path"])
    installed_manifest = {
        "schema_version": 1,
        "artifact_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
        "files": [
            {
                "path": relative,
                "bytes": (installed_root / relative).stat().st_size,
                "mode": 0o644,
                "sha256": hashlib.sha256(
                    (installed_root / relative).read_bytes()
                ).hexdigest(),
            }
            for relative in INSTALLED_MEMBER_PATHS
        ],
    }
    installed_path = inputs / "installed.json"
    _write_private(installed_path, installed_manifest)
    product = session["inputs"]["product_inputs"][0]
    product["installed_manifest"] = _staged(
        "installed-manifest", installed_path, root=Path("/root/installed.json")
    )
    product["local_root"] = str(installed_root)

    context, _raw = _context()
    scenario_path = Path(session["inputs"]["scenario"]["local"]["path"])
    _write_private(scenario_path, dict(context.scenario))
    session["inputs"]["scenario"] = _staged(
        "scenario", scenario_path, root=Path("/root/scenario.json")
    )
    profile_sha256 = session["inputs"]["profile_manifest"]["local"]["sha256"]
    header = {
        "schema_version": 1,
        "execution_kind": "actual_hub_acceptance",
        "adapter": "home_assistant",
        "cell_id": session["cell_id"],
        "product_version": "2026.36.2",
        "profile_id": "hub-http-v1",
        "profile_revision": "1.0.0",
        "profile_sha256": profile_sha256,
        "source_identities": [],
        "artifacts": [
            {
                "role": "home_assistant_integration_archive",
                "sha256": product["staged"]["local"]["sha256"],
            }
        ],
        "runtime": {"hub": {"service_mode": "installed-deb-systemd"}},
    }
    header_path = Path(session["header"]["local"]["path"])
    _write_private(header_path, header)
    session["header"] = _staged("header", header_path, root=Path("/root/header.json"))
    contract_path = Path(session["case_contract"]["local"]["path"])
    shutil.copyfile(source_root / "tools/matrix-contract.json", contract_path)
    contract_path.chmod(0o600)
    session["case_contract"] = _staged(
        "contract", contract_path, root=Path("/root/contract.json")
    )

    runtime_facts = copy.deepcopy(OPERATION_FACTS["observe_installed_runtime"])
    runtime_facts.update(
        archive_sha256=product["staged"]["local"]["sha256"],
        installed_manifest_sha256=hashlib.sha256(installed_path.read_bytes()).hexdigest(),
        module_sha256=installed_manifest["files"][0]["sha256"],
        actor_manifest_sha256s={
            spec["id"]: spec["input_manifest"]["local"]["sha256"]
            for spec in session["actors"]
        },
    )
    operation_facts = copy.deepcopy(OPERATION_FACTS)
    operation_facts["observe_installed_runtime"] = runtime_facts
    sequence_for = {
        "observe_installed_runtime": (1, 1),
        "probe": (1, 1),
        "bad_pair": (1, 1),
        "expired_pair": (1, 1),
        "replay_pair": (1, 1),
        "config_flow_setup": (1, 1),
        "unknown_current": (1, 1),
        "initial_poll": (1, 1),
        "later_poll": (3, 3),
        "unsupported_surface": (3, 3),
        "revocation_refresh": (3, 4),
        "reauthentication_flow": (4, 5),
        "endpoint_restart_poll": (5, 7),
        "outage_poll": (7, 9),
        "unload": (9, 9),
    }
    fixture_path = inputs / "composition.json"
    _write_private(
        fixture_path,
        {
            "operations": {
                operation: {
                    "actor_id": (
                        "ha_client"
                        if operation
                        in {
                            "probe",
                            "bad_pair",
                            "expired_pair",
                            "replay_pair",
                            "unknown_current",
                            "unsupported_surface",
                        }
                        else "ha_flow"
                    ),
                    "before": sequence_for[operation][0],
                    "after": sequence_for[operation][1],
                    "facts": operation_facts[operation],
                    "requests": OPERATION_REQUESTS[operation],
                }
                for operation in OPERATION_REQUESTS
            },
            "installed_members": installed_manifest["files"],
            "installed_root": str(installed_root),
        },
    )
    _write_private(session_path, session)

    program = r'''
import asyncio,json,sys,types
from pathlib import Path
from types import MappingProxyType
from tools.matrix_contract import AdmittedActor
from tools import matrix_live
fixture=json.loads(Path(sys.argv[2]).read_text())
fake=types.ModuleType("pytest")
fake.fixture=lambda function:function
fake.ExitCode=types.SimpleNamespace(OK=0)
def framework_main(_arguments,plugins):
    runtime=plugins[0].matrix_runtime()
    asyncio.run(runtime.verify())
    for operation,row in fixture["operations"].items():
        runtime.record_operation(operation,actor_id=row["actor_id"],before=row["before"],after=row["after"],facts=row["facts"],requests=row["requests"])
    runtime.set_case_facts_from_raw()
    loaded=fixture["operations"]["observe_installed_runtime"]["facts"]["loaded_modules"]
    for name,relative in loaded.items():
        module=types.ModuleType(name)
        module.__file__=str(Path(fixture["installed_root"])/relative)
        module.__cached__=None
        sys.modules[name]=module
    def actors(_self):
        result={}
        product=runtime.session["inputs"]["product_inputs"][0]
        facts=fixture["operations"]["observe_installed_runtime"]["facts"]
        for spec in runtime.session["actors"]:
            actor_runtime={"schema_version":1,"runtime_ref":"ha_container","runtime_kind":"docker-container","container_id":"2"*64,"python":{"executable":facts["python_executable"],"sha256":"3"*64,"version":"3.14.2"},"home_assistant":{"version":"2026.8.3"},"artifact":{"role":"home_assistant_integration_archive","sha256":facts["archive_sha256"],"installed_root":fixture["installed_root"],"installed_manifest_sha256":facts["installed_manifest_sha256"],"installed_members":fixture["installed_members"]},"actor_input_manifest_sha256":spec["input_manifest"]["local"]["sha256"]}
            result[spec["id"]]=AdmittedActor(id=spec["id"],kind=spec["kind"],runtime_ref=spec["runtime_ref"],entrypoint_ref=spec["entrypoint_ref"],artifact_roles=tuple(spec["artifact_roles"]),source_roles=tuple(spec["source_roles"]),installed_manifest=MappingProxyType(dict(spec["input_manifest"]["local"])),runtime=MappingProxyType(actor_runtime))
        return result
    runtime._actors=types.MethodType(actors,runtime)
    return 0
fake.main=framework_main
sys.modules["pytest"]=fake
raise SystemExit(matrix_live.main([sys.argv[1]]))
'''.replace("\n+", "\n")
    environment = dict(
        os.environ,
        PYTHONPATH=str(source_root),
        SSL_CERT_FILE=session["inputs"]["certificate"]["local"]["path"],
        REQUESTS_CA_BUNDLE=session["inputs"]["certificate"]["local"]["path"],
    )
    process = subprocess.Popen(
        [sys.executable, "-c", program, str(session_path), str(fixture_path)],
        cwd=source_root,
        env=environment,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert process.stdin is not None
    assert process.stdout is not None
    process.stdin.write(
        canonical_json_bytes(
            {
                "schema_version": 1,
                "type": "challenge",
                "session_id": SESSION_ID,
                "sequence": 0,
                "challenge": "1" * 64,
            }
        )
        + b"\n"
    )
    process.stdin.flush()
    broker_requests: list[dict] = []

    def broker_peer() -> None:
        assert process.stdin is not None
        assert process.stdout is not None
        for sequence in (1, 2):
            request = json.loads(process.stdout.readline())
            broker_requests.append(request)
            descriptor = {
                "status": "ready",
                "provenance": "installed-package-service",
                "endpoint": "https://localhost:18480",
                "hub_id": HUB_ID,
                "hub_pid": 42,
                "hub_started_at": "synthetic-start",
                "service_generation": "generation-1",
                "binary_sha256": "1" * 64,
                "seed_binary_sha256": "b" * 64,
                "profile_id": "hub-http-v1@1.0.0",
                "profile_path": "/root/profile/SHA256SUMS",
                "profile_sha256": profile_sha256,
                "scenario_path": "/root/scenario.json",
                "scenario_sha256": session["inputs"]["scenario"]["local"]["sha256"],
                "certificate_path": "/root/certificate.pem",
            }
            proof = {
                "status": "verified",
                "session_id": SESSION_ID,
                "sequence": sequence,
                "config": {
                    "scenario_sha256": descriptor["scenario_sha256"],
                    "seed_sha256": "b" * 64,
                    "store_id": HUB_ID,
                    "store_schema_version": 4,
                },
                "service": {"generation": "generation-1"},
                "discovery": {"hub_id": HUB_ID, "product_version": "2026.36.2"},
                "tls": {
                    "endpoint": descriptor["endpoint"],
                    "certificate_der_sha256": session["inputs"][
                        "certificate_der_sha256"
                    ],
                },
            }
            process.stdin.write(
                canonical_json_bytes(
                    {
                        "schema_version": 1,
                        "type": "reply",
                        "session_id": SESSION_ID,
                        "sequence": request["sequence"],
                        "challenge": str(sequence + 1) * 64,
                        "result": {
                            "descriptor": descriptor,
                            "proof": proof,
                            "invitation": {
                                "pairingId": ACTIVE_PAIRING_ID,
                                "expiresAtMs": 1000,
                            },
                            "expired_invitation": {
                                "pairingId": EXPIRED_PAIRING_ID,
                                "expiresAtMs": 1,
                            },
                            "events": [],
                        },
                    }
                )
                + b"\n"
            )
            process.stdin.flush()

    peer = threading.Thread(target=broker_peer)
    peer.start()
    ready_path = outputs / "coordination/ready-000001.json"
    for _ in range(500):
        if ready_path.exists():
            break
        if process.poll() is not None:
            break
        time.sleep(0.01)
    assert ready_path.exists()
    normalized = json.loads((outputs / "normalized.json").read_text())
    assert [case["id"] for case in normalized["cases"]] == list(REQUIRED_CASES)
    close_result = outputs / "close-result.json"
    _write_private(close_result, {"stopped": True, "cleanup": True})
    ready = json.loads(ready_path.read_text())
    _write_private(
        outputs / "coordination/ack-000001.json",
        {
            "schema_version": 1,
            "type": "ack",
            "session_id": SESSION_ID,
            "cell_id": session["cell_id"],
            "session_input_sha256": hashlib.sha256(
                session_path.read_bytes()
            ).hexdigest(),
            "instance_nonce": session["instance_nonce"],
            "sequence": 1,
            "ready_sha256": hashlib.sha256(ready_path.read_bytes()).hexdigest(),
            "phase": ready["phase"],
            "status": "accepted",
            "action": "close_completed",
            "result": file_binding(close_result),
        },
    )
    assert process.wait(timeout=5) == 0
    peer.join(timeout=2)
    assert not peer.is_alive()
    assert [request["op"] for request in broker_requests] == ["verify", "verify"]
    assert process.stderr is not None and process.stderr.read() == b""
    assert (outputs / "framework.log").read_bytes() == b""


def test_finalize_holds_attachment_until_exact_close_ack(tmp_path: Path) -> None:
    """Real finalize publishes immutable evidence and waits before exit/close."""
    output = tmp_path / "output"
    output.mkdir(mode=0o700)
    coordination = output / "coordination"
    events: list[str] = []

    class Broker:
        def assert_attached(self) -> None:
            events.append("attached")

        def close(self) -> None:
            events.append("closed")

    class Runtime(MatrixRuntime):
        async def verify(self):
            events.append("verified")
            return {}

        def _actors(self):
            return {}

        def _invocations(self):
            return ()

        def _cases(self, _context):
            return []

        def validate_loaded_modules(self):
            events.append("inventory")
            return {}

    runtime = object.__new__(Runtime)
    runtime.case_facts = {}
    runtime.sequence = 11
    runtime.header = {"schema_version": 1}
    runtime.scenario = {}
    runtime.raw = {}
    runtime.raw_bindings = {}
    runtime.observations = {11: {"proof_sha256": "b" * 64}}
    runtime.session_input_sha256 = SHA
    runtime.session = {
        "session_id": SESSION_ID,
        "cell_id": "home_assistant__debian13_arm64",
        "outputs": {
            "normalized": str(output / "normalized.json"),
            "actor_evidence": str(output / "actors.json"),
            "coordination_dir": str(coordination),
        },
    }
    runtime.broker = Broker()
    runtime.channel = CoordinationChannel(
        coordination,
        session_id=SESSION_ID,
        cell_id="home_assistant__debian13_arm64",
        session_input_sha256=SHA,
        instance_nonce="b" * 64,
        cleanup_timeout_ms=45_000,
    )
    errors: list[BaseException] = []

    def finish() -> None:
        try:
            asyncio.run(runtime.finalize())
        except BaseException as error:
            errors.append(error)

    thread = threading.Thread(target=finish)
    thread.start()
    ready_path = coordination / "ready-000001.json"
    for _ in range(200):
        if ready_path.exists():
            break
        time.sleep(0.01)
    assert ready_path.exists()
    assert thread.is_alive()
    assert events[:3] == ["verified", "inventory", "attached"]
    assert "closed" not in events

    result_path = output / "close-result.json"
    _write_private(result_path, {"stopped": True, "cleanup": True})
    _write_private(
        coordination / "ack-000001.json",
        {
            "schema_version": 1,
            "type": "ack",
            "session_id": SESSION_ID,
            "cell_id": "home_assistant__debian13_arm64",
            "session_input_sha256": SHA,
            "instance_nonce": "b" * 64,
            "sequence": 1,
            "ready_sha256": hashlib.sha256(ready_path.read_bytes()).hexdigest(),
            "phase": "evidence_ready",
            "status": "accepted",
            "action": "close_completed",
            "result": file_binding(result_path),
        },
    )
    thread.join(timeout=3)
    assert not thread.is_alive()
    assert errors == []
    assert events[-1] == "inventory"
    assert "closed" not in events
    runtime.broker.close()
    assert events[-1] == "closed"
