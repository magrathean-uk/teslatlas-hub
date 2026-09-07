import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest import mock

from . import installed
from ..installed_hosts import session as installed_session_module
from ..installed_hosts.bounded import Deadline
from ..installed_hosts.session import InstalledSession


SESSION = "11111111-1111-4111-8111-111111111111"


class FakeEvidence:
    state = "closed"
    cleanup_errors = ()
    final_stopped = {"status": "stopped", "service": {"state": "stopped"}}
    journal_path = "/private/journal"
    journal_sha256 = "f" * 64
    local_transport = {"closed": True}


class FakeSession:
    def __init__(self, events, close_error=None):
        self.events = events
        self.close_error = close_error
        self.descriptor = {"schema_version": 1, "kind": "installed-host", "broker_socket": "/private/broker.sock", "session_id": SESSION, "registration_sha256": "e" * 64}

    def request(self, request):
        self.events.append("verify")
        return {"proof": {"status": "verified", "session_id": SESSION, "sequence": 7}}

    def close(self):
        self.events.append("close")
        if self.close_error:
            raise self.close_error
        return FakeEvidence()

    def latest_observation(self, _deadline):
        return {"status": "verified", "session_id": SESSION, "sequence": 8}


class FakeFactory:
    current = None

    @classmethod
    def open(cls, config, inventory):
        return cls.current


class InstalledCompletionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)

    def tearDown(self):
        self.temporary.cleanup()

    def _session_input(self):
        coord = self.root / "coordination"
        coord.mkdir(mode=0o700)
        path = self.root / "session.json"
        value = {
            "schema_version": 1, "kind": "matrix-adapter-session",
            "cell_id": "typescript_node__macos_arm64", "session_id": SESSION,
            "instance_nonce": "c" * 64,
            "outputs": {"normalized": str(self.root / "normalized.json"),
                        "actor_evidence": str(self.root / "actors.json"),
                        "coordination_dir": str(coord),
                        "framework_log": str(self.root / "framework.log")},
            "bounds": {"cell_timeout_ms": 5000, "cleanup_timeout_ms": 45000,
                       "frame_bytes": 1048576, "evidence_bytes": 8388608,
                       "framework_log_bytes": 8388608},
        }
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode() + b"\n"
        path.write_bytes(raw); path.chmod(0o600)
        return path, value, hashlib.sha256(raw).hexdigest()

    def _child(self, session_path, mode="success"):
        script = self.root / ("child-" + mode + ".py")
        script.write_text(
            "import hashlib,json,os,pathlib,sys,time\n"
            "p=pathlib.Path(sys.argv[1]); s=json.loads(p.read_text()); c=pathlib.Path(s['outputs']['coordination_dir'])\n"
            "def write(path,value):\n raw=json.dumps(value,sort_keys=True,separators=(',',':')).encode()+b'\\n'; fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600); os.write(fd,raw); os.fsync(fd); os.close(fd); return {'path':str(path),'sha256':hashlib.sha256(raw).hexdigest()}\n"
            + ("sys.exit(0)\n" if mode == "premature" else
               "n=write(pathlib.Path(s['outputs']['normalized']),{'schema_version':1}); a=write(pathlib.Path(s['outputs']['actor_evidence']),{'schema_version':1}); comp=write(c/'adapter-completion.json',{'schema_version':1,'session_id':s['session_id'],'cell_id':s['cell_id'],'session_input_sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'normalized':n,'actor_evidence':a}); ready={'schema_version':1,'type':'ready','session_id':s['session_id'],'cell_id':s['cell_id'],'session_input_sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'instance_nonce':s['instance_nonce'],'sequence':1,'phase':'evidence_ready','observation':{'session_sequence':8,'proof_sha256':hashlib.sha256(json.dumps({'status':'verified','session_id':s['session_id'],'sequence':8},sort_keys=True,separators=(',',':')).encode()).hexdigest()},'evidence':comp}; write(c/'ready-000001.json',ready);\nwhile not (c/'ack-000001.json').exists(): time.sleep(.01)\nack=json.loads((c/'ack-000001.json').read_text()); sys.exit(0 if ack['action']=='close_completed' else 9)\n"),
            encoding="utf-8",
        )
        return subprocess.Popen([sys.executable, str(script), str(session_path)], start_new_session=True)

    def test_valid_child_is_alive_for_close_then_ack_then_zero(self):
        session_path, value, digest = self._session_input()
        events = []
        process = self._child(session_path)
        result = installed.supervise_completion(
            FakeSession(events), process, session_path, value,
            initial_observation={"session_sequence": 7, "proof_sha256": installed.proof_sha256({"status": "verified", "session_id": SESSION, "sequence": 7})},
            admit=lambda normalized, actors: events.append("admit"),
        )
        events.append("zero")
        self.assertEqual(["admit", "close", "zero"], events)
        self.assertEqual(0, result.exit_code)
        self.assertEqual("closed", result.session_evidence.state)

    def test_premature_exit_and_failed_close_are_permanent_failures(self):
        path, value, _ = self._session_input()
        process = self._child(path, "premature")
        events = []
        with self.assertRaises(installed.InstalledExecutionError):
            installed.supervise_completion(
                FakeSession(events), process, path, value,
                initial_observation={"session_sequence": 7, "proof_sha256": installed.proof_sha256({"status": "verified", "session_id": SESSION, "sequence": 7})},
                admit=lambda *_: None,
            )
        self.assertIn("close", events)
        self.root = Path(tempfile.mkdtemp(dir=self.root)).resolve()
        self.root.chmod(0o700)
        path, value, _ = self._session_input()
        process = self._child(path)
        with self.assertRaises(installed.InstalledExecutionError):
            installed.supervise_completion(
                FakeSession([], close_error=RuntimeError("synthetic close failure")),
                process, path, value,
                initial_observation={"session_sequence": 7, "proof_sha256": installed.proof_sha256({"status": "verified", "session_id": SESSION, "sequence": 7})},
                admit=lambda *_: None,
            )
        rejected = json.loads((Path(value["outputs"]["coordination_dir"]) / "ack-000001.json").read_text())
        self.assertEqual(("rejected", "abort", None), (rejected["status"], rejected["action"], rejected["result"]))

    def test_execute_opens_and_verifies_before_adapter_launch(self):
        path, value, _ = self._session_input()
        value["host_session"] = {"schema_version": 1, "kind": "installed-host", "broker_socket": "/private/broker.sock", "session_id": SESSION, "registration_sha256": "e" * 64}
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode() + b"\n"
        path.write_bytes(raw)
        events = []
        FakeFactory.current = FakeSession(events)

        def build(descriptor, running):
            events.append("build")
            self.assertEqual(value["host_session"], descriptor)
            self.assertEqual(7, running["proof"]["sequence"])
            return path, value

        def launch(session_path, session_input):
            events.append("launch")
            return self._child(session_path)

        result = installed.execute_installed(
            {}, {}, build_session_input=build, launch_adapter=launch,
            admit=lambda *_: events.append("admit"), session_factory=FakeFactory,
        )
        self.assertEqual(0, result.exit_code)
        self.assertEqual(["verify", "build", "launch", "admit", "close"], events)

    def test_controller_observation_accessor_is_locked_bounded_and_detached(self):
        session = InstalledSession.__new__(InstalledSession)
        session._operation_lock = threading.Lock()
        session._observations = [{"sequence": 7, "nested": {"state": "running"}}]
        snapshot = session.observations_snapshot(Deadline(1))
        self.assertEqual(({"sequence": 7, "nested": {"state": "running"}},), snapshot)
        snapshot[0]["nested"]["state"] = "changed"
        self.assertEqual("running", session._observations[0]["nested"]["state"])
        self.assertEqual(7, session.latest_observation(Deadline(1))["sequence"])
        session._operation_lock.acquire()
        try:
            with self.assertRaises(TimeoutError):
                session.observations_snapshot(Deadline(0.01))
        finally:
            session._operation_lock.release()

    def test_open_revalidates_provider_specific_identity_before_transport_use(self):
        order = []

        class Identity:
            def revalidate(self, deadline):
                self.deadline = deadline
                order.append("revalidate-provider")
                return self

        class Lease:
            def acquire(self, timeout):
                self.timeout = timeout
                return self

            def release(self):
                order.append("release-lease")

            def quarantine(self, errors):
                raise AssertionError(errors)

        class Transport:
            def open(self, registered):
                order.append("open-transport")
                return {
                    "schema_version": 1, "type": "challenge",
                    "session_id": registered.config["session_id"],
                    "sequence": 0, "challenge": "a" * 64,
                }

        manifest_path = self.root / "manifest.json"
        manifest_path.write_text("{}", encoding="utf-8")
        binding = {"path": str(self.root / "input"), "sha256": "1" * 64}
        package = {"path": str(self.root / "package"), "sha256": "d" * 64}
        registered = SimpleNamespace(
            registration={"provider": "lima-debian", "provider_tool": {"path": "/opt/homebrew/bin/limactl", "sha256": "9" * 64}},
            config={
                "session_id": SESSION, "host_id": "synthetic-host",
                "lifetime_seconds": 600,
                "local_private_root": str(self.root / "private"),
                "controller_bundle": binding, "package": package,
                "package_manifest": {"path": str(manifest_path), "sha256": "e" * 64},
                "seed": binding, "profile": binding, "scenario": binding,
                "expected": {
                    "product_version": "2026.36.2", "os": "Debian 13",
                    "architecture": "amd64", "hub_executable_sha256": "f" * 64,
                },
            },
        )
        identity = Identity()
        lease = Lease()
        validated_manifest = {
            "package_sha256": "d" * 64, "product_version": "2026.36.2",
            "os": "Debian 13", "architecture": "amd64",
            "hub_executable": {"path": "/usr/bin/teslatlas-hub", "sha256": "f" * 64},
        }
        with mock.patch.object(installed_session_module, "read_registered_config", return_value=registered), \
                mock.patch.object(installed_session_module, "HostLease", return_value=lease), \
                mock.patch.object(installed_session_module, "_hash_regular_file"), \
                mock.patch.object(installed_session_module, "validate_package_manifest", return_value=validated_manifest), \
                mock.patch("hub.tools.interop.installed_hosts.transport._verify_provider_tool", side_effect=lambda registration: order.append("verify-provider") or identity):
            session = InstalledSession.open({}, {}, transport=Transport())
        try:
            self.assertIs(identity, session.provider_tool_identity)
            self.assertIsInstance(identity.deadline, Deadline)
            self.assertEqual(["verify-provider", "revalidate-provider", "open-transport"], order)
        finally:
            session._close_broker()
            session.journal.close()
            lease.release()


if __name__ == "__main__":
    unittest.main()
