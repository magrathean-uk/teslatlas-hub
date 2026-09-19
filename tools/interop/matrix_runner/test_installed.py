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
    def __init__(self, events, close_error=None, close_delay=0):
        self.events = events
        self.close_error = close_error
        self.close_delay = close_delay
        self.request_deadline = None
        self.close_deadline = None
        self.descriptor = {"schema_version": 1, "kind": "installed-host", "broker_socket": "/private/broker.sock", "session_id": SESSION, "registration_sha256": "e" * 64}

    def request(self, request, deadline=None):
        self.events.append("verify")
        self.request_deadline = deadline
        return {"proof": {"status": "verified", "session_id": SESSION, "sequence": 7}}

    def close(self, deadline=None):
        self.events.append("close")
        self.close_deadline = deadline
        if self.close_delay:
            time.sleep(self.close_delay)
        if self.close_error:
            raise self.close_error
        return FakeEvidence()

    def latest_observation(self, _deadline):
        return {"status": "verified", "session_id": SESSION, "sequence": 8}


class FakeFactory:
    current = None

    @classmethod
    def open(cls, config, inventory, deadline=None):
        cls.current.open_deadline = deadline
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
            admit=lambda normalized, actors, deadline=None: events.append("admit"),
        )
        events.append("zero")
        self.assertEqual(["admit", "close", "zero"], events)
        self.assertEqual(0, result.exit_code)
        self.assertEqual("closed", result.session_evidence.state)

    def test_home_assistant_runs_initial_advance_barrier_then_final_close(self):
        path, value, _ = self._session_input()
        value.update(adapter_id="home_assistant", client_id="home_assistant",
                     cell_id="home_assistant__macos_arm64")
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode() + b"\n"
        path.write_bytes(raw)
        initial_proof = {"status": "verified", "session_id": SESSION, "sequence": 7}
        advanced_proof = {"status": "verified", "session_id": SESSION, "sequence": 8}
        initial_observation = {"session_sequence": 7, "proof_sha256": installed.proof_sha256(initial_proof)}
        advanced_observation = {"session_sequence": 8, "proof_sha256": installed.proof_sha256(advanced_proof)}
        script = self.root / "child-ha.py"
        script.write_text(
            "import hashlib,json,os,pathlib,sys,time\n"
            "p=pathlib.Path(sys.argv[1]); s=json.loads(p.read_text()); c=pathlib.Path(s['outputs']['coordination_dir'])\n"
            "def write(path,value):\n raw=json.dumps(value,sort_keys=True,separators=(',',':')).encode()+b'\\n'; fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600); os.write(fd,raw); os.fsync(fd); os.close(fd); return {'path':str(path),'sha256':hashlib.sha256(raw).hexdigest()}\n"
            "sh=hashlib.sha256(p.read_bytes()).hexdigest(); initial={'session_id':s['session_id'],'observation':{'session_sequence':7,'proof_sha256':'"
            + initial_observation["proof_sha256"] + "'}}; witness=write(c/'initial-witness.json',initial); ready={'schema_version':1,'type':'ready','session_id':s['session_id'],'cell_id':s['cell_id'],'session_input_sha256':sh,'instance_nonce':s['instance_nonce'],'sequence':1,'phase':'ha_initial_observation_ready','observation':initial['observation'],'evidence':witness}; write(c/'ready-000001.json',ready)\n"
            "while not (c/'ack-000001.json').exists(): time.sleep(.01)\n"
            "n=write(pathlib.Path(s['outputs']['normalized']),{'schema_version':1}); a=write(pathlib.Path(s['outputs']['actor_evidence']),{'schema_version':1}); comp=write(c/'adapter-completion.json',{'schema_version':1,'session_id':s['session_id'],'cell_id':s['cell_id'],'session_input_sha256':sh,'normalized':n,'actor_evidence':a}); ready={'schema_version':1,'type':'ready','session_id':s['session_id'],'cell_id':s['cell_id'],'session_input_sha256':sh,'instance_nonce':s['instance_nonce'],'sequence':2,'phase':'evidence_ready','observation':{'session_sequence':8,'proof_sha256':'"
            + advanced_observation["proof_sha256"] + "'},'evidence':comp}; write(c/'ready-000002.json',ready)\n"
            "while not (c/'ack-000002.json').exists(): time.sleep(.01)\n"
            "sys.exit(0 if json.loads((c/'ack-000002.json').read_text())['action']=='close_completed' else 9)\n",
            encoding="utf-8",
        )

        class FakeHA(FakeSession):
            def __init__(self):
                super().__init__([])
                self.advance_calls = 0

            def advance_once_for_ha(self, deadline=None):
                self.advance_calls += 1
                return {"proof": advanced_proof, "advance": {
                    "before_store_sha256": "1" * 64, "after_store_sha256": "2" * 64,
                    "scenario_sha256": "3" * 64, "seed_sha256": "4" * 64,
                }}

            def latest_observation(self, _deadline):
                return advanced_proof

        session = FakeHA()
        process = subprocess.Popen([sys.executable, str(script), str(path)], start_new_session=True)
        try:
            result = installed.supervise_completion(
                session, process, path, value,
                initial_observation=initial_observation,
                admit=lambda *_args, **_kwargs: None,
            )
            self.assertEqual(1, session.advance_calls)
            self.assertEqual(2, result.ack["sequence"])
            first_ack = Path(value["outputs"]["coordination_dir"]) / "ack-000001.json"
            self.assertEqual("advance_once", json.loads(first_ack.read_text())["action"])
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=2)

    def test_premature_exit_and_failed_close_are_permanent_failures(self):
        path, value, _ = self._session_input()
        process = self._child(path, "premature")
        events = []
        with self.assertRaises(installed.InstalledExecutionError):
            installed.supervise_completion(
                FakeSession(events), process, path, value,
                initial_observation={"session_sequence": 7, "proof_sha256": installed.proof_sha256({"status": "verified", "session_id": SESSION, "sequence": 7})},
                admit=lambda *_args, deadline=None: None,
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
                admit=lambda *_args, deadline=None: None,
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

        def build(descriptor, running, deadline=None):
            events.append("build")
            self.assertIsInstance(deadline, Deadline)
            self.assertEqual(value["host_session"], descriptor)
            self.assertEqual(7, running["proof"]["sequence"])
            return path, value

        def launch(session_path, session_input, deadline=None, session=None, running=None):
            events.append("launch")
            self.assertIsInstance(deadline, Deadline)
            self.assertIs(session, FakeFactory.current)
            self.assertEqual(7, running["proof"]["sequence"])
            return self._child(session_path)

        result = installed.execute_installed(
            {}, {}, build_session_input=build, launch_adapter=launch,
            admit=lambda *_args, **kwargs: (self.assertIsInstance(kwargs["deadline"], Deadline), events.append("admit")),
            session_factory=FakeFactory, cell_timeout_ms=5000,
        )
        self.assertEqual(0, result.exit_code)
        self.assertEqual(["verify", "build", "launch", "admit", "close"], events)
        self.assertIsInstance(FakeFactory.current.open_deadline, Deadline)
        self.assertIs(FakeFactory.current.request_deadline, FakeFactory.current.open_deadline)
        self.assertIsInstance(FakeFactory.current.close_deadline, Deadline)

    def test_execute_deadline_starts_before_open_and_prevents_late_launch(self):
        path, value, _ = self._session_input()
        value["host_session"] = FakeSession([]).descriptor
        events = []
        session = FakeSession(events)

        class SlowFactory:
            @classmethod
            def open(cls, config, inventory, deadline=None):
                self.assertIsInstance(deadline, Deadline)
                time.sleep(0.08)
                session.open_deadline = deadline
                return session

        launched = []

        with self.assertRaisesRegex(installed.InstalledExecutionError, "startup timed out"):
            installed.execute_installed(
                {}, {}, build_session_input=lambda *_args, **_kwargs: (path, value),
                launch_adapter=lambda *_args, **_kwargs: launched.append(True) or self._child(path),
                admit=lambda *_args, **_kwargs: None, session_factory=SlowFactory,
                cell_timeout_ms=30,
            )
        self.assertEqual([], launched)
        self.assertIn("close", events)

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

    def test_runner_builds_immutable_controller_admission_view_from_records(self):
        proof = {
            "session_id": SESSION, "sequence": 7,
            "config": {"scenario_sha256": "a" * 64, "seed_sha256": "b" * 64,
                       "store_id": "store-1", "store_schema_version": 59},
            "discovery": {"hub_id": "hub-1"},
            "service": {"generation": "boot:start"}, "tls": {},
        }
        record = {
            "status": "admitted", "operation": "verify",
            "processed_binding": {"sequence": 7}, "proof": proof,
            "started_monotonic_ns": 10, "finished_monotonic_ns": 20,
            "observed_at_ms": 1000, "result_sha256": "c" * 64,
            "invitation": {"pairing_id": "pair-1", "expires_at_ms": 2000},
            "expired_invitation": {"pairing_id": "pair-0", "expires_at_ms": 900},
        }

        class SnapshotSession:
            class Journal:
                path = "/private/controller.journal.jsonl"

                @staticmethod
                def digest():
                    return "d" * 64

            journal = Journal()

            def observations_snapshot(self, _deadline):
                return (proof,)

            def operation_records_snapshot(self, _deadline):
                return (record,)

        view = installed.build_controller_admission_view(
            SnapshotSession(), {"session_id": SESSION}, deadline=Deadline(1)
        )
        self.assertEqual(7, view["observations"][7]["sequence"])
        self.assertEqual("c" * 64, view["observations"][7]["result_sha256"])
        with self.assertRaises(TypeError):
            view["observations"][7]["operation"] = "pair"

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
            def open(self, registered, deadline=None):
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
                mock.patch("hub.tools.interop.installed_hosts.transport._verify_provider_tool", side_effect=lambda registration, deadline=None: order.append("verify-provider") or identity):
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
