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

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))

import matrix_contract
import matrix_live
import matrix_wire


SESSION_ID = "12345678-1234-4234-8234-123456789abc"
DIGEST = "a" * 64


def _raw(actor_id, operation, facts, requests=(), before=7, after=7):
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
        "facts": facts,
        "requests": list(requests),
        "cleanup": {
            "status": "passed", "transport_resources_closed": True,
            "auxiliary_fixture_stopped": True, "process_exited": True,
        },
    })


def _request(method="GET", route="/.well-known/teslatlas-hub", status=200, request_id="r1"):
    return {"method": method, "route": route, "status": status, "request_id": request_id}


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
        runtime_ref=actor_id,
        entrypoint_ref=f"swift_current_{'native' if actor_id == 'swift_macos' else 'linux'}_test",
        artifact_roles=("swift_sdk_product",),
        source_roles=("swift_sdk_source",),
        installed_manifest=MappingProxyType({"path": f"/{actor_id}.json", "sha256": DIGEST}),
        runtime=MappingProxyType({
            "schema_version": 1, "runtime_ref": actor_id,
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
    generations = {
        range(1, 8): "generation-1", range(8, 10): "generation-2",
        range(10, 18): "generation-3", range(18, 20): "generation-4",
        range(20, 22): "generation-5",
    }
    observations = {}
    for sequence, operation in operations.items():
        generation = next(value for keys, value in generations.items() if sequence in keys)
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
            "service_generation": generation,
            "invitations": None if state == "stopped" else {
                "active": {"pairing_id": active, "expires_at_ms": 1_788_566_500_000},
                "expired": {"pairing_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "expires_at_ms": 1_788_566_300_000},
            },
            "transition": transition,
        })
    return MappingProxyType(observations)


def _context(case_id="unauthenticated_discovery", facts=None, requests=None):
    if requests is None:
        requests = [
            _request("GET", "/.well-known/teslatlas-hub", 200, "discovery"),
            _request("GET", "/healthz", 200, "health"),
            _request("GET", "/readyz", 200, "readiness"),
        ]
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
    raw = {}
    updated_invocations = []
    for index, invocation in enumerate(context.invocations):
        actor_requests = [] if matrix_contract.CASE_KINDS[case_id] != "http" else [
            dict(request, request_id=f"{request['request_id']}-{index}") for request in requests
        ]
        raw[invocation.evidence_id] = _raw(
            invocation.actor_id, invocation.operation, dict(facts[invocation.actor_id]),
            actor_requests, invocation.session_sequence_before, invocation.session_sequence_after,
        )
        updated_invocations.append(dataclasses.replace(
            invocation, request_ids=tuple(item["request_id"] for item in actor_requests)
        ))
    return dataclasses.replace(
        context, invocations=tuple(updated_invocations), raw=MappingProxyType(raw)
    )


def _case(case_id, facts, context=None):
    transcript = []
    if context is not None:
        for invocation in context.invocations:
            if invocation.case_id == case_id and invocation.evidence_id in context.raw:
                transcript.extend(dict(item) for item in context.raw[invocation.evidence_id]["requests"])
    return {
        "id": case_id,
        "status": "passed",
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
        context = _context("endpoint_restart", requests=[_request()])
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
            contract = matrix_wire.load_phase_contract(TOOLS / f"{actor_id}-phases.json", actor_id)
            self.assertEqual(list(range(1, 7)), [item["ordinal"] for item in contract["phases"]])
            self.assertEqual(
                ["bootstrap_pair", "revoke_and_pair", "restart", "outage_stop", "outage_start", "final_verify"],
                [item["phase_id"] for item in contract["phases"]],
            )
            self.assertTrue(all(set(item["operations"]) <= matrix_wire.OPERATIONS for item in contract["phases"]))
            self.assertEqual("initial_observation", contract["resources"][0]["id"])

    def test_inert_manifest_binds_validator_and_closed_raw_schema(self):
        contract = matrix_contract.validate_manifest(TOOLS / "matrix-contract.json")
        self.assertEqual(["swift-raw-v1"], [item["id"] for item in contract["raw_schemas"]])

    def test_worker_config_has_no_broker_or_command_authority(self):
        config = {
            "schema_version": 1, "kind": "matrix-actor-worker", "actor_id": "swift_macos",
            "session_id": SESSION_ID, "cell_id": "swift__macos_arm64", "instance_nonce": DIGEST,
            "session_input_sha256": DIGEST,
            "phase_contract": {"id": "phase", "root": {"path": "/root", "sha256": DIGEST}, "local": {"path": "/local", "sha256": DIGEST}},
            "inputs": [{"id": "initial_observation", "root": {"path": "/observation", "sha256": DIGEST}, "local": {"path": "/observation", "sha256": DIGEST}}], "private_root": "/private", "coordination_dir": "/coord", "evidence_path": "/evidence", "log_path": "/log",
        }
        matrix_wire.validate_worker_config(config)
        config["broker_socket"] = "/broker"
        with self.assertRaises(matrix_wire.MatrixWireError):
            matrix_wire.validate_worker_config(config)


class CoordinatorTests(unittest.TestCase):
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
            phase_contract = matrix_wire.load_phase_contract(TOOLS / "swift_macos-phases.json", "swift_macos")
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
