import dataclasses
import hashlib
import json
import os
import socket
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from types import MappingProxyType

import jsonschema

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))

import matrix_contract
import matrix_live
import matrix_wire


SESSION_ID = "12345678-1234-4234-8234-123456789abc"
DIGEST = "a" * 64


def _load_phase_contract(actor_id):
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / f"{actor_id}-phases.json"
        path.write_bytes((TOOLS / f"{actor_id}-phases.json").read_bytes())
        os.chmod(path, 0o600)
        return matrix_wire.load_phase_contract(path, actor_id)


def _raw(actor_id, operation, facts, requests=(), before=7, after=7,
         credential_device_id=None):
    return MappingProxyType({
        "schema_version": 1,
        "session_id": SESSION_ID,
        "cell_id": "swift__macos_arm64",
        "session_input_sha256": DIGEST,
        "actor_id": actor_id,
        "operation": operation,
        "actor_manifest_sha256": DIGEST,
        "session_sequence_before": before,
        "session_sequence_after": after,
        "credential_device_id": credential_device_id,
        "facts": facts,
        "requests": list(requests),
        "cleanup": {
            "status": "passed", "transport_resources_closed": True,
            "auxiliary_fixture_stopped": True, "process_exited": True,
        },
    })


def _request(method="GET", route="/.well-known/teslatlas-hub", status=200,
             request_id="r1", scope=None, request_if_none_match=None,
             response_etag=None, response_cache_control=None):
    return {
        "method": method, "route": route, "status": status,
        "request_id": request_id, "scope": scope or route,
        "request_if_none_match": request_if_none_match,
        "response_etag": response_etag,
        "response_cache_control": response_cache_control,
    }


def _requests_for(case_id, pairing_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"):
    discovery = _request(scope="/.well-known/teslatlas-hub")
    vehicles = _request(route="/v1/vehicles", scope="/v1/vehicles")
    claim = lambda status: _request(
        "POST", "/v1/pairings/{pairing_id}/claim", status,
        scope=f"/v1/pairings/{pairing_id}/claim",
    )
    def drives(suffix, status=200, request_if_none_match=None,
               response_etag=None, response_cache_control=None):
        return _request(
            route="/v1/vehicles/{vehicle_id}/drives", status=status,
            scope=f"/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?{suffix}",
            request_if_none_match=request_if_none_match,
            response_etag=response_etag,
            response_cache_control=response_cache_control,
        )
    etags = ['"' + digit * 64 + '"' for digit in ("a", "b", "c")]
    return {
        "candidate_artifact_identity": [],
        "installed_service_runtime": [],
        "discovery_identity_profile": [discovery],
        "unauthenticated_discovery": [
            discovery, _request(route="/healthz"), _request(route="/readyz"),
        ],
        "bad_invitation": [claim(401)],
        "expired_invitation": [],
        "replayed_invitation": [claim(401)],
        "real_auth": [claim(200), vehicles],
        "credential_lifecycle_reauth": [claim(200), vehicles],
        "revocation": [_request(route="/v1/vehicles", status=401, scope="/v1/vehicles")],
        "unknown_vehicle": [_request(
            route="/v1/vehicles/{vehicle_id}/current", status=404,
            scope=f"/v1/vehicles/{matrix_contract.UNKNOWN_VEHICLE}/current",
        )],
        "exact_current_values": [
            _request(route="/v1/vehicles/{vehicle_id}/current", request_id="current-1",
                     scope="/v1/vehicles/11111111-1111-4111-8111-111111111111/current"),
            _request(route="/v1/vehicles/{vehicle_id}/current", request_id="current-2",
                     scope="/v1/vehicles/22222222-2222-4222-8222-222222222222/current"),
        ],
        "endpoint_restart": [vehicles],
        "outage_recovery": [
            _request(route="/v1/vehicles", status=0, request_id="outage", scope="/v1/vehicles"),
            _request(route="/v1/vehicles", request_id="recovery", scope="/v1/vehicles"),
        ],
        "unsupported_operation_zero_requests": [],
        "credential_rotation_api": [
            discovery,
            _request("POST", "/v1/device/rotate", 200, "rotate", "/v1/device/rotate"),
            _request(request_id="discovery-2", scope="/.well-known/teslatlas-hub"),
            _request(route="/v1/vehicles", status=401, request_id="old", scope="/v1/vehicles"),
            _request(route="/v1/vehicles", request_id="new", scope="/v1/vehicles"),
        ],
        "drives_three_page_order": [drives("limit=2"), drives("limit=2&cursor=page2"), drives("limit=2&cursor=page3")],
        "drives_terminal_cursor": [drives("limit=2&cursor=page3")],
        "drives_etag_304": [
            drives("limit=2", response_etag=etags[0], response_cache_control="no-store"),
            drives("limit=2", 304, etags[0], etags[0], "no-store"),
            drives("limit=2&cursor=page2", response_etag=etags[1], response_cache_control="no-store"),
            drives("limit=2&cursor=page2", 304, etags[1], etags[1], "no-store"),
            drives("limit=2&cursor=page3", response_etag=etags[2], response_cache_control="no-store"),
            drives("limit=2&cursor=page3", 304, etags[2], etags[2], "no-store"),
        ],
        "drives_wrong_vehicle_cursor": [_request(
            route="/v1/vehicles/{vehicle_id}/drives", status=400,
            scope="/v1/vehicles/22222222-2222-4222-8222-222222222222/drives?limit=2&cursor=page2",
        )],
        "drives_wrong_filter_cursor": [_request(
            route="/v1/vehicles/{vehicle_id}/drives", status=400,
            scope="/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?from_ms=1788566400001&limit=2&cursor=page2",
        )],
        "native_macos_transport": [discovery, _request(route="/healthz"), _request(route="/readyz")],
        "native_linux_transport": [discovery, _request(route="/healthz"), _request(route="/readyz")],
        "transport_cancellation": [_request(
            route="/cancel", status=0, scope="/cancel?fixture=matrix-cancellation-v1",
        )],
        "transport_body_limit": [_request(
            route="/oversize", status=0, scope="/oversize?fixture=matrix-body-limit-v1",
        )],
    }[case_id]


def _fixture_cursor_provenance(case_id):
    if case_id == "drives_three_page_order":
        requests = _requests_for(case_id)
        return {
            "page2_sha256": hashlib.sha256(
                matrix_contract._scope_cursor(
                    requests[1]["scope"], matrix_contract.PRIMARY_VEHICLE
                ).encode()
            ).hexdigest(),
            "page3_sha256": hashlib.sha256(
                matrix_contract._scope_cursor(
                    requests[2]["scope"], matrix_contract.PRIMARY_VEHICLE
                ).encode()
            ).hexdigest(),
        }
    if case_id == "drives_etag_304":
        requests = _requests_for(case_id)
        return {
            "page2_sha256": hashlib.sha256(
                matrix_contract._scope_cursor(
                    requests[2]["scope"], matrix_contract.PRIMARY_VEHICLE
                ).encode()
            ).hexdigest(),
            "page3_sha256": hashlib.sha256(
                matrix_contract._scope_cursor(
                    requests[4]["scope"], matrix_contract.PRIMARY_VEHICLE
                ).encode()
            ).hexdigest(),
        }
    if case_id == "drives_terminal_cursor":
        requests = _requests_for(case_id)
        return {
            "terminal_sha256": hashlib.sha256(
                matrix_contract._scope_cursor(
                    requests[0]["scope"], matrix_contract.PRIMARY_VEHICLE
                ).encode()
            ).hexdigest()
        }
    if case_id in {"drives_wrong_vehicle_cursor", "drives_wrong_filter_cursor"}:
        requests = _requests_for(case_id)
        vehicle = (
            matrix_contract.SECONDARY_VEHICLE
            if case_id == "drives_wrong_vehicle_cursor"
            else matrix_contract.PRIMARY_VEHICLE
        )
        return {
            "cursor_sha256": hashlib.sha256(
                matrix_contract._scope_cursor(requests[0]["scope"], vehicle).encode()
            ).hexdigest()
        }
    return None


def _phase_admissions(actor_id):
    boundaries = (
        ((1, 3), (3, 5), (5, 8), (8, 9), (9, 10), (10, 11))
        if actor_id == "swift_macos"
        else ((11, 13), (13, 15), (15, 18), (18, 19), (19, 20), (20, 21))
    )
    return [
        {
            "ordinal": ordinal, "phase_id": phase_id,
            "session_sequence_before": bounds[0],
            "session_sequence_after": bounds[1],
            "ready_sha256": hashlib.sha256(f"{actor_id}:{phase_id}".encode()).hexdigest(),
        }
        for ordinal, ((phase_id, _), bounds) in enumerate(
            zip(matrix_contract.PHASE_RECIPES, boundaries), 1
        )
    ]


def _actor(actor_id):
    member = {"path": "Tests.xctest", "sha256": DIGEST, "bytes": 1, "mode": 0o755}
    binding = {"path": "/installed/Tests.xctest", "sha256": DIGEST}
    is_macos = actor_id == "swift_macos"
    execution = {
        "kind": "native-process", "executable": binding,
        "argv_sha256": DIGEST, "environment_sha256": DIGEST,
    } if is_macos else {
        "kind": "docker-exec", "container_id": DIGEST, "image_id": DIGEST,
        "executable": binding, "argv_sha256": DIGEST, "environment_sha256": DIGEST,
    }
    transport = {"kind": "URLSession", "foundation": binding} if is_macos else {
        "kind": "libcurl",
        "libcurl": {"path": "/usr/lib/libcurl.so", "sha256": DIGEST, "version": "8.0"},
        "tls_backend": "OpenSSL", "libssl": binding, "libcrypto": binding,
    }
    return matrix_contract.AdmittedActor(
        id=actor_id,
        kind="current_swift_transport",
        runtime_ref="swift_macos" if is_macos else "swift_container",
        entrypoint_ref=f"swift_current_{'native' if actor_id == 'swift_macos' else 'linux'}_test",
        artifact_roles=("swift_sdk_product",),
        source_roles=("swift_sdk_source",),
        installed_manifest=MappingProxyType({"path": f"/{actor_id}.json", "sha256": DIGEST}),
        runtime=MappingProxyType({
            "schema_version": 1,
            "runtime_ref": "swift_macos" if is_macos else "swift_container",
            "runtime_kind": "native-process" if is_macos else "docker-container",
            "execution": execution,
            "platform": {"os": "macOS" if is_macos else "Ubuntu", "version": "27.0" if is_macos else "22.04.5", "architecture": "arm64"},
            "compiler": {"swift_version": "6.4" if is_macos else "6.0.3", "swiftc": binding},
            "test_product": {"executable": binding, "modules": [member], "resources": [member], "dependencies": [member]},
            "transport": transport,
            "artifact": {"role": "swift_sdk_product", "sha256": "b" * 64, "installed_root": "/installed", "installed_manifest_sha256": DIGEST, "installed_members": [member]},
            "actor_input_manifest_sha256": DIGEST,
            "phase_admissions": _phase_admissions(actor_id),
        }),
    )


def _controller_observations():
    operations = {
        1: "verify", 2: "verify", 3: "pair", 4: "revoke", 5: "pair",
        6: "verify", 7: "stop", 8: "start", 9: "stop", 10: "start",
        11: "verify", 12: "verify", 13: "pair", 14: "revoke", 15: "pair",
        16: "verify", 17: "stop", 18: "start", 19: "stop", 20: "start",
        21: "verify",
    }
    active_ids = {
        range(1, 4): "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        range(4, 13): "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        range(13, 15): "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
        range(15, 22): "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
    }
    observations = {}
    generation = 1
    for sequence, operation in operations.items():
        if operation in {"pair", "revoke", "start"}:
            generation += 1
        active = next(value for keys, value in active_ids.items() if sequence in keys)
        state = "stopped" if operation == "stop" else "running"
        transition = None
        if operation == "pair":
            transition = {"kind": "pair", "from_sequence": sequence - 1}
        elif operation == "revoke":
            transition = {
                "kind": "revoke", "from_sequence": sequence - 1,
                "device_id": "77777777-7777-4777-8777-777777777777",
            }
        elif operation == "stop":
            transition = {"kind": "stop", "from_sequence": sequence - 1}
        elif operation == "start":
            transition = {
                "kind": "start", "from_sequence": sequence - 1,
                "stopped_sequence": sequence - 1,
            }
        observations[sequence] = MappingProxyType({
            "schema_version": 1, "session_id": SESSION_ID, "sequence": sequence,
            "operation": operation, "state": state,
            "started_monotonic_ns": sequence * 100,
            "finished_monotonic_ns": sequence * 100 + 50,
            "observed_at_ms": 1_788_566_400_000,
            "result_sha256": hashlib.sha256(f"result:{sequence}".encode()).hexdigest(),
            "proof_sha256": None if state == "stopped" else hashlib.sha256(f"proof:{sequence}".encode()).hexdigest(),
            "scenario_sha256": "f" * 64, "seed_sha256": "1" * 64,
            "store_id": "99999999-9999-4999-8999-999999999999",
            "store_schema_version": 57,
            "hub_id": "11111111-1111-4111-8111-111111111111",
            "service_generation": f"generation-{generation}",
            "invitations": None if state == "stopped" else {
                "active": {"pairing_id": active, "expires_at_ms": 1_788_566_500_000},
                "expired": {"pairing_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "expires_at_ms": 1_788_566_300_000},
            },
            "transition": transition,
        })
    return MappingProxyType(observations)


def _context(case_id="unauthenticated_discovery", facts=None, requests=None):
    actors = {actor_id: _actor(actor_id) for actor_id in matrix_contract.ACTOR_IDS}
    invocations = []
    operation = matrix_contract.CASE_OPERATIONS[case_id][0]
    anchor_kind = {
        "credential_lifecycle_reauth": "revoke_and_pair",
        "revocation": "revoke_and_pair",
        "endpoint_restart": "restart",
        "outage_recovery": "outage_start",
        "transport_cancellation": "outage_start",
        "transport_body_limit": "outage_start",
    }.get(case_id, "bootstrap_pair")
    for actor_id in matrix_contract.ACTOR_IDS:
        phases = {item["phase_id"]: item for item in actors[actor_id].runtime["phase_admissions"]}
        bootstrap_after = phases["bootstrap_pair"]["session_sequence_after"]
        if case_id in {"credential_lifecycle_reauth", "revocation"}:
            before, after = bootstrap_after, phases[anchor_kind]["session_sequence_after"]
        elif case_id == "endpoint_restart":
            before, after = phases["restart"]["session_sequence_before"], phases["restart"]["session_sequence_after"]
        elif case_id == "outage_recovery":
            before, after = phases["outage_stop"]["session_sequence_before"], phases["outage_start"]["session_sequence_after"]
        elif case_id in {"transport_cancellation", "transport_body_limit"}:
            before = after = phases["outage_start"]["session_sequence_after"]
        else:
            before = after = bootstrap_after
        evidence_id = f"{actor_id}-{operation}"
        invocations.append(matrix_contract.AdmittedInvocation(
            id=f"invoke-{evidence_id}", case_id=case_id, actor_id=actor_id,
            operation=operation, session_sequence_before=before,
            session_sequence_after=after, evidence_id=evidence_id,
            request_ids=(),
        ))
    context = matrix_contract.AdmissionContext(
        adapter_id="swift", cell_id="swift__macos_arm64", session_id=SESSION_ID,
        header=MappingProxyType({
            "artifacts": [
                {"role": "hub_executable", "sha256": "a" * 64},
                {"role": "swift_sdk_product", "sha256": "b" * 64},
            ],
            "runtime": {"hub": {"service_mode": "installed-app-launchagent"}},
        }),
        scenario=MappingProxyType({"vehicle_ids": [
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
        ]}),
        actors=MappingProxyType(actors), invocations=tuple(invocations), raw=MappingProxyType({}),
        controller_observations=_controller_observations(),
    )
    if facts is None:
        facts = matrix_contract._expected_from_context(case_id, context)
        provenance = _fixture_cursor_provenance(case_id)
        if provenance is not None:
            facts = {
                actor_id: dict(actor_facts, cursor_provenance=dict(provenance))
                for actor_id, actor_facts in facts.items()
            }
    raw = {}
    updated_invocations = []
    for index, invocation in enumerate(context.invocations):
        active_pairing_id = facts[invocation.actor_id].get("pairing_id", "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
        selected_requests = _requests_for(case_id, active_pairing_id) if requests is None else requests
        actor_requests = [] if matrix_contract.CASE_KINDS[case_id] != "http" else [
            dict(request, request_id=f"{request['request_id']}-{index}") for request in selected_requests
        ]
        credential_device_id = (
            "77777777-7777-4777-8777-777777777777"
            if case_id in {"revocation", "credential_rotation_api"} else
            "88888888-8888-4888-8888-888888888888"
            if case_id == "credential_lifecycle_reauth" else None
        )
        raw[invocation.evidence_id] = _raw(
            invocation.actor_id, invocation.operation, dict(facts[invocation.actor_id]),
            actor_requests, invocation.session_sequence_before, invocation.session_sequence_after,
            credential_device_id,
        )
        updated_invocations.append(dataclasses.replace(
            invocation, request_ids=tuple(item["request_id"] for item in actor_requests)
        ))
    if case_id in {
        "drives_terminal_cursor", "drives_etag_304",
        "drives_wrong_vehicle_cursor", "drives_wrong_filter_cursor",
    }:
        prior = _context("drives_three_page_order")
        for actor_id in matrix_contract.ACTOR_IDS:
            prior_invocation = next(
                item for item in prior.invocations if item.actor_id == actor_id
            )
            raw[prior_invocation.evidence_id] = prior.raw[prior_invocation.evidence_id]
    return dataclasses.replace(
        context, invocations=tuple(updated_invocations), raw=MappingProxyType(raw)
    )


def _case(case_id, facts, context=None, status=None):
    transcript = []
    if context is not None:
        for invocation in context.invocations:
            if invocation.case_id == case_id and invocation.evidence_id in context.raw:
                transcript.extend(dict(item) for item in context.raw[invocation.evidence_id]["requests"])
    return {
        "id": case_id,
        "status": status or ("pending" if case_id == "installed_service_runtime" else "passed"),
        "expected": facts,
        "actual": facts,
        "evidence_kind": matrix_contract.CASE_KINDS[case_id],
        "request_transcript": transcript,
    }


def _combined_facts(context, case_id):
    return {
        invocation.actor_id: dict(context.raw[invocation.evidence_id]["facts"])
        for invocation in context.invocations if invocation.case_id == case_id
    }


def _composition_write_bytes(path, raw):
    path = Path(path)
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.write_bytes(raw)
    os.chmod(path, 0o600)
    return path


def _composition_staged(identifier, root_path, local_path, raw):
    root_path = _composition_write_bytes(root_path, raw)
    local_path = _composition_write_bytes(local_path, raw)
    return {
        "id": identifier,
        "root": matrix_wire.file_binding(root_path),
        "local": matrix_wire.file_binding(local_path),
    }


class _CompositionFixture:
    """Small real-file/socket fixture for the existing installed coordinator."""

    def __init__(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        os.chmod(self.root, 0o700)
        self.stop_event = threading.Event()
        self.broker_operations = []
        self.broker_error = None
        self._broker_connection = None
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.broker_path = self.root / "broker.sock"
        self.listener.bind(str(self.broker_path))
        self.listener.listen(1)
        self.broker_thread = threading.Thread(target=self._broker_loop, daemon=True)
        self.broker_thread.start()

        self._write_inputs()
        self.launcher = _CompositionLauncher(self)
        self.ack_thread = threading.Thread(target=self._ack_loop, daemon=True)
        self.ack_thread.start()

    def _write_inputs(self):
        profile_root = self.root / "profile-root" / "hub-http-v1" / "1.0.0"
        profile_local = self.root / "profile-local" / "hub-http-v1" / "1.0.0"
        profile_bytes = {
            name: b"{}\n" for name in matrix_wire.PROFILE_MEMBERS
            if name != "SHA256SUMS"
        }
        checksum_bytes = "".join(
            f"{hashlib.sha256(profile_bytes[name]).hexdigest()}  {name}\n"
            for name in sorted(profile_bytes)
        ).encode()
        profile_bytes["SHA256SUMS"] = checksum_bytes
        profile_members = []
        for index, name in enumerate(matrix_wire.PROFILE_MEMBERS, 1):
            profile_members.append(_composition_staged(
                f"profile_member_{index:02d}",
                profile_root / name,
                profile_local / name,
                profile_bytes[name],
            ))
        checksum = next(item for item in profile_members if item["root"]["path"].endswith("/SHA256SUMS"))
        profile_manifest = {
            "id": "profile_manifest",
            "root": checksum["root"],
            "local": checksum["local"],
        }

        hub_digest = hashlib.sha256(b"hub-binary\n").hexdigest()
        product_raw = b"swift-sdk-product\n"
        product_digest = hashlib.sha256(product_raw).hexdigest()
        self.product_digest = product_digest
        self.hub_digest = hub_digest

        header = {
            "schema_version": 1,
            "execution_kind": "actual_hub_acceptance",
            "adapter": "swift",
            "cell_id": "swift__macos_arm64",
            "product_version": matrix_contract.PRODUCT_VERSION,
            "profile_id": "hub-http-v1",
            "profile_revision": "1.0.0",
            "profile_sha256": checksum["local"]["sha256"],
            "source_identities": [{"role": "swift_sdk_source", "sha256": DIGEST}],
            "artifacts": [
                {"role": "hub_executable", "sha256": hub_digest},
                {"role": "swift_sdk_product", "sha256": product_digest},
            ],
            "runtime": {"hub": {"service_mode": "installed-app-launchagent"}},
        }
        header_raw = matrix_wire.canonical_json_bytes(header) + b"\n"
        header = _composition_staged(
            "header", self.root / "header-root.json", self.root / "header-local.json", header_raw
        )
        case_raw = (TOOLS / "matrix-contract.json").read_bytes()
        case_contract = _composition_staged(
            "case_contract", self.root / "case-root.json", self.root / "case-local.json", case_raw
        )
        scenario_raw = matrix_wire.canonical_json_bytes({
            "vehicle_ids": [
                "11111111-1111-4111-8111-111111111111",
                "22222222-2222-4222-8222-222222222222",
            ]
        }) + b"\n"
        scenario = _composition_staged(
            "scenario", self.root / "scenario-root.json", self.root / "scenario-local.json", scenario_raw
        )
        certificate = _composition_staged(
            "certificate", self.root / "certificate-root.pem", self.root / "certificate-local.pem",
            b"-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----\n",
        )

        product = _composition_staged(
            "swift_product", self.root / "product-root.bin", self.root / "product-local.bin", product_raw
        )
        product_member = {
            "path": "Products/TeslatlasCurrentHub.swiftmodule",
            "bytes": 1,
            "mode": 0o644,
            "sha256": DIGEST,
        }
        product_manifest_raw = matrix_wire.canonical_json_bytes({
            "schema_version": 1,
            "artifact_sha256": product_digest,
            "files": [product_member],
        }) + b"\n"
        product_manifest = _composition_staged(
            "swift_product_manifest",
            self.root / "product-manifest-root.json",
            self.root / "product-manifest-local.json",
            product_manifest_raw,
        )

        actor_specs = []
        actor_manifests = {}
        phase_sequences = {
            "swift_macos": (1, 3, 5, 7, 7, 8),
            "swift_linux": (9, 11, 13, 15, 15, 16),
        }
        for actor_id in matrix_contract.ACTOR_IDS:
            build_raw = f"build-record:{actor_id}\n".encode()
            build_path = _composition_write_bytes(
                self.root / "build-records" / f"{actor_id}.json", build_raw
            )
            build_binding = matrix_wire.file_binding(build_path)
            actor_member = {
                "path": f"Tests/{actor_id}.xctest",
                "bytes": 1,
                "mode": 0o755,
                "sha256": DIGEST,
            }
            actor_manifest_raw = matrix_wire.canonical_json_bytes({
                "schema_version": 1,
                "build_record": build_binding,
                "files": [actor_member],
            }) + b"\n"
            actor_manifest = _composition_staged(
                f"{actor_id}_input_manifest",
                self.root / "actor-manifests" / f"{actor_id}-root.json",
                self.root / "actor-manifests" / f"{actor_id}-local.json",
                actor_manifest_raw,
            )
            actor_manifests[actor_id] = actor_manifest
            phase_raw = (TOOLS / f"{actor_id}-phases.json").read_bytes()
            phase = _composition_staged(
                f"{actor_id}_phase_contract",
                self.root / "phases-root" / actor_id / "hub-http-v1" / "1.0.0" / "phases.json",
                self.root / "phases-local" / actor_id / "hub-http-v1" / "1.0.0" / "phases.json",
                phase_raw,
            )
            execution, runtime_ref, entrypoint = matrix_wire.ACTORS[actor_id]
            actor_specs.append({
                "id": actor_id,
                "kind": "current_swift_transport",
                "execution": execution,
                "runtime_ref": runtime_ref,
                "artifact_roles": ["swift_sdk_product"],
                "source_roles": ["swift_sdk_source"],
                "entrypoint_ref": entrypoint,
                "input_manifest": actor_manifest,
                "phase_contract": phase,
            })

        coordination = self.root / "coordination"
        coordination.mkdir(mode=0o700)
        self.normalized_path = self.root / "normalized.json"
        self.actor_evidence_path = self.root / "actor-evidence.json"
        self.worker_evidence_paths = []
        session = {
            "schema_version": 1,
            "kind": "matrix-adapter-session",
            "run_id": "swift-composition-fixture",
            "cell_id": "swift__macos_arm64",
            "adapter_id": "swift",
            "client_id": "swift",
            "session_id": SESSION_ID,
            "instance_nonce": DIGEST,
            "header": header,
            "case_contract": case_contract,
            "host_session": {
                "schema_version": 1,
                "kind": "installed-host",
                "broker_socket": str(self.broker_path),
                "session_id": SESSION_ID,
                "registration_sha256": "e" * 64,
            },
            "broker": {"kind": "unix", "socket_path": str(self.broker_path)},
            "inputs": {
                "profile_manifest": profile_manifest,
                "profile_members": profile_members,
                "scenario": scenario,
                "certificate": certificate,
                "certificate_der_sha256": "f" * 64,
                "product_inputs": [{
                    "artifact_role": "swift_sdk_product",
                    "staged": product,
                    "installed_manifest": product_manifest,
                    "local_root": str(self.root / "product-install"),
                }],
            },
            "actors": actor_specs,
            "outputs": {
                "normalized": str(self.normalized_path),
                "actor_evidence": str(self.actor_evidence_path),
                "coordination_dir": str(coordination),
                "framework_log": str(self.root / "framework.log"),
            },
            "bounds": {
                "cell_timeout_ms": 900_000,
                "cleanup_timeout_ms": 45_000,
                "frame_bytes": matrix_wire.MAX_FRAME_BYTES,
                "evidence_bytes": matrix_wire.MAX_EVIDENCE_BYTES,
                "framework_log_bytes": matrix_wire.MAX_EVIDENCE_BYTES,
            },
        }
        self.session_path = _composition_write_bytes(
            self.root / "session.json",
            matrix_wire.canonical_json_bytes(session) + b"\n",
        )
        self.session_digest = hashlib.sha256(self.session_path.read_bytes()).hexdigest()
        self.actor_manifests = actor_manifests
        self.phase_sequences = phase_sequences
        self.runtime_evidence = {
            actor_id: self._runtime_evidence(actor_id, actor_manifests[actor_id]["local"]["sha256"])
            for actor_id in matrix_contract.ACTOR_IDS
        }
        self.rows = {
            actor_id: self._worker_rows(actor_id, actor_manifests[actor_id]["local"]["sha256"])
            for actor_id in matrix_contract.ACTOR_IDS
        }
        self.profile_paths = {
            "profile": checksum["local"]["path"],
            "scenario": scenario["local"]["path"],
            "certificate": certificate["local"]["path"],
        }

    def _runtime_evidence(self, actor_id, actor_manifest_sha256):
        runtime = json.loads(json.dumps(dict(_actor(actor_id).runtime)))
        runtime["actor_input_manifest_sha256"] = actor_manifest_sha256
        runtime["artifact"]["sha256"] = self.product_digest
        runtime["artifact"]["installed_manifest_sha256"] = actor_manifest_sha256
        return runtime

    def _worker_rows(self, actor_id, actor_manifest_sha256):
        rows = []
        for case_id in matrix_contract.REQUIRED_CASES:
            source = _context(case_id)
            header = dict(source.header)
            header["artifacts"] = [
                {"role": "hub_executable", "sha256": self.hub_digest},
                {"role": "swift_sdk_product", "sha256": self.product_digest},
            ]
            actors = {}
            for candidate in matrix_contract.ACTOR_IDS:
                base = _actor(candidate)
                runtime = json.loads(json.dumps(dict(base.runtime)))
                runtime["actor_input_manifest_sha256"] = actor_manifest_sha256 if candidate == actor_id else DIGEST
                runtime["artifact"]["sha256"] = self.product_digest
                runtime["artifact"]["installed_manifest_sha256"] = runtime["actor_input_manifest_sha256"]
                manifest_sha256 = actor_manifest_sha256 if candidate == actor_id else DIGEST
                actors[candidate] = dataclasses.replace(
                    base,
                    installed_manifest=MappingProxyType({
                        "path": f"/{candidate}-manifest.json", "sha256": manifest_sha256
                    }),
                    runtime=MappingProxyType(runtime),
                )
            adjusted = dataclasses.replace(
                source,
                header=MappingProxyType(header),
                actors=MappingProxyType(actors),
            )
            facts = matrix_contract._expected_from_context(case_id, adjusted)
            invocation = next(
                item for item in source.invocations if item.actor_id == actor_id
            )
            raw = source.raw[invocation.evidence_id]
            actor_facts = dict(facts[actor_id])
            if isinstance(raw["facts"], dict) and "cursor_provenance" in raw["facts"]:
                actor_facts["cursor_provenance"] = dict(raw["facts"]["cursor_provenance"])
            rows.append({
                "case_id": case_id,
                "operation": raw["operation"],
                "actor_manifest_sha256": actor_manifest_sha256,
                "session_sequence_before": raw["session_sequence_before"],
                "session_sequence_after": raw["session_sequence_after"],
                "credential_device_id": raw["credential_device_id"],
                "facts": actor_facts,
                "requests": [dict(request) for request in raw["requests"]],
            })
        return rows

    @staticmethod
    def _proof(sequence):
        return {"status": "verified", "session_id": SESSION_ID, "sequence": sequence}

    def _running_result(self, sequence):
        active = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        expired = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        descriptor = {
            "status": "ready",
            "provenance": "installed-package-service",
            "endpoint": "https://127.0.0.1:18480",
            "hub_id": "11111111-1111-4111-8111-111111111111",
            "hub_pid": 1,
            "hub_started_at": "generation:1",
            "service_generation": "generation:1",
            "binary_sha256": self.hub_digest,
            "seed_binary_sha256": DIGEST,
            "profile_id": "hub-http-v1",
            "profile_path": self.profile_paths["profile"],
            "profile_sha256": "b" * 64,
            "scenario_path": self.profile_paths["scenario"],
            "scenario_sha256": DIGEST,
            "certificate_path": self.profile_paths["certificate"],
        }
        def invitation(pairing_id):
            return {
                "pairingId": pairing_id,
                "secret": "private",
                "expiresAtMs": 1_788_566_500_000,
                "endpoint": "https://127.0.0.1:18480",
                "tlsPin": DIGEST,
                "pairingUri": "teslatlas-hub://pair",
            }
        return {
            "descriptor": descriptor,
            "proof": self._proof(sequence),
            "invitation": invitation(active),
            "expired_invitation": invitation(expired),
            "events": [],
        }

    def _broker_loop(self):
        connection = None
        try:
            connection, _ = self.listener.accept()
            self._broker_connection = connection
            with connection:
                connection.sendall(matrix_wire.canonical_json_bytes({
                    "schema_version": 1,
                    "type": "challenge",
                    "session_id": SESSION_ID,
                    "sequence": 0,
                    "challenge": "c0",
                }) + b"\n")
                reader = connection.makefile("rb")
                outer = 0
                proof = 0
                while not self.stop_event.is_set():
                    raw = reader.readline()
                    if not raw:
                        break
                    request = matrix_wire.strict_json(raw)
                    outer += 1
                    if (
                        not isinstance(request, dict)
                        or request.get("session_id") != SESSION_ID
                        or request.get("sequence") != outer
                    ):
                        raise AssertionError("broker request identity changed")
                    operation = request.get("op")
                    self.broker_operations.append(operation)
                    if operation == "stop":
                        result = {"stopped": True, "events": []}
                    else:
                        proof += 1
                        result = self._running_result(proof)
                    reply = {
                        "schema_version": 1,
                        "type": "reply",
                        "session_id": SESSION_ID,
                        "sequence": outer,
                        "challenge": f"c{outer}",
                        "result": result,
                    }
                    connection.sendall(matrix_wire.canonical_json_bytes(reply) + b"\n")
        except (OSError, ValueError, AssertionError) as error:
            if not self.stop_event.is_set():
                self.broker_error = error
        finally:
            if connection is not None:
                self._broker_connection = None

    def _ack_loop(self):
        ready_path = self.root / "coordination" / "ready-000001.json"
        ack_path = self.root / "coordination" / "ack-000001.json"
        while not self.stop_event.is_set():
            if ready_path.exists():
                ready = matrix_wire.file_binding(ready_path, maximum=65_536)
                close = matrix_wire.write_exclusive_json(
                    self.root / "coordination" / "closed.json", {"status": "closed"}
                )
                matrix_wire.write_exclusive_json(ack_path, {
                    "schema_version": 1,
                    "type": "ack",
                    "session_id": SESSION_ID,
                    "cell_id": "swift__macos_arm64",
                    "session_input_sha256": self.session_digest,
                    "instance_nonce": DIGEST,
                    "sequence": 1,
                    "ready_sha256": ready["sha256"],
                    "phase": "evidence_ready",
                    "status": "accepted",
                    "action": "close_completed",
                    "result": close,
                })
                return
            self.stop_event.wait(0.005)

    def close(self):
        self.stop_event.set()
        try:
            self.listener.close()
        except OSError:
            pass
        if self._broker_connection is not None:
            try:
                self._broker_connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            try:
                self._broker_connection.close()
            except OSError:
                pass
        self.broker_thread.join(timeout=2)
        self.ack_thread.join(timeout=2)
        self.temporary.cleanup()


class _CompositionLauncher:
    def __init__(self, fixture):
        self.fixture = fixture
        self.budgets = []

    def remaining_cell_ms(self, actor_id):
        self.budgets.append(actor_id)
        return 900_000

    def run_worker(self, actor_id, worker_config_binding, phase_callback):
        config = matrix_wire.read_bound_json(
            worker_config_binding, maximum=matrix_wire.MAX_INPUT_BYTES,
            label="worker config",
        )
        phase_contract = matrix_wire.read_bound_json(
            config["phase_contract"]["local"], maximum=matrix_wire.MAX_INPUT_BYTES,
            label="worker phase contract",
        )
        actor_root = Path(config["coordination_dir"])
        for phase, observation_sequence in zip(
            phase_contract["phases"], self.fixture.phase_sequences[actor_id]
        ):
            witness = matrix_wire.write_exclusive_json(
                actor_root / f"witness-{phase['ordinal']:06d}.json",
                {
                    "schema_version": 1,
                    "session_id": SESSION_ID,
                    "actor_id": actor_id,
                    "phase_id": phase["phase_id"],
                    "device_id": (
                        "77777777-7777-4777-8777-777777777777"
                        if phase["phase_id"] == "revoke_and_pair" else None
                    ),
                },
            )
            ready = matrix_wire.write_exclusive_json(
                actor_root / f"ready-{phase['ordinal']:06d}.json",
                {
                    "schema_version": 1,
                    "type": "worker_ready",
                    "session_id": SESSION_ID,
                    "cell_id": config["cell_id"],
                    "session_input_sha256": config["session_input_sha256"],
                    "instance_nonce": config["instance_nonce"],
                    "sequence": phase["ordinal"],
                    "actor_id": actor_id,
                    "phase": phase["phase_id"],
                    "observation": {
                        "session_sequence": observation_sequence,
                        "proof_sha256": matrix_live._proof_sha256(
                            self.fixture._proof(observation_sequence)
                        ),
                    },
                    "evidence": witness,
                },
            )
            phase_callback(ready)
        evidence = {
            "schema_version": 1,
            "session_id": SESSION_ID,
            "cell_id": config["cell_id"],
            "session_input_sha256": config["session_input_sha256"],
            "actor_id": actor_id,
            "raw": self.fixture.rows[actor_id],
            "cleanup": {
                "status": "passed",
                "transport_resources_closed": True,
                "auxiliary_fixture_stopped": True,
            },
        }
        evidence_binding = matrix_wire.write_exclusive_json(
            Path(config["evidence_path"]), evidence
        )
        log_binding = matrix_wire.write_exclusive_json(
            Path(config["log_path"]), {"actor_id": actor_id, "status": "passed"}
        )
        self.fixture.worker_evidence_paths.append(Path(config["evidence_path"]))
        return {
            "schema_version": 1,
            "kind": "swift-worker-outcome",
            "actor_id": actor_id,
            "status": "passed",
            "framework_exit_code": 0,
            "timed_out": False,
            "evidence": evidence_binding,
            "log": log_binding,
            "runtime": self.fixture.runtime_evidence[actor_id],
            "cleanup": {
                "process_exited": True,
                "stdout_closed": True,
                "stderr_closed": True,
            },
        }

    def controller_observations(self, session_id):
        self.fixture.observation_session_id = session_id
        return _controller_observations()


def _build_composition_fixture():
    return _CompositionFixture()


class ContractTests(unittest.TestCase):
    def test_all_25_cases_require_both_real_transport_actors(self):
        self.assertEqual(25, len(matrix_contract.REQUIRED_CASES))
        self.assertEqual(("swift_macos", "swift_linux"), matrix_contract.ACTOR_IDS)
        self.assertTrue(all(
            matrix_contract.CASE_ACTORS[case_id] == matrix_contract.ACTOR_IDS
            for case_id in matrix_contract.REQUIRED_CASES
        ))

    def test_wrong_cursor_cases_preserve_real_swift_http_semantics(self):
        for case_id in ("drives_wrong_vehicle_cursor", "drives_wrong_filter_cursor"):
            self.assertEqual("http", matrix_contract.CASE_KINDS[case_id])
            facts = matrix_contract.expected_facts(case_id, {})
            self.assertEqual(
                {"typed_error": "api", "http_status": 400, "error_code": "invalid_cursor"},
                facts["swift_macos"],
            )

    def test_valid_dual_transport_case_is_admitted(self):
        context = _context()
        facts = matrix_contract.expected_normalized_facts("unauthenticated_discovery", context)
        self.assertEqual(
            matrix_contract.AdmissionDecision("passed", "accepted"),
            matrix_contract.admit_case(_case("unauthenticated_discovery", facts, context), context),
        )

    def test_real_session_linux_runtime_reference_and_pair_revoke_generations_are_admitted(self):
        context = _context()
        self.assertEqual("swift_container", context.actors["swift_linux"].runtime_ref)
        self.assertEqual("swift_container", context.actors["swift_linux"].runtime["runtime_ref"])
        facts = matrix_contract.expected_normalized_facts("unauthenticated_discovery", context)
        self.assertEqual(
            "accepted",
            matrix_contract.admit_case(_case("unauthenticated_discovery", facts, context), context).code,
        )

        changed = dict(context.controller_observations)
        changed[6] = MappingProxyType(dict(changed[6], service_generation="wrong-generation"))
        broken = dataclasses.replace(context, controller_observations=MappingProxyType(changed))
        self.assertEqual(
            "controller_mismatch",
            matrix_contract.admit_case(_case("unauthenticated_discovery", facts, context), broken).code,
        )

    def test_every_http_case_requires_exact_route_order_status_and_scope(self):
        self.assertFalse(matrix_contract._request_requirement(
            "endpoint_restart", [_request(route="/healthz")]
        ))
        self.assertFalse(matrix_contract._request_requirement(
            "real_auth", [_request(route="/healthz")]
        ))
        self.assertFalse(matrix_contract._request_requirement(
            "drives_three_page_order", [_request(route="/healthz")]
        ))
        self.assertFalse(matrix_contract._request_requirement(
            "credential_rotation_api", [
                _request(route="/.well-known/teslatlas-hub", request_id="d1"),
                _request("POST", "/v1/device/rotate", 200, "rotate"),
                _request(route="/.well-known/teslatlas-hub", request_id="d2"),
                _request(route="/v1/vehicles", status=200, request_id="new"),
            ]
        ))
        self.assertFalse(matrix_contract._request_requirement(
            "outage_recovery", [_request(route="/v1/vehicles", status=200)]
        ))
        self.assertFalse(matrix_contract._request_requirement(
            "transport_cancellation",
            [_request(route="/healthz", status=0, scope="/healthz")],
        ))

    def test_reviewer_scope_substitutions_are_rejected(self):
        substitutions = {
            "three_arbitrary_drive_queries_on_one_vehicle": (
                "drives_three_page_order",
                [
                    "/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?wrong=1",
                    "/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?wrong=2",
                    "/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?wrong=3",
                ],
            ),
            "terminal_cursor_wrong_query": (
                "drives_terminal_cursor",
                [
                    "/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?wrong=1",
                ],
            ),
            "wrong_vehicle_same_vehicle": (
                "drives_wrong_vehicle_cursor",
                [
                    "/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?limit=2&cursor=page2",
                ],
            ),
            "wrong_filter_without_filter": (
                "drives_wrong_filter_cursor",
                [
                    "/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?limit=2&cursor=page2",
                ],
            ),
            "discovery_with_extra_path": (
                "discovery_identity_profile",
                ["/.well-known/teslatlas-hub/extra"],
            ),
        }
        for name, (case_id, scopes) in substitutions.items():
            with self.subTest(name=name):
                requests = [dict(item) for item in _requests_for(case_id)]
                for request, scope in zip(requests, scopes):
                    request["scope"] = scope
                self.assertFalse(matrix_contract._request_requirement(case_id, requests))

        pairing_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        requests = [dict(item) for item in _requests_for("real_auth", pairing_id)]
        requests[0]["scope"] = "/v1/pairings/bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb/claim"
        self.assertFalse(
            matrix_contract._request_requirement("real_auth", requests, pairing_id)
        )

        repeated = [dict(item) for item in _requests_for("drives_three_page_order")]
        repeated[2]["scope"] = repeated[1]["scope"]
        self.assertFalse(matrix_contract._request_requirement("drives_three_page_order", repeated))

        repeated_etag = [dict(item) for item in _requests_for("drives_etag_304")]
        repeated_etag[4]["scope"] = repeated_etag[2]["scope"]
        repeated_etag[5]["scope"] = repeated_etag[3]["scope"]
        self.assertFalse(matrix_contract._request_requirement("drives_etag_304", repeated_etag))

    def test_cursor_provenance_binds_requests_to_issued_page_cursors(self):
        three_page = _context("drives_three_page_order")
        three_page_facts = matrix_contract.expected_normalized_facts(
            "drives_three_page_order", three_page
        )
        three_page_case = _case("drives_three_page_order", three_page_facts, three_page)
        forged_raw = dict(three_page.raw)
        evidence_id = three_page.invocations[0].evidence_id
        raw = three_page.raw[evidence_id]
        requests = [dict(item) for item in raw["requests"]]
        requests[1]["scope"] = requests[1]["scope"].replace(
            "cursor=page2", "cursor=forged"
        )
        forged_raw[evidence_id] = MappingProxyType(dict(raw, requests=requests))
        forged_context = dataclasses.replace(
            three_page, raw=MappingProxyType(forged_raw)
        )
        self.assertEqual(
            "raw_fact_mismatch",
            matrix_contract.admit_case(three_page_case, forged_context).code,
        )

        wrong_vehicle = _context("drives_wrong_vehicle_cursor")
        wrong_facts = matrix_contract.expected_normalized_facts(
            "drives_wrong_vehicle_cursor", wrong_vehicle
        )
        wrong_case = _case("drives_wrong_vehicle_cursor", wrong_facts, wrong_vehicle)
        forged_raw = dict(wrong_vehicle.raw)
        evidence_id = wrong_vehicle.invocations[0].evidence_id
        raw = wrong_vehicle.raw[evidence_id]
        requests = [dict(item) for item in raw["requests"]]
        requests[0]["scope"] = requests[0]["scope"].replace(
            "cursor=page2", "cursor=forged"
        )
        forged_provenance = {"cursor_sha256": hashlib.sha256(b"forged").hexdigest()}
        forged_facts = dict(raw["facts"], cursor_provenance=forged_provenance)
        forged_raw[evidence_id] = MappingProxyType(
            dict(raw, requests=requests, facts=forged_facts)
        )
        forged_context = dataclasses.replace(
            wrong_vehicle, raw=MappingProxyType(forged_raw)
        )
        self.assertEqual(
            "raw_fact_mismatch",
            matrix_contract.admit_case(wrong_case, forged_context).code,
        )

        self.assertFalse(
            matrix_contract._request_requirement(
                "real_auth", [dict(item) for item in _requests_for("real_auth")]
            )
        )

        terminal = _context("drives_terminal_cursor")
        terminal_facts = matrix_contract.expected_normalized_facts(
            "drives_terminal_cursor", terminal
        )
        terminal_case = _case("drives_terminal_cursor", terminal_facts, terminal)
        anchor_id = "swift_macos-drives_three_pages"
        anchor = terminal.raw[anchor_id]
        forged_anchor = MappingProxyType(dict(anchor, operation="wrong_vehicle_cursor"))
        forged_raw = dict(terminal.raw, **{anchor_id: forged_anchor})
        forged_terminal = dataclasses.replace(
            terminal, raw=MappingProxyType(forged_raw)
        )
        self.assertEqual(
            "raw_fact_mismatch",
            matrix_contract.admit_case(terminal_case, forged_terminal).code,
        )

    def test_etag_case_requires_exact_conditional_header_witnesses(self):
        context = _context("drives_etag_304")
        facts = matrix_contract.expected_normalized_facts("drives_etag_304", context)
        self.assertEqual("accepted", matrix_contract.admit_case(
            _case("drives_etag_304", facts, context), context
        ).code)
        mutations = {
            "missing_if_none_match": (1, "request_if_none_match", None),
            "wrong_response_etag": (3, "response_etag", '"' + "f" * 64 + '"'),
            "wrong_cache_control": (5, "response_cache_control", "private"),
        }
        for name, (index, field, value) in mutations.items():
            with self.subTest(name=name):
                raw = context.raw[context.invocations[0].evidence_id]
                requests = [dict(item) for item in raw["requests"]]
                requests[index][field] = value
                changed_raw = dict(context.raw)
                changed_raw[context.invocations[0].evidence_id] = MappingProxyType(
                    dict(raw, requests=requests)
                )
                changed = dataclasses.replace(
                    context, raw=MappingProxyType(changed_raw)
                )
                self.assertEqual(
                    "request_mismatch",
                    matrix_contract.admit_case(
                        _case("drives_etag_304", facts, changed), changed
                    ).code,
                )

    def test_raw_schema_accepts_real_discovery_and_vehicle_fact_types(self):
        schema = json.loads((TOOLS / "swift-raw-v1.schema.json").read_text())
        discovery = dict(_raw(
            "swift_macos", "discovery",
            {
                "hub_id": SESSION_ID, "api_versions": ["1.0"],
                "protocol": "teslatlas-sync", "protocol_major": 1,
                "pack_format": "sqlite-zstd", "version": "2026.36.2",
            },
        ))
        real_auth = dict(_raw(
            "swift_macos", "real_auth",
            {
                "pairing_id": SESSION_ID, "claimed": 200,
                "vehicles": [
                    {"vehicle_id": SESSION_ID, "display_name": "Interop"},
                    {"vehicle_id": "22345678-1234-4234-8234-123456789abc", "display_name": "Empty"},
                ],
            },
        ))
        jsonschema.Draft202012Validator(schema).validate(discovery)
        jsonschema.Draft202012Validator(schema).validate(real_auth)

        wrong = dict(real_auth)
        wrong["facts"] = dict(real_auth["facts"])
        wrong["facts"]["vehicles"] = [{"vehicle_id": SESSION_ID}]
        self.assertTrue(list(jsonschema.Draft202012Validator(schema).iter_errors(wrong)))

    def test_all_25_serialization_shapes_pass_schema_and_pure_admission(self):
        schema = json.loads((TOOLS / "swift-raw-v1.schema.json").read_text())
        validator = jsonschema.Draft202012Validator(schema, format_checker=jsonschema.FormatChecker())
        for case_id in matrix_contract.REQUIRED_CASES:
            with self.subTest(case_id=case_id):
                context = _context(case_id)
                facts = matrix_contract.expected_normalized_facts(case_id, context)
                for raw in context.raw.values():
                    validator.validate(dict(raw))
                decision = matrix_contract.admit_case(_case(case_id, facts, context), context)
                expected_status = "pending" if case_id == "installed_service_runtime" else "passed"
                self.assertEqual(expected_status, decision.status, decision.code)

    def test_missing_or_wrong_actor_is_rejected(self):
        context = _context()
        facts = matrix_contract.expected_normalized_facts("unauthenticated_discovery", context)
        broken = dataclasses.replace(
            context,
            actors=MappingProxyType({"swift_macos": context.actors["swift_macos"]}),
            invocations=(context.invocations[0],),
        )
        self.assertEqual("wrong_actor", matrix_contract.admit_case(_case("unauthenticated_discovery", facts), broken).code)

    def test_wrong_source_artifact_or_installed_member_is_rejected(self):
        context = _context()
        facts = matrix_contract.expected_normalized_facts("unauthenticated_discovery", context)
        for field, value, code in (
            ("source_roles", ("protocol_source",), "wrong_source_role"),
            ("artifact_roles", ("hub_executable",), "wrong_artifact_role"),
            ("installed_manifest", MappingProxyType({"path": "/wrong", "sha256": "e" * 64}), "installed_manifest_mismatch"),
        ):
            actors = dict(context.actors)
            actors["swift_linux"] = dataclasses.replace(actors["swift_linux"], **{field: value})
            broken = dataclasses.replace(context, actors=MappingProxyType(actors))
            self.assertEqual(code, matrix_contract.admit_case(_case("unauthenticated_discovery", facts), broken).code)

    def test_stale_sequence_changed_raw_missing_subcase_and_cleanup_fail(self):
        context = _context()
        raw_facts = _combined_facts(context, "unauthenticated_discovery")
        facts = matrix_contract.expected_normalized_facts("unauthenticated_discovery", context)
        case = _case("unauthenticated_discovery", facts, context)
        stale = dataclasses.replace(context, invocations=(dataclasses.replace(context.invocations[0], session_sequence_before=6),) + context.invocations[1:])
        self.assertEqual("controller_mismatch", matrix_contract.admit_case(case, stale).code)
        missing = dataclasses.replace(context, raw=MappingProxyType({context.invocations[0].evidence_id: context.raw[context.invocations[0].evidence_id]}))
        self.assertEqual("raw_missing", matrix_contract.admit_case(case, missing).code)
        evidence_id = context.invocations[1].evidence_id
        changed_raw = dict(context.raw)
        changed_raw[evidence_id] = _raw(
            "swift_linux", context.invocations[1].operation,
            dict(raw_facts["swift_linux"], discovery=418),
            [_request(request_id="different")],
            context.invocations[1].session_sequence_before,
            context.invocations[1].session_sequence_after,
        )
        changed = dataclasses.replace(context, raw=MappingProxyType(changed_raw))
        self.assertEqual("raw_fact_mismatch", matrix_contract.admit_case(case, changed).code)
        dirty = dict(context.raw)
        value = dict(dirty[evidence_id]); value["cleanup"] = {"status": "failed", "workers_exited": False}; dirty[evidence_id] = MappingProxyType(value)
        self.assertEqual("cleanup_failure", matrix_contract.admit_case(case, dataclasses.replace(context, raw=MappingProxyType(dirty))).code)

    def test_unrelated_request_and_literal_type_null_zero_changes_fail(self):
        context = _context()
        facts = matrix_contract.expected_normalized_facts("unauthenticated_discovery", context)
        unrelated = _context(requests=[_request(route="/healthz")])
        self.assertEqual("request_mismatch", matrix_contract.admit_case(_case("unauthenticated_discovery", facts, unrelated), unrelated).code)
        for key, replacement in (("discovery", True), ("credential_absent", 0)):
            wrong = dict(facts); wrong[key] = replacement
            self.assertEqual("literal_mismatch", matrix_contract.admit_case(_case("unauthenticated_discovery", wrong), context).code)
        current = _context("exact_current_values")
        expected = dict(matrix_contract.expected_normalized_facts("exact_current_values", current))
        expected["outside_temp"] = 0
        self.assertEqual("literal_mismatch", matrix_contract.admit_case(_case("exact_current_values", expected), current).code)

    def test_expiry_cannot_be_replaced_by_replay_shaped_raw(self):
        context = _context("expired_invitation", requests=[])
        case = _case("expired_invitation", matrix_contract.expected_normalized_facts("expired_invitation", context), context)
        self.assertEqual("accepted", matrix_contract.admit_case(case, context).code)

        changed_raw = {
            key: MappingProxyType(dict(raw, facts=dict(
                raw["facts"], pairing_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                expires_at_ms=1_788_566_500_000,
            )))
            for key, raw in context.raw.items()
        }
        changed = dataclasses.replace(context, raw=MappingProxyType(changed_raw))
        replay_case = _case("expired_invitation", matrix_contract.expected_normalized_facts("expired_invitation", context))
        self.assertEqual("raw_fact_mismatch", matrix_contract.admit_case(replay_case, changed).code)

    def test_restart_requires_connected_stop_start_and_new_generation(self):
        context = _context("endpoint_restart", requests=[_request(route="/v1/vehicles")])
        case = _case("endpoint_restart", matrix_contract.expected_normalized_facts("endpoint_restart", context), context)
        self.assertEqual("accepted", matrix_contract.admit_case(case, context).code)

        after_sequence = context.invocations[0].session_sequence_after
        before_sequence = context.invocations[0].session_sequence_before
        unchanged = dict(context.controller_observations)
        unchanged[after_sequence] = MappingProxyType(dict(
            unchanged[after_sequence],
            service_generation=unchanged[before_sequence]["service_generation"],
        ))
        changed = dataclasses.replace(context, controller_observations=unchanged)
        self.assertEqual("controller_mismatch", matrix_contract.admit_case(case, changed).code)

        changed_invocations = tuple(
            dataclasses.replace(item, session_sequence_before=item.session_sequence_before - 1)
            for item in context.invocations
        )
        changed_raw = {
            key: MappingProxyType(dict(value, session_sequence_before=value["session_sequence_before"] - 1))
            for key, value in context.raw.items()
        }
        changed = dataclasses.replace(context, invocations=changed_invocations,
                                      raw=MappingProxyType(changed_raw))
        self.assertEqual("controller_mismatch", matrix_contract.admit_case(case, changed).code)

    def test_coherent_actor_phase_and_raw_anchor_substitution_is_rejected(self):
        context = _context()
        actors = dict(context.actors)
        mac_phases = actors["swift_macos"].runtime["phase_admissions"]
        linux_phases = actors["swift_linux"].runtime["phase_admissions"]
        for actor_id, phases in (("swift_macos", linux_phases), ("swift_linux", mac_phases)):
            runtime = dict(actors[actor_id].runtime)
            runtime["phase_admissions"] = phases
            actors[actor_id] = dataclasses.replace(actors[actor_id], runtime=MappingProxyType(runtime))
        invocations = []
        raw = dict(context.raw)
        for invocation in context.invocations:
            phase = actors[invocation.actor_id].runtime["phase_admissions"][0]
            anchor = phase["session_sequence_after"]
            invocations.append(dataclasses.replace(
                invocation, session_sequence_before=anchor, session_sequence_after=anchor
            ))
            raw[invocation.evidence_id] = MappingProxyType(dict(
                raw[invocation.evidence_id],
                session_sequence_before=anchor, session_sequence_after=anchor,
            ))
        changed = dataclasses.replace(
            context, actors=MappingProxyType(actors), invocations=tuple(invocations),
            raw=MappingProxyType(raw),
        )
        facts = matrix_contract.expected_normalized_facts("unauthenticated_discovery", changed)
        self.assertEqual(
            "controller_mismatch",
            matrix_contract.admit_case(_case("unauthenticated_discovery", facts, changed), changed).code,
        )


class WireTests(unittest.TestCase):
    def test_linux_worker_contract_is_closed_and_host_bridged(self):
        contract_path = TOOLS / "swift-linux-worker-contract-v1.json"
        contract = json.loads(contract_path.read_text(encoding="utf-8"))
        self.assertEqual(
            {
                "schema_version", "kind", "client_id", "actor_id", "execution_kind",
                "target_cells", "purpose", "authority", "registration", "relay",
                "forbidden_authority",
            },
            set(contract),
        )
        self.assertEqual(1, contract["schema_version"])
        self.assertEqual("swift-linux-worker-contract", contract["kind"])
        self.assertEqual(("swift__debian13_amd64", "swift__debian13_arm64"), tuple(contract["target_cells"]))

        registration = contract["registration"]
        self.assertTrue(registration["closed"])
        self.assertFalse(registration["additional_properties"])
        self.assertEqual(
            {
                "schema_version", "kind", "client_id", "actor_id", "runtime_ref",
                "entrypoint_ref", "provider", "container", "source_product_map",
                "config", "log_path", "io", "outcome", "cleanup",
            },
            set(registration["required_keys"]),
        )
        properties = registration["properties"]
        self.assertEqual("swift-docker-exec-registration", properties["kind"]["const"])
        self.assertEqual("swift_current_linux_test", properties["entrypoint_ref"]["const"])
        self.assertEqual("swift_container", properties["runtime_ref"]["const"])
        self.assertEqual(
            {"docker_executable", "context", "provider", "machine", "kernel", "architecture", "emulation"},
            set(properties["provider"]["required_keys"]),
        )
        container = properties["container"]
        self.assertEqual(
            {
                "full_container_id", "image_ref", "full_image_id", "image_digest", "platform",
                "user", "network_mode", "privilege", "mounts",
            },
            set(container["required_keys"]),
        )
        self.assertEqual("host", container["properties"]["network_mode"]["const"])
        self.assertEqual([], container["properties"]["mounts"]["const"])
        self.assertEqual("nonprivileged", container["properties"]["privilege"]["const"])
        self.assertEqual("swift_current_linux_test", properties["entrypoint_ref"]["const"])
        self.assertEqual("/dev/null", properties["io"]["properties"]["stdin"]["path"])
        self.assertEqual("actor-log", properties["io"]["properties"]["stdout"]["kind"])
        self.assertEqual("actor-log", properties["io"]["properties"]["stderr"]["kind"])
        self.assertFalse(properties["io"]["properties"]["tty"]["const"])

        relay = contract["relay"]
        self.assertEqual("swift-linux-worker-relay", relay["properties"]["kind"]["const"])
        broker = relay["broker"]["properties"]
        self.assertEqual("docker_exec_pipe", broker["kind"]["const"])
        self.assertEqual("host-root-coordinator", broker["location"]["const"])
        self.assertFalse(broker["worker_socket_exposure"]["const"])
        transfer = relay["file_transfer"]["properties"]
        self.assertEqual("docker-archive", transfer["mode"]["const"])
        self.assertEqual(
            {"worker_config", "phase_contract", "initial_observation", "session_inputs",
             "swift_source", "swift_product", "swift_product_manifest", "phase_result", "phase_ack"},
            {entry["id"] for entry in transfer["host_to_container"]},
        )
        self.assertEqual(
            {"phase_ready", "phase_witness", "worker_evidence", "worker_log"},
            {entry["id"] for entry in transfer["container_to_host"]},
        )
        self.assertEqual(
            {"completion_ready", "completion_ack", "adapter_completion"},
            {entry["id"] for entry in transfer["host_only"]},
        )
        self.assertEqual("container_to_host", relay["handshake"]["phase_ready"]["direction"])
        self.assertEqual("host_to_container", relay["handshake"]["phase_ack"]["direction"])
        self.assertIn("write_host_completion_ready_and_wait_for_root_ack", relay["handshake"]["ordering"])
        self.assertIn("broker_frame", contract["forbidden_authority"])

        launcher = json.loads((TOOLS / "swift-launcher-interface-v1.json").read_text(encoding="utf-8"))
        binding = launcher["linux_worker_contract"]
        self.assertEqual(str(contract_path), binding["path"])
        self.assertEqual(hashlib.sha256(contract_path.read_bytes()).hexdigest(), binding["sha256"])
        self.assertEqual(dict(matrix_live.LAUNCHER_INTERFACE["linux_worker_contract"]), {
            "path": binding["path"], "sha256": binding["sha256"],
        })

    def test_broker_uses_strict_challenge_and_advances_outer_sequence(self):
        left, right = socket.socketpair()
        def server():
            with right:
                right.sendall((json.dumps({"schema_version": 1, "type": "challenge", "session_id": SESSION_ID, "sequence": 0, "challenge": "c0"}) + "\n").encode())
                request = json.loads(right.makefile("rb").readline())
                self.assertEqual({"schema_version", "session_id", "sequence", "challenge", "op"}, set(request))
                descriptor = {
                    "status": "ready", "provenance": "installed-package-service",
                    "endpoint": "https://127.0.0.1:18480", "hub_id": SESSION_ID,
                    "hub_pid": 1, "hub_started_at": "generation:1", "service_generation": "generation:1",
                    "binary_sha256": DIGEST, "seed_binary_sha256": DIGEST,
                    "profile_id": "hub-http-v1", "profile_path": "/profile",
                    "profile_sha256": DIGEST, "scenario_path": "/scenario",
                    "scenario_sha256": DIGEST, "certificate_path": "/certificate",
                }
                def invitation(pairing_id):
                    return {"pairingId": pairing_id, "secret": "private", "expiresAtMs": 1,
                            "endpoint": "https://127.0.0.1:18480", "tlsPin": DIGEST,
                            "pairingUri": "teslatlas-hub://pair"}
                result = {
                    "descriptor": descriptor,
                    "proof": {"status": "verified", "session_id": SESSION_ID, "sequence": 41},
                    "invitation": invitation("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
                    "expired_invitation": invitation("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
                    "events": [],
                }
                right.sendall((json.dumps({"schema_version": 1, "type": "reply", "session_id": SESSION_ID, "sequence": 1, "challenge": "c1", "result": result}) + "\n").encode())
        thread = threading.Thread(target=server); thread.start()
        with left:
            broker = matrix_wire.BrokerClient.attach(left, SESSION_ID)
            result = broker.request("verify")
            self.assertEqual(41, result["proof"]["sequence"])
        thread.join()

    def test_phase_contracts_are_complete_and_have_fixed_recipes(self):
        for actor_id in matrix_contract.ACTOR_IDS:
            contract = _load_phase_contract(actor_id)
            self.assertEqual(list(range(1, 7)), [item["ordinal"] for item in contract["phases"]])
            self.assertEqual(
                ["bootstrap_pair", "revoke_and_pair", "restart", "outage_stop", "outage_start", "final_verify"],
                [item["phase_id"] for item in contract["phases"]],
            )
            self.assertTrue(all(set(item["operations"]) <= matrix_wire.OPERATIONS for item in contract["phases"]))
            self.assertEqual(
                [200_000, 330_000, 160_000, 70_000, 70_000, 40_000],
                [item["timeout_ms"] for item in contract["phases"]],
            )
            self.assertEqual("initial_observation", contract["resources"][0]["id"])

    def test_inert_manifest_binds_validator_and_closed_raw_schema(self):
        contract = matrix_contract.validate_manifest(TOOLS / "matrix-contract.json")
        self.assertEqual(["swift-raw-v1"], [item["id"] for item in contract["raw_schemas"]])

    def test_worker_config_has_no_broker_or_command_authority(self):
        config = {
            "schema_version": 1, "kind": "matrix-actor-worker", "actor_id": "swift_macos",
            "session_id": SESSION_ID, "cell_id": "swift__macos_arm64", "instance_nonce": DIGEST,
            "session_input_sha256": DIGEST,
            "remaining_cell_ms": 900_000,
            "phase_contract": {"id": "phase", "root": {"path": "/root", "sha256": DIGEST}, "local": {"path": "/local", "sha256": DIGEST}},
            "inputs": [{"id": "initial_observation", "root": {"path": "/observation", "sha256": DIGEST}, "local": {"path": "/observation", "sha256": DIGEST}}], "private_root": "/private", "coordination_dir": "/coord", "evidence_path": "/evidence", "log_path": "/log",
        }
        matrix_wire.validate_worker_config(config)
        config["broker_socket"] = "/broker"
        with self.assertRaises(matrix_wire.MatrixWireError):
            matrix_wire.validate_worker_config(config)

    def test_private_bound_json_rejects_symlink_group_mode_oversize_and_changed_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            os.chmod(root, 0o700)
            target = root / "value.json"
            binding = matrix_wire.write_exclusive_json(target, {"value": 1})
            self.assertEqual({"value": 1}, matrix_wire.read_bound_json(binding, maximum=64))

            os.chmod(target, 0o640)
            with self.assertRaises(matrix_wire.MatrixWireError):
                matrix_wire.read_bound_json(binding, maximum=64)
            os.chmod(target, 0o600)

            link = root / "link.json"
            link.symlink_to(target)
            with self.assertRaises(matrix_wire.MatrixWireError):
                matrix_wire.read_bound_json({"path": str(link), "sha256": binding["sha256"]}, maximum=64)

            real_parent = root / "real-parent"
            real_parent.mkdir(mode=0o700)
            nested_target = real_parent / "nested.json"
            nested_binding = matrix_wire.write_exclusive_json(nested_target, {"value": 3})
            parent_link = root / "parent-link"
            parent_link.symlink_to(real_parent, target_is_directory=True)
            with self.assertRaises(matrix_wire.MatrixWireError):
                matrix_wire.read_bound_json(
                    {"path": str(parent_link / "nested.json"), "sha256": nested_binding["sha256"]},
                    maximum=64,
                )

            with self.assertRaises(matrix_wire.MatrixWireError):
                matrix_wire.read_bound_json(binding, maximum=2)

            target.write_bytes(b'{"value":2}\n')
            os.chmod(target, 0o600)
            with self.assertRaises(matrix_wire.MatrixWireError):
                matrix_wire.read_bound_json(binding, maximum=64)

    def test_session_input_requires_reserved_profile_member_ids(self):
        fixture = _build_composition_fixture()
        try:
            session = json.loads(fixture.session_path.read_text(encoding="utf-8"))
            session["inputs"]["profile_members"][0]["id"] = "profile_member_00"
            _composition_write_bytes(
                fixture.session_path,
                matrix_wire.canonical_json_bytes(session) + b"\n",
            )
            with self.assertRaises(matrix_wire.MatrixWireError):
                matrix_wire.load_session_input(fixture.session_path)
        finally:
            fixture.close()


class CoordinatorTests(unittest.TestCase):
    def test_real_session_input_composes_workers_and_writes_bound_case_evidence(self):
        fixture = _build_composition_fixture()
        try:
            result = matrix_live.run_installed(fixture.session_path, fixture.launcher)
            self.assertEqual(0, result)
            normalized = matrix_wire.read_bound_json(
                matrix_wire.file_binding(fixture.normalized_path),
                maximum=matrix_wire.MAX_EVIDENCE_BYTES,
                label="normalized evidence",
            )
            self.assertEqual(
                list(matrix_contract.REQUIRED_CASES),
                [case["id"] for case in normalized["cases"]],
            )
            self.assertEqual(
                "pending",
                next(case["status"] for case in normalized["cases"]
                     if case["id"] == "installed_service_runtime"),
            )
            self.assertTrue(all(
                case["status"] == "passed"
                for case in normalized["cases"]
                if case["id"] != "installed_service_runtime"
            ))

            contract = matrix_contract.validate_manifest(TOOLS / "matrix-contract.json")
            raw_schema = json.loads(Path(
                contract["raw_schemas"][0]["schema"]["path"]
            ).read_text())
            raw_validator = jsonschema.Draft202012Validator(
                raw_schema, format_checker=jsonschema.FormatChecker()
            )
            actor_evidence = matrix_wire.read_bound_json(
                matrix_wire.file_binding(fixture.actor_evidence_path),
                maximum=matrix_wire.MAX_EVIDENCE_BYTES,
                label="actor evidence",
            )
            self.assertEqual(
                ["swift_macos", "swift_linux"],
                [actor["id"] for actor in actor_evidence["actors"]],
            )
            for actor in actor_evidence["actors"]:
                self.assertEqual(
                    ["swift-raw-v1"] * len(actor["raw_evidence"]),
                    [item["schema_id"] for item in actor["raw_evidence"]],
                )
                for item in actor["raw_evidence"]:
                    raw = matrix_wire.read_bound_json(
                        item["binding"], maximum=matrix_wire.MAX_EVIDENCE_BYTES,
                        label="normalized case evidence",
                    )
                    raw_validator.validate(raw)
            worker_schema = json.loads((TOOLS / "swift-worker-v1.schema.json").read_text())
            worker_validator = jsonschema.Draft202012Validator(
                worker_schema, format_checker=jsonschema.FormatChecker()
            )
            for evidence_path in fixture.worker_evidence_paths:
                worker_validator.validate(json.loads(evidence_path.read_text()))
        finally:
            fixture.close()

    def test_initial_guest_sequence_is_derived_and_strictly_advances(self):
        tracker = matrix_live.GuestProofTracker()
        tracker.observe({"status": "verified", "session_id": SESSION_ID, "sequence": 19})
        self.assertEqual(19, tracker.sequence)
        tracker.observe({"status": "verified", "session_id": SESSION_ID, "sequence": 23})
        with self.assertRaises(matrix_wire.MatrixWireError):
            tracker.observe({"status": "verified", "session_id": SESSION_ID, "sequence": 23})

    def test_completion_ack_must_be_accepted_close_completed(self):
        ack = {"schema_version": 1, "type": "ack", "session_id": SESSION_ID,
               "cell_id": "swift__macos_arm64", "session_input_sha256": DIGEST,
               "instance_nonce": DIGEST, "sequence": 1, "ready_sha256": DIGEST,
               "phase": "evidence_ready", "status": "accepted", "action": "close_completed",
               "result": {"path": "/close.json", "sha256": DIGEST}}
        matrix_wire.validate_completion_ack(ack, SESSION_ID, "swift__macos_arm64", DIGEST, DIGEST, 1, DIGEST)
        for key, value in (("status", "rejected"), ("action", "abort"), ("ready_sha256", "b" * 64)):
            broken = dict(ack); broken[key] = value
            with self.assertRaises(matrix_wire.MatrixWireError):
                matrix_wire.validate_completion_ack(broken, SESSION_ID, "swift__macos_arm64", DIGEST, DIGEST, 1, DIGEST)

    def test_phase_callback_binds_exact_retained_worker_ready_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            os.chmod(directory, 0o700)
            root = Path(directory)
            (root / "swift_macos").mkdir(mode=0o700)
            witness = matrix_wire.write_exclusive_json(root / "witness.json", {
                "schema_version": 1, "session_id": SESSION_ID,
                "actor_id": "swift_macos", "phase_id": "bootstrap_pair",
                "device_id": None,
            })
            tracker = matrix_live.GuestProofTracker(SESSION_ID)
            tracker.observe({"status": "verified", "session_id": SESSION_ID, "sequence": 19})
            class Coordinator:
                session = {"session_id": SESSION_ID, "cell_id": "swift__macos_arm64",
                           "instance_nonce": DIGEST, "outputs": {"coordination_dir": str(root)}}
                session_input_sha256 = DIGEST
                proofs = tracker
                @staticmethod
                def apply_phase_recipe(actor_id, phase_id, private_witness=None):
                    return [{"operation": "verify", "result": {"proof": {"sequence": 20}}}]
            phase_contract = _load_phase_contract("swift_macos")
            callback = matrix_live.WorkerPhaseController(Coordinator(), "swift_macos", phase_contract)
            ready = {
                "schema_version": 1, "type": "worker_ready", "session_id": SESSION_ID,
                "cell_id": "swift__macos_arm64", "session_input_sha256": DIGEST,
                "instance_nonce": DIGEST, "sequence": 1, "actor_id": "swift_macos",
                "phase": "bootstrap_pair",
                "observation": {"session_sequence": 19, "proof_sha256": tracker.observations[19]["proof_sha256"]},
                "evidence": witness,
            }
            ready_binding = matrix_wire.write_exclusive_json(root / "worker-ready-000001.json", ready)
            ack = callback(ready_binding)
            self.assertEqual(ready_binding["sha256"], ack["ready_sha256"])


if __name__ == "__main__":
    unittest.main()
