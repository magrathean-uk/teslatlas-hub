# SPDX-License-Identifier: AGPL-3.0-only
import hashlib
import json
import os
import socket
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

from tools.interop.installed_hosts.contract import ContractError
from tools.interop.installed_hosts.bounded import Deadline
from tools.interop.installed_hosts import session as session_module
from tools.interop.installed_hosts.session import CleanupError, InstalledSession, SessionError
from tools.interop.installed_hosts.guest import GuestController
from tools.interop.installed_hosts.test_contract import config, registration
from tools.interop.installed_hosts.test_platform_observation import observation, registered


CHALLENGE_0 = "a" * 64


class FakeTransport:
    def __init__(self, fail=None, stale_reply=False, reuse_generation=False):
        self.fail = fail
        self.stale_reply = stale_reply
        self.reuse_generation = reuse_generation
        self.requests = []
        self.closed = False
        self.registered = None
        self.sequence = 0
        self.challenge = CHALLENGE_0
        self.running = True
        self.generation = 1

    def open(self, registered, deadline=None):
        self.registered = registered
        if self.fail == "open":
            raise RuntimeError("synthetic open failure")
        self.public_certificate_path = str(Path(registered.config["local_private_root"]) / "fake-public-ca.pem")
        self.public_certificate_der_sha256 = "1" * 64
        Path(self.public_certificate_path).parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        Path(self.public_certificate_path).write_bytes(b"synthetic public certificate")
        os.chmod(self.public_certificate_path, 0o600)
        return {"schema_version": 1, "type": "challenge", "session_id": registered.config["session_id"], "sequence": 0, "challenge": self.challenge}

    def exchange(self, request, timeout):
        self.requests.append(dict(request))
        self.sequence = request["sequence"]
        op = request["op"]
        if self.fail == op:
            raise RuntimeError("synthetic {} failure".format(op))
        if op == "stop":
            self.running = False
            result = {"stopped": True, "events": [{"operation": "stop", "forced_escalation": False}]}
        else:
            if op in ("start", "pair", "revoke", "advance-once"):
                self.running = True
                if not self.reuse_generation:
                    self.generation += 1
            proof = observation(start="{}:1".format(self.generation))
            proof["host"]["host_id"] = self.registered.config["host_id"]
            proof["sequence"] = request["sequence"]
            proof["challenge"] = request["challenge"]
            def invitation(pairing_id, expires):
                endpoint = "https://127.0.0.1:18480"
                secret = "private"
                pin = "1" * 64
                return {
                    "pairingId": pairing_id, "secret": secret, "expiresAtMs": expires,
                    "endpoint": endpoint, "tlsPin": pin,
                    "pairingUri": "teslatlas-hub://pair?endpoint=https%3A%2F%2F127.0.0.1%3A18480&pairing_id={}&secret={}&tls_pin={}".format(pairing_id, secret, pin),
                }
            now = int(time.time() * 1000)
            result = {
                "proof": proof,
                "invitation": invitation("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", now + 900_000),
                "expired_invitation": invitation("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", now - 1),
                "events": [{"operation": op}],
            }
            if op == "advance-once":
                result["advance"] = {"before_store_sha256": "7" * 64, "after_store_sha256": "8" * 64, "scenario_sha256": "3" * 64, "seed_sha256": "1" * 64}
        next_challenge = "{:064x}".format(self.sequence + 10)
        return {
            "schema_version": 1,
            "type": "reply",
            "session_id": self.registered.config["session_id"],
            "sequence": self.sequence - 1 if self.stale_reply else self.sequence,
            "challenge": next_challenge,
            "result": result,
        }

    def verify_stopped(self, registered, timeout):
        if self.fail == "verify_stopped":
            raise RuntimeError("synthetic stopped verifier failure")
        acquired = observation(start="{}:1".format(self.generation))["service"]
        operation = next((item for item in reversed(self.requests) if item["op"] == "stop"), None)
        stop_evidence = {
            "session_id": registered.config["session_id"], "pid": acquired["hub"]["pid"],
            "operation": "stop", "hub_start_identity": acquired["hub"]["start_identity"],
            "control_group": acquired["control_group"], "result_raw": "success",
            "invocation_id": acquired["invocation_id"], "exec_main_code_raw": "1",
            "exec_main_code_semantic": "exited", "exec_main_status_raw": "0", "normal_exit": True,
            "operation_sequence": operation["sequence"] if operation else self.sequence,
            "operation_challenge": operation["challenge"] if operation else self.challenge,
            "acquired_service_sha256": hashlib.sha256(json.dumps(acquired, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
        }

        return {
            "schema_version": 1,
            "status": "stopped",
            "session_id": registered.config["session_id"],
            "host_id": registered.config["host_id"],
            "service": {"state": "stopped", "generation": None, "forced_escalation": False, "normal_exit": True, "owned_generation": {"service": acquired, "stop_evidence": stop_evidence}, "cleanup_errors": []},
            "listener": {"host": "127.0.0.1", "port": 18480, "owner_pid": None},
        }

    def finish_writer(self, timeout):
        return True

    def close(self, timeout=None, preserve_recovery=False):
        self.closed = True
        if self.fail == "close":
            raise RuntimeError("synthetic transport close failure")


def write_binding(directory, name, payload):
    path = directory / name
    path.write_bytes(payload)
    return {"path": str(path), "sha256": hashlib.sha256(payload).hexdigest()}


def inputs(directory):
    provider_id = "fresh-debian-" + hashlib.sha256(str(directory).encode()).hexdigest()[:12]
    reg = registration("fresh-debian")
    reg["provider"] = "lima-debian"
    reg["provider_tool"]["path"] = "/opt/homebrew/bin/limactl"
    reg["provider_id"] = provider_id
    reg["host_id"] = provider_id
    reg["ssh"]["host_alias"] = "lima-" + provider_id
    reg["guest"].update(
        os="Debian 13",
        architecture="amd64",
        native_or_emulated="emulated",
        permitted_service_user="teslatlas",
        permitted_service_uid=997,
        machine_identity_sha256="4" * 64,
    )
    reg["guest"]["python"]["sha256"] = "6" * 64
    reg["guest"]["python_process"]["sha256"] = "6" * 64
    reg["lease"]["resource_id"] = provider_id
    reg_raw = json.dumps(reg, sort_keys=True, separators=(",", ":")).encode()
    reg_binding = write_binding(directory, "registration.json", reg_raw)
    cfg = config(reg_binding["path"], reg_binding["sha256"])
    cfg["host_id"] = provider_id
    cfg["expected"].update(os="Debian 13", architecture="amd64", native_or_emulated="emulated", service_mode="installed-deb-systemd")
    cfg["cell_id"] = "protocol_actual_hub__debian13_amd64"
    cfg["guest_run_id"] = "run-1/protocol_actual_hub__debian13_amd64"
    cfg["expected"]["hub_executable_sha256"] = "f" * 64
    cfg["guest_run_id"] = "run-1/cell-1"
    cfg["local_private_root"] = str(directory / "private")
    for key, name in (
        ("controller_bundle", "controller.tar"), ("package", "package.deb"),
        ("package_manifest", "manifest.json"), ("seed", "seed"),
        ("profile", "profile"), ("scenario", "scenario.json"),
    ):
        cfg[key] = write_binding(directory, name, key.encode())
    # Match the hand-derived synthetic observation literals.
    cfg["controller_bundle"]["sha256"] = "c" * 64
    cfg["package"]["sha256"] = "d" * 64
    cfg["package_manifest"]["sha256"] = "e" * 64
    cfg["seed"]["sha256"] = "1" * 64
    cfg["profile"]["sha256"] = "2" * 64
    cfg["scenario"]["sha256"] = "3" * 64
    return cfg, {provider_id: reg_binding["sha256"]}


class SessionTests(unittest.TestCase):
    def open_session(self, directory, transport=None):
        cfg, inventory = inputs(directory)
        session = InstalledSession.open(cfg, inventory, transport=transport or FakeTransport(), verify_local_inputs=False)
        self.addCleanup(session._host_lease.release)
        self.addCleanup(session.journal.close)
        self.addCleanup(session._close_broker)
        return session

    def test_requests_add_fresh_guest_nonce_and_reject_stale_reply(self):
        with tempfile.TemporaryDirectory() as raw:
            transport = FakeTransport(stale_reply=True)
            session = self.open_session(Path(raw), transport)
            with self.assertRaisesRegex(SessionError, "sequence"):
                session.request({"op": "verify"})
            self.assertEqual(transport.requests[0]["challenge"], CHALLENGE_0)
            with self.assertRaises(CleanupError):
                session.close()

    def test_open_passes_the_enclosing_deadline_to_transport(self):
        class DeadlineTransport(FakeTransport):
            def open(self, registered, deadline):
                self.open_deadline = deadline
                return super().open(registered)

        with tempfile.TemporaryDirectory() as raw:
            cfg, inventory = inputs(Path(raw))
            deadline = Deadline(1)
            transport = DeadlineTransport()
            session = InstalledSession.open(
                cfg, inventory, transport=transport, verify_local_inputs=False, deadline=deadline,
            )
            try:
                self.assertIs(deadline, transport.open_deadline)
            finally:
                session.close()
            self.assertTrue(transport.closed)

    def test_open_stops_local_input_hashing_when_deadline_expires(self):
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            cfg, inventory = inputs(directory)
            transport = FakeTransport()
            deadline = Deadline(0.01)
            manifest = {
                "package_sha256": "d" * 64,
                "product_version": "2026.36.2",
                "os": "Debian 13",
                "architecture": "amd64",
                "hub_executable": {"path": "/usr/bin/teslatlas-hub", "sha256": "f" * 64},
            }

            def slow_hash(_binding, _label, deadline=None):
                deadline.remaining()
                time.sleep(0.02)
                deadline.remaining()

            with mock.patch("tools.interop.installed_hosts.transport._verify_provider_tool", return_value=None), \
                    mock.patch.object(session_module, "_hash_regular_file", side_effect=slow_hash), \
                    mock.patch.object(session_module, "_read_json_file", return_value=manifest), \
                    mock.patch.object(session_module, "validate_package_manifest", return_value=manifest):
                with self.assertRaises(TimeoutError):
                    InstalledSession.open(
                        cfg, inventory, transport=transport, verify_local_inputs=True, deadline=deadline,
                    )

            self.assertIsNone(transport.registered)

    def test_copied_proof_from_another_session_is_not_admitted(self):
        with tempfile.TemporaryDirectory() as raw:
            session = self.open_session(Path(raw))
            original = session.transport.exchange

            def copied(request, timeout):
                reply = original(request, timeout)
                reply["result"]["proof"]["session_id"] = "22222222-2222-4222-8222-222222222222"
                return reply

            session.transport.exchange = copied
            with self.assertRaisesRegex(ContractError, "owned session"):
                session.request({"op": "verify"})
            with self.assertRaises(CleanupError):
                session.close()

    def test_stale_proof_inside_a_fresh_guest_reply_is_not_admitted(self):
        with tempfile.TemporaryDirectory() as raw:
            session = self.open_session(Path(raw))
            original = session.transport.exchange

            def stale_proof(request, timeout):
                reply = original(request, timeout)
                reply["result"]["proof"]["challenge"] = "f" * 64
                return reply

            session.transport.exchange = stale_proof
            with self.assertRaisesRegex(SessionError, "current guest request"):
                session.request({"op": "verify"})
            with self.assertRaises(CleanupError):
                session.close()

    def test_restart_requires_a_changed_kernel_generation_even_when_pid_changes(self):
        with tempfile.TemporaryDirectory() as raw:
            session = self.open_session(Path(raw), FakeTransport(reuse_generation=True))
            before = session.request({"op": "verify"})
            session.request({"op": "stop"})
            with self.assertRaisesRegex(SessionError, "generation did not change"):
                session.request({"op": "start"})
            self.assertEqual(before["descriptor"]["hub_started_at"], "1:1")
            with self.assertRaises(CleanupError):
                session.close()

    def test_pair_requires_the_internal_stop_start_to_change_generation(self):
        with tempfile.TemporaryDirectory() as raw:
            session = self.open_session(Path(raw), FakeTransport(reuse_generation=True))
            session.request({"op": "verify"})
            with self.assertRaisesRegex(SessionError, "generation did not change after pair"):
                session.request({"op": "pair"})
            with self.assertRaises(CleanupError):
                session.close()

    def test_private_broker_accepts_one_peer_and_closes_on_duplicate_sequence(self):
        with tempfile.TemporaryDirectory() as raw:
            session = self.open_session(Path(raw))
            descriptor = session.descriptor
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                client.connect(descriptor["broker_socket"])
                greeting = json.loads(client.makefile("rb").readline())
                self.assertEqual(greeting["challenge"], CHALLENGE_0)
                request = {
                    "schema_version": 1, "session_id": descriptor["session_id"],
                    "sequence": 1, "challenge": greeting["challenge"], "op": "verify",
                }
                client.sendall(json.dumps(request).encode() + b"\n")
                reply = json.loads(client.makefile("rb").readline())
                self.assertEqual(reply["type"], "reply")
                self.assertEqual(reply["result"]["proof"]["challenge"], request["challenge"])
                self.assertEqual(reply["challenge"], session._guest_challenge)
                client.sendall(json.dumps(request).encode() + b"\n")
                error = json.loads(client.makefile("rb").readline())
                self.assertEqual(error["error"]["code"], "invalid-request")
                self.assertEqual(client.makefile("rb").readline(), b"")
            with self.assertRaises(CleanupError) as captured:
                session.close()
            self.assertEqual(captured.exception.evidence.state, "failed")

    def test_private_broker_distinguishes_operation_failure_from_invalid_request(self):
        with tempfile.TemporaryDirectory() as raw:
            session = self.open_session(Path(raw), FakeTransport(fail="verify"))
            descriptor = session.descriptor
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                client.connect(descriptor["broker_socket"])
                stream = client.makefile("rb")
                greeting = json.loads(stream.readline())
                request = {
                    "schema_version": 1, "session_id": descriptor["session_id"],
                    "sequence": 1, "challenge": greeting["challenge"], "op": "verify",
                }
                client.sendall(json.dumps(request).encode() + b"\n")
                error = json.loads(stream.readline())
                self.assertEqual(error["error"], {"code": "operation-failed"})
                self.assertEqual(stream.readline(), b"")
            with self.assertRaises(CleanupError):
                session.close()

    def test_interrupted_mutation_is_journaled_and_cleanup_still_runs(self):
        with tempfile.TemporaryDirectory() as raw:
            transport = FakeTransport(fail="pair")
            session = self.open_session(Path(raw), transport)
            session.request({"op": "verify"})
            with self.assertRaisesRegex(RuntimeError, "pair failure"):
                session.request({"op": "pair"})
            with self.assertRaises(CleanupError) as captured:
                session.close()
            entries = [json.loads(line) for line in Path(captured.exception.evidence.journal_path).read_text().splitlines()]
            self.assertTrue(any(item["kind"] == "intent" and item["operation"] == "pair" for item in entries))
            self.assertTrue(any(item["kind"] == "failure" and item["operation"] == "pair" for item in entries))
            self.assertTrue(transport.closed)

    def test_failed_dispatched_operation_has_a_redacted_public_record(self):
        with tempfile.TemporaryDirectory() as raw:
            session = self.open_session(Path(raw), FakeTransport(fail="verify"))
            with self.assertRaisesRegex(RuntimeError, "verify failure"):
                session.request({"op": "verify"})

            records = session.operation_records_snapshot(Deadline(1))

            self.assertEqual(1, len(records))
            self.assertEqual(
                {
                    "operation": "verify",
                    "request": {"op": "verify"},
                    "request_binding": {"op": "verify", "sequence": 1, "challenge": CHALLENGE_0},
                    "processed_binding": None,
                    "status": "failed",
                    "failure": "RuntimeError",
                    "result_binding": None,
                    "result_sha256": None,
                    "proof": None,
                    "invitation": None,
                    "expired_invitation": None,
                    "advance": None,
                    "events_sha256": None,
                    "final_stopped": None,
                },
                {key: records[0][key] for key in (
                    "operation", "request", "request_binding", "processed_binding", "status", "failure", "result_binding",
                    "result_sha256", "proof", "invitation", "expired_invitation", "advance",
                    "events_sha256", "final_stopped",
                )},
            )
            self.assertGreaterEqual(records[0]["finished_monotonic_ns"], records[0]["started_monotonic_ns"])
            self.assertNotIn("synthetic verify failure", json.dumps(records, sort_keys=True))
            with self.assertRaises(CleanupError):
                session.close()

    def test_operation_record_snapshot_retains_processed_public_facts_without_secrets(self):
        with tempfile.TemporaryDirectory() as raw:
            session = self.open_session(Path(raw))
            verified = session.request({"op": "verify"})
            session.request({"op": "stop"})

            records = session.operation_records_snapshot(Deadline(1))

            self.assertEqual(["verify", "stop"], [item["operation"] for item in records])
            verify, stop = records
            self.assertEqual({"op": "verify"}, verify["request"])
            self.assertEqual(
                {"op": "verify", "sequence": 1, "challenge": CHALLENGE_0},
                verify["request_binding"],
            )
            self.assertEqual(verify["request_binding"], verify["processed_binding"])
            self.assertEqual(verified["proof"], verify["proof"])
            self.assertEqual(
                {
                    "pairing_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    "endpoint": "https://127.0.0.1:18480",
                    "tls_pin": "1" * 64,
                },
                {key: verify["invitation"][key] for key in ("pairing_id", "endpoint", "tls_pin")},
            )
            self.assertIsInstance(verify["invitation"]["expires_at_ms"], int)
            self.assertEqual(stop["request_binding"], stop["processed_binding"])
            self.assertIsNone(stop["proof"])
            self.assertGreaterEqual(verify["finished_monotonic_ns"], verify["started_monotonic_ns"])
            self.assertIsInstance(verify["observed_at_ms"], int)
            result_binding = verify["result_binding"]
            persisted_result = json.loads(Path(result_binding["path"]).read_text(encoding="utf-8"))
            self.assertEqual(result_binding["sha256"], verify["result_sha256"])
            self.assertEqual(result_binding["sha256"], hashlib.sha256(Path(result_binding["path"]).read_bytes()).hexdigest())
            self.assertEqual("private", persisted_result["invitation"]["secret"])
            self.assertNotIn("descriptor", persisted_result)
            self.assertNotIn('"secret":"private"', json.dumps(records, sort_keys=True))
            self.assertIsInstance(verify["result_sha256"], str)
            self.assertEqual(64, len(verify["result_sha256"]))

            journal = [
                json.loads(line)
                for line in Path(session.journal.path).read_text(encoding="utf-8").splitlines()
            ]
            retained = [item for item in journal if item.get("kind") == "operation-record"]
            self.assertEqual(2, len(retained))
            self.assertEqual("admitted", retained[0]["status"])
            self.assertEqual(verify["request_binding"], retained[0]["request_binding"])
            self.assertEqual(verify["processed_binding"], retained[0]["processed_binding"])
            self.assertEqual(verify["result_sha256"], retained[0]["result_binding"]["sha256"])
            self.assertNotIn("private", json.dumps(retained, sort_keys=True))

            records[0]["proof"]["service"]["generation"] = "changed"
            self.assertNotEqual(
                "changed",
                session.operation_records_snapshot(Deadline(1))[0]["proof"]["service"]["generation"],
            )

    def test_request_does_not_return_success_after_deadline_expires_during_retention(self):
        with tempfile.TemporaryDirectory() as raw:
            session = self.open_session(Path(raw))
            original = session_module.durable_bytes

            def slow_durable_bytes(path, payload):
                time.sleep(0.02)
                return original(path, payload)

            with mock.patch.object(session_module, "durable_bytes", side_effect=slow_durable_bytes):
                with self.assertRaises(TimeoutError):
                    session.request({"op": "verify"}, deadline=Deadline(0.01))

            records = session.operation_records_snapshot(Deadline(1))
            self.assertEqual("failed", records[-1]["status"])
            self.assertEqual("TimeoutError", records[-1]["failure"])
            with self.assertRaises(CleanupError):
                session.close()

    def test_close_expired_between_stages_keeps_cleanup_errors_and_failed_evidence(self):
        with tempfile.TemporaryDirectory() as raw:
            transport = FakeTransport()
            session = self.open_session(Path(raw), transport)
            deadline = Deadline(1)
            calls = {"count": 0}

            def expires_between_stages(cap=None):
                calls["count"] += 1
                if calls["count"] > 3:
                    raise TimeoutError("synthetic cleanup deadline")
                return min(0.01, cap) if cap is not None else 0.01

            deadline.remaining = expires_between_stages
            with self.assertRaises(CleanupError) as captured:
                session.close(deadline=deadline)

            evidence = captured.exception.evidence
            self.assertEqual("failed", evidence.state)
            self.assertTrue(transport.closed)
            self.assertTrue(any(item["error"] == "TimeoutError" for item in evidence.cleanup_errors))
            self.assertIs(evidence, session._final_evidence)

    def test_final_stopped_skips_failed_or_unprocessed_stop_records(self):
        with tempfile.TemporaryDirectory() as raw:
            transport = FakeTransport(fail="stop")
            session = self.open_session(Path(raw), transport)
            with self.assertRaises(CleanupError) as captured:
                session.close()

            serialized = json.dumps(captured.exception.evidence.cleanup_errors, sort_keys=True)
            self.assertNotIn("NoneType", serialized)
            self.assertNotIn("not subscriptable", serialized)

    def test_cleanup_stop_record_binds_the_independent_stopped_proof(self):
        with tempfile.TemporaryDirectory() as raw:
            session = self.open_session(Path(raw))
            session.request({"op": "verify"})

            evidence = session.close()
            stop = session.operation_records_snapshot(Deadline(1))[-1]
            stopped = evidence.final_stopped["service"]["owned_generation"]["stop_evidence"]

            self.assertEqual("stop", stop["operation"])
            self.assertEqual(evidence.final_stopped, stop["final_stopped"])
            self.assertEqual(stopped["operation_sequence"], stop["processed_binding"]["sequence"])
            self.assertEqual(stopped["operation_challenge"], stop["processed_binding"]["challenge"])

    def test_close_clamps_transport_cleanup_to_the_supplied_deadline(self):
        class DeadlineCloseTransport(FakeTransport):
            def close(self, timeout=None, preserve_recovery=False):
                self.close_timeout = timeout
                return super().close(timeout=timeout, preserve_recovery=preserve_recovery)

        with tempfile.TemporaryDirectory() as raw:
            transport = DeadlineCloseTransport()
            session = self.open_session(Path(raw), transport)
            session.request({"op": "verify"})
            cleanup_deadline = Deadline(1)

            session.close(deadline=cleanup_deadline)

            self.assertGreater(transport.close_timeout, 0)
            self.assertLessEqual(transport.close_timeout, 1)

    def test_cleanup_attempts_transport_close_after_stop_and_verifier_failures(self):
        with tempfile.TemporaryDirectory() as raw:
            transport = FakeTransport(fail="stop")
            session = self.open_session(Path(raw), transport)
            with self.assertRaises(CleanupError) as captured:
                session.close()
            self.assertTrue(transport.closed)
            self.assertEqual(captured.exception.evidence.state, "failed")
            self.assertGreaterEqual(len(captured.exception.evidence.cleanup_errors), 1)

    def test_partial_open_failure_closes_transport_and_removes_broker(self):
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            cfg, inventory = inputs(directory)
            transport = FakeTransport(fail="open")
            with self.assertRaisesRegex(RuntimeError, "open failure"):
                InstalledSession.open(cfg, inventory, transport=transport, verify_local_inputs=False)
            self.assertTrue(transport.closed)
            sockets = list((directory / "private").glob("**/*.sock")) if (directory / "private").exists() else []
            self.assertEqual(sockets, [])

    def test_ha_advance_once_is_private_and_cell_bound(self):
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            cfg, inventory = inputs(directory)
            session = InstalledSession.open(cfg, inventory, transport=FakeTransport(), verify_local_inputs=False)
            with self.assertRaisesRegex(SessionError, "Home Assistant cell"):
                session.advance_once_for_ha()
            session.close()
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            cfg, inventory = inputs(directory)
            cfg["cell_id"] = "home_assistant__debian13_amd64"
            cfg["adapter_id"] = "home_assistant"
            cfg["client_id"] = "home_assistant"
            transport = FakeTransport()
            session = InstalledSession.open(cfg, inventory, transport=transport, verify_local_inputs=False)
            advance_deadline = Deadline(1)
            result = session.advance_once_for_ha(deadline=advance_deadline)
            self.assertEqual(result["advance"]["after_store_sha256"], "8" * 64)
            self.assertEqual(transport.requests[-1]["op"], "advance-once")
            records = session.operation_records_snapshot(Deadline(1))
            self.assertEqual(["verify", "advance-once"], [item["operation"] for item in records])
            advance = records[-1]
            self.assertEqual(advance["request_binding"], advance["processed_binding"])
            self.assertEqual(
                {
                    "before_store_sha256": "7" * 64,
                    "after_store_sha256": "8" * 64,
                    "scenario_sha256": "3" * 64,
                    "seed_sha256": "1" * 64,
                },
                advance["advance"],
            )
            session.close()

    def test_failed_ha_advance_has_a_redacted_public_record(self):
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            cfg, inventory = inputs(directory)
            cfg["cell_id"] = "home_assistant__debian13_amd64"
            cfg["adapter_id"] = "home_assistant"
            cfg["client_id"] = "home_assistant"
            session = InstalledSession.open(
                cfg, inventory, transport=FakeTransport(fail="advance-once"), verify_local_inputs=False,
            )
            with self.assertRaisesRegex(RuntimeError, "advance-once failure"):
                session.advance_once_for_ha(deadline=Deadline(1))

            records = session.operation_records_snapshot(Deadline(1))

            self.assertEqual(["verify", "advance-once"], [item["operation"] for item in records])
            failed = records[-1]
            self.assertEqual("failed", failed["status"])
            self.assertEqual("RuntimeError", failed["failure"])
            self.assertEqual({"op": "advance-once"}, {"op": failed["request"]["op"]})
            self.assertIsNone(failed["result_binding"])
            with self.assertRaises(CleanupError):
                session.close()
            session.journal.close()
            session._host_lease.release()

    def test_guest_controller_rejects_unknown_revoke_and_cleans_interrupted_pair(self):
        class Platform:
            def __init__(self):
                self.running = False
                self.stop_calls = 0
                self.fail_pair = False
                self.private = {"ownership_path": "/tmp/teslatlas-installed-host-synthetic-ownership.json"}

            def preflight(self):
                return {"stopped": True}

            def prepare(self):
                return {"invitation": {"secret": "normal"}, "expired_invitation": {"secret": "expired"}, "seed_facts": {"store_id": "hub-uuid-1", "store_schema_version": 59, "scenario_sha256": "3" * 64}}

            def start(self):
                self.running = True

            def stop(self):
                self.stop_calls += 1
                self.running = False
                service=observation(start="1:1")["service"]
                return {"operation":"stop","session_id":registered("Debian 13").config["session_id"],"pid":service["hub"]["pid"],"control_group":service["control_group"],"result_raw":"success","exec_main_code_raw":"1","exec_main_status_raw":"0","normal_exit":True}

            def pair(self, seconds=900):
                if self.fail_pair:
                    raise RuntimeError("synthetic pair interruption")
                return {"secret": "next"}

            def revoke(self, device_id):
                raise AssertionError("unknown device reached platform")

            def observe(self):
                return observation(start="1:1")

            def paired_device_ids(self):
                return set()

            def verify_stopped(self, owned=None, initial=False):
                return {"status": "stopped"}

        platform = Platform()
        controller = GuestController(registered("Debian 13"), platform, challenge_factory=lambda: "a" * 64)
        greeting = controller.open()
        unknown = {
            "schema_version": 1, "session_id": controller.registered.config["session_id"], "sequence": 1,
            "challenge": greeting["challenge"], "op": "revoke",
            "device_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "budget_ms": 1000,
        }
        with self.assertRaisesRegex(SessionError, "not claimed"):
            controller.handle(unknown)
        controller = GuestController(registered("Debian 13"), platform, challenge_factory=lambda: "b" * 64)
        greeting = controller.open()
        controller.handle({"schema_version": 1, "session_id": controller.registered.config["session_id"], "sequence": 1, "challenge": greeting["challenge"], "op": "verify", "budget_ms": 1000})
        platform.fail_pair = True
        request = {"schema_version": 1, "session_id": controller.registered.config["session_id"], "sequence": 2, "challenge": controller.challenge, "op": "pair", "budget_ms": 1000}
        with self.assertRaisesRegex(RuntimeError, "pair interruption"):
            controller.handle(request)
        controller.close()
        self.assertFalse(platform.running)
        self.assertGreaterEqual(platform.stop_calls, 1)
        Path(platform.private["ownership_path"]).unlink(missing_ok=True)


if __name__ == "__main__":
    unittest.main()
