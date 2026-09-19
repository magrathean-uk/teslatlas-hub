# SPDX-License-Identifier: AGPL-3.0-only
"""Reviewed shared defects at real runner/file/process/import boundaries.

These tests never invoke Git, an installed host, Docker, SSH or a browser.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest import mock

from . import adapter_wire, installed, swift_entrypoint
from .test_installed import FakeEvidence, FakeSession
from .test_installed import InstalledCompletionTests, FakeFactory


ROOT = Path(__file__).resolve().parents[4]
INTEROP = Path(__file__).resolve().parents[1]
SESSION = "11111111-1111-4111-8111-111111111111"


class SharedCorrectionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)

    def tearDown(self):
        self.temporary.cleanup()

    def write(self, name, value):
        target = self.root / name
        raw = value if isinstance(value, bytes) else json.dumps(value).encode() + b"\n"
        target.write_bytes(raw)
        target.chmod(0o600)
        return {"path": str(target), "sha256": hashlib.sha256(raw).hexdigest()}

    def test_i1_real_run_package_composes_v2_dispatch_to_pending(self):
        # The OS/source observer boundary is replaced; dispatch/import are real.
        script = """
import runpy
from unittest import mock
runner=runpy.run_path('hub/tools/interop/run.py')
run=runner['_run_job']
g=run.__globals__
cell={'id':'typescript_node__macos_arm64','client_id':'typescript_node','hub_target':'macos_arm64','adapter':'typescript_node'}
job={'adapter':'typescript_node','cell_id':cell['id'],'evidence_path':'/private/not-created','runtime':{}}
with mock.patch.dict(g,{'_validate_job':lambda *a:None,'_observe_job_identities':lambda *a,**k:([],[]),'_validate_cohort_binding':lambda *a,**k:{}}):
 result=run(job,cell,{'schema_version':2,'execution_kind':'actual_hub_acceptance','product_version':'2026.36.2','cohort_inputs':{} ,'jobs':[job]}, {'clients':{'typescript_node':{'required_cases':['installed_service_runtime']}}})
assert result['status']=='pending',result
assert any('reviewed installed' in error for error in result['errors']),result
print('REAL_RUN_V2_PENDING')
"""
        outcome = subprocess.run([sys.executable, "-B", "-c", script], cwd=ROOT,
                                 capture_output=True, timeout=5)
        self.assertEqual(0, outcome.returncode, outcome.stderr.decode())
        self.assertIn(b"REAL_RUN_V2_PENDING", outcome.stdout)

    def test_i17_rejected_supplement_is_a_redacted_failed_row(self):
        import runpy
        if str(INTEROP) not in sys.path:
            sys.path.insert(0, str(INTEROP))
        runner = runpy.run_path(str(ROOT / "hub" / "tools" / "interop" / "run.py"))
        run_job = runner["_run_job"]
        registry = __import__("matrix_runner.installed_registry", fromlist=["dispatch"])
        validation = __import__("matrix_runner.receipt_validation", fromlist=["_supplement"])
        evidence = self.write("row-evidence.json", {"runtime": {}})
        dispatch = SimpleNamespace(
            result=SimpleNamespace(exit_code=0), supplement=evidence,
            logs={"stdout": {}, "stderr": {}, "command_record": {}, "duration_ms": 0},
        )
        job = {
            "cell_id": "typescript_node__macos_arm64", "adapter": "typescript_node",
            "evidence_path": evidence["path"], "max_output_bytes": 8_388_608,
            "runtime": {},
        }
        cell = {"id": job["cell_id"], "client_id": "typescript_node",
                "hub_target": "macos_arm64", "adapter": "typescript_node"}
        config = {"schema_version": 2, "execution_kind": "actual_hub_acceptance",
                  "product_version": "2026.36.2", "cohort_inputs": {}}
        matrix = {"clients": {"typescript_node": {"required_cases": ["case"]}}}
        case = {"id": "case", "status": "passed", "command": [], "exit_code": 0,
                "expected": {"required": True}, "actual": {"ok": True},
                "evidence_kind": "identity", "evidence_path": evidence["path"],
                "evidence_sha256": evidence["sha256"], "request_transcript": []}
        with mock.patch.dict(run_job.__globals__, {"_validate_job": lambda *args: None,
                                      "_observe_job_identities": lambda *args, **kwargs: ([], []),
                                      "_normalize_evidence": lambda *args: ([case], [])}), \
                mock.patch.object(registry, "dispatch", return_value=dispatch), \
                mock.patch.object(validation, "_supplement",
                                  side_effect=validation.ReceiptValidationError("private host detail")):
            result = run_job(job, cell, config, matrix)
        self.assertEqual("failed", result["status"])
        self.assertEqual(["installed supplement rejected"], result["errors"])
        self.assertEqual(evidence, result["supplement"])

    def test_i8_fifo_binding_rejects_before_open_can_block(self):
        fifo = self.root / "ready-fifo.json"
        os.mkfifo(fifo, 0o600)
        code = "from matrix_runner.adapter_wire import file_binding,WireError\nimport sys\ntry:file_binding(sys.argv[1])\nexcept WireError:sys.exit(0)\nsys.exit(7)"
        environment = dict(os.environ)
        environment["PYTHONPATH"] = os.pathsep.join(
            item for item in (str(INTEROP), environment.get("PYTHONPATH", "")) if item
        )
        try:
            result = subprocess.run([sys.executable, "-B", "-c", code, str(fifo)],
                                    capture_output=True, timeout=0.6, env=environment)
        except subprocess.TimeoutExpired:
            self.fail("FIFO binding blocked before type admission")
        self.assertEqual(0, result.returncode, result.stderr.decode())

    def test_i8_oversize_binding_rejects_before_loading_bytes(self):
        target = self.root / "oversize.json"
        with target.open("wb") as stream:
            stream.truncate(8_388_609)
        target.chmod(0o600)
        with self.assertRaises(adapter_wire.WireError):
            adapter_wire.file_binding(target)

    def test_i8_foreign_uid_is_not_an_admitted_private_file(self):
        bound = self.write("owner.json", {})
        with mock.patch.object(os, "getuid", return_value=os.getuid() + 1):
            with self.assertRaises(adapter_wire.WireError):
                adapter_wire.read_bound_file(bound, label="owner", maximum=1024)

    def test_i9_missing_input_still_closes_session_and_entire_child_generation(self):
        process = subprocess.Popen([sys.executable, "-B", "-c", "import time; time.sleep(10)"],
                                   start_new_session=True)
        events = []
        try:
            with self.assertRaises(Exception):
                installed.supervise_completion(FakeSession(events), process,
                    self.root / "missing.json", {}, initial_observation={}, admit=lambda *_: None)
            self.assertIn("close", events, "admission failed before cleanup ownership began")
            self.assertIsNotNone(process.poll(), "launched process leaked on initial input failure")
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=2)

    def test_i9_exited_leader_does_not_hide_term_resistant_descendant(self):
        marker = self.root / "descendant.pid"
        code = """
import os,pathlib,signal,sys,time
child=os.fork()
if child:
 pathlib.Path(sys.argv[1]).write_text(str(child));sys.exit(0)
signal.signal(signal.SIGTERM,signal.SIG_IGN)
time.sleep(10)
"""
        process = subprocess.Popen([sys.executable, "-B", "-c", code, str(marker)],
                                   start_new_session=True)
        try:
            process.wait(timeout=2)
            child = int(marker.read_text())
            installed._terminate(process)
            deadline = time.monotonic() + 1
            while time.monotonic() < deadline:
                try:
                    os.kill(child, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.01)
            else:
                self.fail("descendant remained after leader exit")
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=2)

    def test_i9_descendant_after_successful_leader_exit_is_still_a_failure(self):
        helper = InstalledCompletionTests()
        helper.root = self.root
        session_path, value, _ = helper._session_input()
        marker = self.root / "late-descendant.pid"
        script = self.root / "child-post-ack.py"
        script.write_text(
            "import hashlib,json,os,pathlib,signal,sys,time\n"
            "p=pathlib.Path(sys.argv[1]); s=json.loads(p.read_text()); c=pathlib.Path(s['outputs']['coordination_dir'])\n"
            "def write(path,value):\n raw=json.dumps(value,sort_keys=True,separators=(',',':')).encode()+b'\\n'; fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600); os.write(fd,raw); os.fsync(fd); os.close(fd); return {'path':str(path),'sha256':hashlib.sha256(raw).hexdigest()}\n"
            "n=write(pathlib.Path(s['outputs']['normalized']),{'schema_version':1}); a=write(pathlib.Path(s['outputs']['actor_evidence']),{'schema_version':1}); comp=write(c/'adapter-completion.json',{'schema_version':1,'session_id':s['session_id'],'cell_id':s['cell_id'],'session_input_sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'normalized':n,'actor_evidence':a}); ready={'schema_version':1,'type':'ready','session_id':s['session_id'],'cell_id':s['cell_id'],'session_input_sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'instance_nonce':s['instance_nonce'],'sequence':1,'phase':'evidence_ready','observation':{'session_sequence':8,'proof_sha256':hashlib.sha256(json.dumps({'status':'verified','session_id':s['session_id'],'sequence':8},sort_keys=True,separators=(',',':')).encode()).hexdigest()},'evidence':comp}; write(c/'ready-000001.json',ready)\n"
            "while not (c/'ack-000001.json').exists(): time.sleep(.01)\n"
            "child=os.fork()\n"
            "if child: pathlib.Path(sys.argv[2]).write_text(str(child)); sys.exit(0)\n"
            "signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(10)\n",
            encoding="utf-8",
        )
        process = subprocess.Popen(
            [sys.executable, str(script), str(session_path), str(marker)],
            start_new_session=True,
        )
        try:
            with self.assertRaises(installed.InstalledExecutionError):
                installed.supervise_completion(
                    FakeSession([]), process, session_path, value,
                    initial_observation={"session_sequence": 7, "proof_sha256": installed.proof_sha256({"status": "verified", "session_id": SESSION, "sequence": 7})},
                    admit=lambda *_: None,
                )
            child = int(marker.read_text())
            until = time.monotonic() + 1
            while time.monotonic() < until:
                try:
                    os.kill(child, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.01)
            else:
                self.fail("descendant survived a failed final-generation check")
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=2)

    def test_i10_expiry_after_close_prevents_an_accepted_ack(self):
        helper = InstalledCompletionTests()
        helper.root = self.root
        session_path, value, _ = helper._session_input()
        process = helper._child(session_path)
        with self.assertRaises(installed.InstalledExecutionError):
            installed.supervise_completion(
                FakeSession([], close_delay=0.35), process, session_path, value,
                initial_observation={"session_sequence": 7, "proof_sha256": installed.proof_sha256({"status": "verified", "session_id": SESSION, "sequence": 7})},
                admit=lambda *_: None, cell_deadline=time.monotonic() + 0.2,
            )
        ack = Path(value["outputs"]["coordination_dir"]) / "ack-000001.json"
        self.assertNotEqual("accepted", json.loads(ack.read_text())["status"])

    def test_i12_real_postponed_dataclass_validator_loads_with_registered_module(self):
        source = self.write("validator.py", b"from __future__ import annotations\nfrom dataclasses import dataclass\nADAPTER_ID='typescript_node'\nCONTRACT_REVISION=1\nDECISION_CODES=frozenset({'accepted'})\n@dataclass(frozen=True)\nclass Decision:\n status: str\n code: str\ndef admit_case(case, context):return Decision('passed','accepted')\n")
        manifest = self.write("contract.json", {"schema_version": 1,
            "adapter_id": "typescript_node", "revision": 1,
            "required_cases": ["candidate_artifact_identity"], "actors": [],
            "cases": [], "raw_schemas": [], "phases": [], "validator": source})
        reviewed = adapter_wire.ReviewedContract(manifest["path"], manifest["sha256"], source["path"], source["sha256"])
        loaded = adapter_wire.load_reviewed_contract("typescript_node", {"typescript_node": reviewed})
        self.assertEqual("passed", loaded.module.admit_case({}, None).status)

    def test_i12_reviewed_module_ignores_unbound_valid_timestamp_bytecode(self):
        # A loader reopening the path will prefer this valid but unadmitted pyc.
        from importlib._bootstrap_external import _code_to_timestamp_pyc
        source = self.write("cached_validator.py", b"ADAPTER_ID='typescript_node'\nCONTRACT_REVISION=1\nDECISION_CODES=frozenset({'accepted'})\ndef admit_case(case,context):return 'admitted-source'\n")
        target = Path(source["path"])
        cached = Path(importlib.util.cache_from_source(str(target)))
        cached.parent.mkdir()
        cached.write_bytes(_code_to_timestamp_pyc(compile("ADAPTER_ID='typescript_node'\nCONTRACT_REVISION=1\nDECISION_CODES=frozenset({'accepted'})\ndef admit_case(case,context):return 'unbound-cache'\n", str(target), "exec"), int(target.stat().st_mtime), target.stat().st_size))
        manifest = self.write("cached_contract.json", {"schema_version": 1,
            "adapter_id": "typescript_node", "revision": 1,
            "required_cases": [], "actors": [], "cases": [], "raw_schemas": [],
            "phases": [], "validator": source})
        entry = adapter_wire.ReviewedContract(manifest["path"], manifest["sha256"], source["path"], source["sha256"])
        loaded = adapter_wire.load_reviewed_contract("typescript_node", {"typescript_node": entry})
        self.assertEqual("admitted-source", loaded.module.admit_case({}, None))

    def test_i16_ready_schema_version_excludes_boolean_and_float(self):
        evidence = self.write("complete.json", {})
        for version in (True, 1.0):
            value = {"schema_version": version, "type": "ready", "session_id": SESSION,
                "cell_id": "typescript_node__macos_arm64", "session_input_sha256": "a" * 64,
                "instance_nonce": "b" * 64, "sequence": 1, "phase": "evidence_ready",
                "observation": {"session_sequence": 7, "proof_sha256": "c" * 64}, "evidence": evidence}
            with self.subTest(version=version), self.assertRaises(adapter_wire.WireError):
                adapter_wire.validate_ready(value, session_id=SESSION, cell_id=value["cell_id"],
                    session_input_sha256="a" * 64, instance_nonce="b" * 64, sequence=1,
                    allowed_phases={"evidence_ready"}, observation=value["observation"])

    def test_i13_incomplete_swift_workers_cannot_be_admitted(self):
        with self.assertRaises(swift_entrypoint.SwiftEntrypointError):
            swift_entrypoint.SwiftLauncherCapability(
                {"swift_macos": lambda *_: {}}, {7: {"session_id": SESSION}})

    def test_i13_snapshot_tracks_actual_operations_and_rejects_foreign_session(self):
        from .installed import Deadline
        observations = {7: {"session_id": SESSION, "invitations": {"active": {"pairing_id": SESSION}}}}
        capability = swift_entrypoint.SwiftLauncherCapability(
            {"swift_macos": lambda *_: {}, "swift_linux": lambda *_: {}},
            lambda: observations, session_id=SESSION, deadline=Deadline(1))
        before = capability.controller_observations(SESSION)
        observations[8] = {"session_id": SESSION, "operation": "verify"}
        self.assertIn(8, capability.controller_observations(SESSION))
        self.assertNotIn(8, before)
        with self.assertRaises(TypeError):
            before[7]["invitations"]["active"]["pairing_id"] = "foreign"
        with self.assertRaises(swift_entrypoint.SwiftEntrypointError):
            capability.controller_observations("22222222-2222-4222-8222-222222222222")
        first = capability.remaining_cell_ms("swift_macos")
        time.sleep(0.02)
        self.assertLess(capability.remaining_cell_ms("swift_linux"), first)
        with self.assertRaises(swift_entrypoint.SwiftEntrypointError):
            capability.remaining_cell_ms("foreign")

    def test_i10_startup_time_is_part_of_the_one_cell_deadline(self):
        helper = InstalledCompletionTests()
        helper.root = self.root
        session_path, value, _ = helper._session_input()
        value["host_session"] = FakeSession([]).descriptor
        value["bounds"]["cell_timeout_ms"] = 100
        session_path.write_text(json.dumps(value))
        events = []
        FakeFactory.current = FakeSession(events)

        def build(*_):
            time.sleep(0.15)
            return session_path, value

        with self.assertRaises(installed.InstalledExecutionError):
            installed.execute_installed({"lifetime_seconds": 1}, {},
                build_session_input=build, launch_adapter=lambda p, _: helper._child(p),
                admit=lambda *_: None, session_factory=FakeFactory,
                cell_timeout_ms=100)
        self.assertIn("close", events)

    def test_i10_admission_expiry_cannot_become_success(self):
        helper = InstalledCompletionTests()
        helper.root = self.root
        session_path, value, _ = helper._session_input()
        value["bounds"]["cell_timeout_ms"] = 80
        session_path.write_text(json.dumps(value))
        process = helper._child(session_path)
        events = []
        with self.assertRaises(installed.InstalledExecutionError):
            installed.supervise_completion(FakeSession(events), process, session_path, value,
                initial_observation={"session_sequence": 7, "proof_sha256": "a" * 64},
                admit=lambda *_: time.sleep(0.12))
        # Expired work must never emit an accepted close acknowledgement.
        ack = Path(value["outputs"]["coordination_dir"]) / "ack-000001.json"
        self.assertNotEqual("accepted", json.loads(ack.read_text())["status"])


if __name__ == "__main__":
    unittest.main()
