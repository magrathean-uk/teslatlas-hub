# SPDX-License-Identifier: AGPL-3.0-only
import hashlib
import json
import os
import socket
import tempfile
import time
import unittest
from pathlib import Path

from tools.interop.installed_hosts.contract import ContractError
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

    def open(self, registered):
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
            self.assertTrue(transport.closed)

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
                request = {
                    "schema_version": 1, "session_id": descriptor["session_id"],
                    "sequence": 1, "challenge": greeting["challenge"], "op": "verify",
                }
                client.sendall(json.dumps(request).encode() + b"\n")
                reply = json.loads(client.makefile("rb").readline())
                self.assertEqual(reply["type"], "reply")
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
            result = session.advance_once_for_ha()
            self.assertEqual(result["advance"]["after_store_sha256"], "8" * 64)
            self.assertEqual(transport.requests[-1]["op"], "advance-once")
            session.close()

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
