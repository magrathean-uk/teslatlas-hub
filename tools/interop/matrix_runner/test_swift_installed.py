# SPDX-License-Identifier: AGPL-3.0-only
"""Source-bound prerequisites for the Swift installed adapter."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import tarfile
import tempfile
import threading
import unittest
from types import MappingProxyType, SimpleNamespace
from unittest import mock

from . import swift_installed
from .adapter_wire import load_reviewed_contract


class SwiftInstalledTests(unittest.TestCase):
    PROFILE_MEMBERS = (
        "auth.schema.json", "cases.json", "discovery.schema.json",
        "errors.schema.json", "examples/claim.json", "examples/current.json",
        "examples/discovery.json", "examples/drives.json", "examples/health.json",
        "examples/invitation.json", "examples/ready.json", "examples/vehicles.json",
        "field-semantics.json", "openapi.json", "profile.json",
        "resources.schema.json", "sync-regression.json",
    )

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)

    def tearDown(self):
        self.temporary.cleanup()

    def private_bytes(self, path, raw):
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        path.write_bytes(raw)
        path.chmod(0o600)
        return {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest()}

    def private_json(self, path, value):
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode() + b"\n"
        return self.private_bytes(path, raw)

    def callback_session(self):
        coordination = self.root / "callback-coordination"
        coordination.mkdir(mode=0o700)
        outputs = self.root / "callback-outputs"
        outputs.mkdir(mode=0o700)
        session_id = "11111111-1111-4111-8111-111111111111"
        session = {
            "schema_version": 1,
            "kind": "matrix-adapter-session",
            "session_id": session_id,
            "cell_id": "swift__macos_arm64",
            "instance_nonce": "a" * 64,
            "outputs": {
                "normalized": str(outputs / "normalized.json"),
                "actor_evidence": str(outputs / "actor-evidence.json"),
                "coordination_dir": str(coordination),
                "framework_log": str(outputs / "framework.log"),
            },
            "actors": [
                {"id": "swift_macos"}, {"id": "swift_linux"},
            ],
            "host_session": {"registration_sha256": "b" * 64},
        }
        path = self.private_json(self.root / "session-input.json", session)
        return path, session

    def test_launch_adapter_uses_the_reviewed_coordinator_and_fixed_workers(self):
        """Catches a launcher that delegates Swift authority to job argv."""
        session_path, session_input = self.callback_session()
        process = object()
        deadline = mock.Mock()
        with mock.patch.object(swift_installed, "start_coordinator_process", return_value=process) as start, \
                mock.patch.object(swift_installed, "run_coordinator", return_value=0) as coordinator:
            result = swift_installed.launch_adapter(
                {"timeout_seconds": 10},
                {"id": session_input["cell_id"], "client_id": "swift"}, {}, object(),
                session_path, session_input, deadline=deadline,
                session="root-session", running={"proof": {"sequence": 1}},
            )
            self.assertIs(process, result)
            start.call_args.args[0]()
        self.assertEqual(1, start.call_count)
        call = coordinator.call_args.args
        self.assertEqual(session_path, call[0])
        self.assertIs(session_input, call[1])
        self.assertEqual("root-session", call[2])
        self.assertIs(deadline, call[3])
        self.assertEqual({"swift_macos", "swift_linux"}, set(call[4]))
        self.assertTrue(all(callable(worker) for worker in call[4].values()))

    def test_runtime_inventory_requires_observed_worker_runtime_not_job_metadata(self):
        """Catches runtime_actual copied from the untrusted job runtime."""
        _session_path, session_input = self.callback_session()
        runtime = {
            "schema_version": 1,
            "runtime_ref": "swift_macos",
            "runtime_kind": "native-process",
            "identity_sha256": "c" * 64,
            "observed": "worker-output",
        }
        for actor_id in ("swift_macos", "swift_linux"):
            value = dict(runtime, runtime_ref=("swift_macos" if actor_id == "swift_macos" else "swift_container"))
            self.private_json(
                Path(session_input["outputs"]["coordination_dir"]) / ("runtime-" + actor_id + ".json"),
                value,
            )
        job = {"runtime": {"forged": "job metadata"}}
        inventory = swift_installed.runtime_inventory(
            job, {"client_id": "swift"}, {}, object(), session_input,
        )
        self.assertNotIn("forged", inventory["runtime_actual"])
        self.assertEqual({"swift_macos", "swift_linux"}, set(inventory["actors"]))

        (Path(session_input["outputs"]["coordination_dir"]) / "runtime-swift_linux.json").unlink()
        with self.assertRaises(swift_installed.SwiftInstalledPending):
            swift_installed.runtime_inventory(
                job, {"client_id": "swift"}, {}, object(), session_input,
            )

    def test_admit_rejects_foreign_worker_evidence_before_semantic_admission(self):
        """Catches actor evidence that bypasses the reviewed Swift validator."""
        _session_path, session_input = self.callback_session()
        actors = {
            "schema_version": 1,
            "session_id": session_input["session_id"],
            "cell_id": session_input["cell_id"],
            "session_input_sha256": "d" * 64,
            "actors": [{"id": "foreign-worker"}],
            "invocations": [],
        }
        with self.assertRaisesRegex(swift_installed.SwiftInstalledPending, "actor evidence"):
            swift_installed.admit(
                {}, {"id": session_input["cell_id"]}, {}, {}, object(), {}, actors,
                admission_views={"controller_observations": {}},
                runtime_context={
                    "session_input": session_input,
                    "runtime_view": {"actors": {
                        "swift_macos": {}, "swift_linux": {},
                    }},
                },
            )

    def test_supplement_binds_closed_runner_evidence_and_both_swift_actors(self):
        """Catches a supplement that omits one actor or independent cleanup."""
        _session_path, session_input = self.callback_session()
        normalized = self.private_json(
            Path(session_input["outputs"]["normalized"]), {"cases": []},
        )
        manifest = self.private_json(self.root / "actor-manifest.json", {
            "schema_version": 1, "build_record": {}, "files": [
                {"path": "Tests.xctest", "bytes": 1, "mode": 448, "sha256": "e" * 64}
            ],
        })
        actors = {
            "schema_version": 1,
            "session_id": session_input["session_id"],
            "cell_id": session_input["cell_id"],
            "session_input_sha256": "d" * 64,
            "actors": [
                {
                    "id": "swift_macos", "kind": "current_swift_transport",
                    "runtime_ref": "swift_macos", "entrypoint_ref": "swift_current_native_test",
                    "artifact_roles": ["swift_sdk_product"], "source_roles": ["swift_sdk_source"],
                    "installed_manifest": manifest, "raw_evidence": [],
                },
                {
                    "id": "swift_linux", "kind": "current_swift_transport",
                    "runtime_ref": "swift_container", "entrypoint_ref": "swift_current_linux_test",
                    "artifact_roles": ["swift_sdk_product"], "source_roles": ["swift_sdk_source"],
                    "installed_manifest": manifest, "raw_evidence": [],
                },
            ],
            "invocations": [],
        }
        actor_evidence = self.private_json(
            Path(session_input["outputs"]["actor_evidence"]), actors,
        )
        completion = {
            "schema_version": 1, "session_id": session_input["session_id"],
            "cell_id": session_input["cell_id"], "session_input_sha256": "d" * 64,
            "normalized": normalized, "actor_evidence": actor_evidence,
        }
        completion_binding = self.private_json(
            Path(session_input["outputs"]["coordination_dir"]) / "adapter-completion.json",
            completion,
        )
        for actor_id, runtime_ref in (("swift_macos", "swift_macos"), ("swift_linux", "swift_container")):
            self.private_json(
                Path(session_input["outputs"]["coordination_dir"]) / ("runtime-" + actor_id + ".json"),
                {"schema_version": 1, "runtime_ref": runtime_ref,
                 "runtime_kind": "native-process", "identity_sha256": "f" * 64},
            )
        runtime_view = swift_installed.runtime_inventory(
            {}, {"client_id": "swift"}, {}, object(), session_input,
        )
        self.private_json(
            Path(session_input["outputs"]["coordination_dir"]) / "ready-000001.json",
            {"kind": "retained-ready"},
        )
        self.private_json(
            Path(session_input["outputs"]["coordination_dir"]) / "ack-000001.json",
            {"kind": "retained-ack"},
        )
        journal = self.private_bytes(self.root / "journal.json", b"journal\n")
        result = SimpleNamespace(
            exit_code=0, completion=completion,
            session_evidence=SimpleNamespace(
                state="closed", cleanup_errors=(),
                final_stopped={"status": "stopped", "service": {"state": "stopped"}},
                journal_path=journal["path"], journal_sha256=journal["sha256"],
                local_transport=[{"kind": "unix", "closed": True}],
            ),
        )
        runtime_context = {
            "session_input": session_input,
            "runtime_view": runtime_view,
            "controller_view": {"observations": {1: {"session_id": session_input["session_id"]}}},
        }
        supplement = swift_installed.build_supplement(
            {"installed_session": {"config": {"sha256": "1" * 64}}},
            {"id": session_input["cell_id"]}, {}, {}, object(), result,
            runtime_context=runtime_context,
        )
        self.assertEqual(Path(session_input["outputs"]["coordination_dir"]) / "installed-supplement.json", Path(supplement["path"]))
        value = json.loads(Path(supplement["path"]).read_text())
        self.assertEqual({"swift_macos", "swift_linux"}, {item["id"] for item in value["actors"]})
        self.assertEqual(completion_binding, value["completion"]["adapter_completion"])

    def test_execution_logs_bind_the_retained_coordinator_stream(self):
        """Catches execution records that report a command without its streams."""
        _session_path, session_input = self.callback_session()
        framework = self.private_bytes(Path(session_input["outputs"]["framework_log"]), b"coordinator stdout\n")
        stderr = self.private_bytes(Path(session_input["outputs"]["framework_log"] + ".stderr.log"), b"coordinator stderr\n")
        normalized = self.private_json(Path(session_input["outputs"]["normalized"]), {"cases": []})
        result = SimpleNamespace(completion={"normalized": normalized}, exit_code=0)
        logs = swift_installed.execution_logs({}, {}, {}, object(), result)
        self.assertEqual(framework["sha256"], logs["stdout"]["sha256"])
        self.assertEqual(stderr["sha256"], logs["stderr"]["sha256"])
        self.assertEqual(0, json.loads(Path(logs["command_record"]["path"]).read_text())["exit_code"])

    def test_build_session_input_passes_the_reviewed_swift_wire(self):
        profile = self.root / "profile" / "hub-http-v1" / "1.0.0"
        checksums = []
        for name in self.PROFILE_MEMBERS:
            raw = (json.dumps({"member": name}, sort_keys=True) + "\n").encode()
            self.private_bytes(profile / name, raw)
            checksums.append(hashlib.sha256(raw).hexdigest() + "  " + name)
        profile_binding = self.private_bytes(
            profile / "SHA256SUMS", ("\n".join(checksums) + "\n").encode(),
        )
        scenario = self.private_json(self.root / "scenario.json", {"schema_version": 1})
        session_config = self.private_json(
            self.root / "installed-session.json",
            {"run_id": "swift-run", "scenario": scenario},
        )
        certificate = self.private_bytes(
            self.root / "certificate.pem",
            b"-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n",
        )

        product_source = self.root / "product-source"
        self.private_bytes(
            product_source / "Package.swift",
            b'let package = Package(name: "teslatlas-sdk-swift")\n',
        )
        self.private_bytes(product_source / "VERSION", b"2026.36.2\n")
        archive = self.root / "swift-product.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            output.add(product_source, arcname="teslatlas-sdk-swift")
        archive.chmod(0o600)
        archive_digest = hashlib.sha256(archive.read_bytes()).hexdigest()

        installed_root = self.root / "installed-swift"
        installed_member = self.private_bytes(
            installed_root / "Package.swift",
            (product_source / "Package.swift").read_bytes(),
        )
        product_inventory = self.private_json(
            self.root / "product-inventory.json",
            {
                "schema_version": 1,
                "artifact_sha256": archive_digest,
                "package_name": "teslatlas-sdk-swift",
                "version": "2026.36.2",
                "files": [{
                    "path": "Package.swift", "bytes": 51, "mode": 384,
                    "sha256": installed_member["sha256"],
                }],
            },
        )
        build_record = self.private_json(self.root / "build-record.json", {"exit_code": 0})
        actor_manifests = {}
        for actor_id in ("swift_macos", "swift_linux"):
            actor_manifests[actor_id] = self.private_json(
                self.root / (actor_id + "-manifest.json"),
                {
                    "schema_version": 1,
                    "build_record": build_record,
                    "files": [{
                        "path": "TeslatlasCurrentHubTests.xctest", "bytes": 1,
                        "mode": 448, "sha256": "b" * 64,
                    }],
                },
            )
        environment = self.private_json(
            self.root / "environment.json",
            {
                "TESLATLAS_SWIFT_PRODUCT_ROOT": str(installed_root),
                "TESLATLAS_SWIFT_PRODUCT_MANIFEST": product_inventory["path"],
                "TESLATLAS_SWIFT_MACOS_INPUT_MANIFEST": actor_manifests["swift_macos"]["path"],
                "TESLATLAS_SWIFT_LINUX_INPUT_MANIFEST": actor_manifests["swift_linux"]["path"],
            },
        )
        session_id = "11111111-1111-4111-8111-111111111111"
        descriptor = {
            "schema_version": 1, "kind": "installed-host",
            "broker_socket": str(self.root / "broker.sock"),
            "session_id": session_id, "registration_sha256": "c" * 64,
        }
        artifact = {
            "role": "swift_sdk_product", "name": "swift",
            "path": str(archive), "embedded_version": "2026.36.2",
            "sha256": archive_digest,
        }
        job = {
            "adapter": "swift", "cell_id": "swift__debian13_amd64",
            "timeout_seconds": 900, "environment_file": environment["path"],
            "installed_session": {"config": session_config},
            "source_identities": [{"role": "swift_sdk_source", "sha256": "d" * 64}],
            "artifacts": [artifact], "runtime": {"hub": {}, "client": {}},
        }
        cell = {
            "id": "swift__debian13_amd64", "client_id": "swift",
            "hub_target": "debian13_amd64",
        }
        config = {
            "product_version": "2026.36.2",
            "profile": {
                "id": "hub-http-v1", "revision": "1.0.0",
                "path": profile_binding["path"], "sha256": profile_binding["sha256"],
            },
        }
        contract = load_reviewed_contract(
            "swift", {"swift": swift_installed.CONTRACT}, private=False,
        )

        session_path, session = swift_installed.build_session_input(
            job, cell, config, {}, contract, descriptor,
            {"descriptor": {"certificate_path": certificate["path"]}},
        )

        spec = importlib.util.spec_from_file_location(
            "_swift_matrix_wire_test", swift_installed.SDK_ROOT / "tools/matrix_wire.py",
        )
        wire = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(wire)
        loaded, _digest = wire.load_session_input(session_path)
        self.assertEqual(session, loaded)
        self.assertEqual({"kind": "unix", "socket_path": descriptor["broker_socket"]}, loaded["broker"])
        self.assertEqual(["swift_macos", "swift_linux"], [actor["id"] for actor in loaded["actors"]])
        self.assertEqual(18, len(loaded["inputs"]["profile_members"]))
        self.assertEqual(
            ["profile_member_{:02d}".format(index) for index in range(1, 19)],
            [member["id"] for member in loaded["inputs"]["profile_members"]],
        )
        self.assertEqual("swift_sdk_product", loaded["inputs"]["product_inputs"][0]["artifact_role"])

    def test_current_reviewed_contract_and_topology_are_fixed_in_hub_source(self):
        contract = swift_installed.CONTRACT
        self.assertEqual("8057eae6af5dfeb55e14544c2a8fa10cdf132f40638fa6b2072c78004dcef823", contract.manifest_sha256)
        self.assertEqual("1019fbfc64c91038dbc68025bf31fb9483abb0cba13588440ab0861892b083df", contract.validator_sha256)
        self.assertEqual(
            (("macos_arm64", "local"), ("debian13_amd64", "docker_exec_pipe"),
             ("debian13_arm64", "docker_exec_pipe")),
            swift_installed.execution_by_target(),
        )
        self.assertEqual(
            (("macos_arm64", "unix"), ("debian13_amd64", "unix"),
             ("debian13_arm64", "unix")),
            swift_installed.broker_kind_by_target(),
        )
        self.assertEqual(
            ("matrix_contract", "matrix_wire"),
            tuple(item.name for item in swift_installed.REVIEWED_ADAPTER.imports),
        )
        self.assertEqual(
            "a7242473cfb091db8cf812c56314e28ebaf559859b1d1e4981f81534ff92ff05",
            swift_installed.REVIEWED_ADAPTER.sha256,
        )
        self.assertEqual(
            (
                "1019fbfc64c91038dbc68025bf31fb9483abb0cba13588440ab0861892b083df",
                "fa9cf4abfd072c1e167568aff6f1efb35b0078a228cadf94bf7f834da5d05422",
            ),
            tuple(item.sha256 for item in swift_installed.REVIEWED_ADAPTER.imports),
        )

    def test_controller_observations_come_from_the_runner_snapshot(self):
        session = object()
        session_input = {"session_id": "11111111-1111-4111-8111-111111111111"}
        view = MappingProxyType({"observations": MappingProxyType({
            7: MappingProxyType({"session_id": session_input["session_id"]}),
        })})
        with mock.patch.object(swift_installed.installed, "build_controller_admission_view", return_value=view) as build:
            observations = swift_installed.controller_observations(session, session_input, object())
        self.assertEqual({7: {"session_id": session_input["session_id"]}}, observations)
        self.assertIs(session, build.call_args.args[0])

    def test_coordinator_receives_only_fixed_capabilities(self):
        session_input = {"session_id": "11111111-1111-4111-8111-111111111111"}
        deadline = mock.Mock(remaining=lambda: 1)
        workers = {"swift_macos": lambda *_args: {}, "swift_linux": lambda *_args: {}}
        with mock.patch.object(swift_installed, "controller_observations", return_value={7: {"session_id": session_input["session_id"]}}), \
                mock.patch.object(swift_installed.swift_entrypoint, "run", return_value=0) as run:
            self.assertEqual(0, swift_installed.run_coordinator("/private/session.json", session_input, object(), deadline, workers))
            capability = run.call_args.args[1]
            self.assertEqual(1000, capability.remaining_cell_ms("swift_macos"))
            self.assertEqual({7: {"session_id": session_input["session_id"]}}, capability.controller_observations(session_input["session_id"]))

    def test_product_inventory_is_bound_to_the_selected_swift_artifact(self):
        artifact = {"role": "swift_sdk_product", "sha256": "a" * 64, "embedded_version": "2026.36.2"}
        inventory = {"schema_version": 1, "artifact_sha256": "a" * 64,
                     "package_name": "teslatlas-sdk-swift", "version": "2026.36.2",
                     "files": [{"path": "Package.swift", "bytes": 1, "mode": 420, "sha256": "b" * 64}]}
        self.assertEqual(inventory, swift_installed.validate_product_inventory(artifact, inventory))
        foreign = dict(inventory, artifact_sha256="c" * 64)
        with self.assertRaises(swift_installed.SwiftInstalledPending):
            swift_installed.validate_product_inventory(artifact, foreign)

    def test_installed_product_inventory_rehashes_actual_members(self):
        root = self.root / "installed-product"
        member = self.private_bytes(root / "Package.swift", b"reviewed Swift package\n")
        artifact = {
            "role": "swift_sdk_product", "sha256": "a" * 64,
            "embedded_version": "2026.36.2",
        }
        inventory = {
            "schema_version": 1, "artifact_sha256": "a" * 64,
            "package_name": "teslatlas-sdk-swift", "version": "2026.36.2",
            "files": [{
                "path": "Package.swift", "bytes": 23, "mode": 384,
                "sha256": member["sha256"],
            }],
        }
        self.assertEqual(
            inventory,
            swift_installed.validate_installed_product(root, artifact, inventory),
        )
        (root / "Package.swift").write_bytes(b"substitute Swift package\n")
        with self.assertRaises(swift_installed.SwiftInstalledPending):
            swift_installed.validate_installed_product(root, artifact, inventory)

    def test_coordinator_process_tracks_the_root_thread_lifetime(self):
        release = threading.Event()
        process = swift_installed.start_coordinator_process(
            lambda: release.wait(timeout=2) and 0,
        )
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        self.assertIsNone(process.poll())
        release.set()
        self.assertEqual(0, process.wait(timeout=2))

    def test_worker_process_bridges_retained_ready_to_exclusive_ack(self):
        coordination = self.root / "coordination"
        private = self.root / "private"
        coordination.mkdir(mode=0o700)
        private.mkdir(mode=0o700)
        phase = self.private_json(
            self.root / "phases.json",
            {
                "schema_version": 1, "kind": "swift-worker-phases",
                "actor_id": "swift_macos", "resources": [],
                "phases": [{
                    "phase_id": "bootstrap_pair", "ordinal": 1,
                    "recipe_id": "swift-bootstrap_pair",
                    "operations": ["verify", "pair"], "timeout_ms": 210000,
                }],
            },
        )
        config = self.private_json(
            self.root / "worker-config.json",
            {
                "schema_version": 1, "kind": "matrix-actor-worker",
                "actor_id": "swift_macos",
                "session_id": "11111111-1111-4111-8111-111111111111",
                "cell_id": "swift__macos_arm64", "instance_nonce": "a" * 64,
                "session_input_sha256": "b" * 64, "remaining_cell_ms": 2000,
                "phase_contract": {"id": "phases", "root": phase, "local": phase},
                "inputs": [], "private_root": str(private),
                "coordination_dir": str(coordination),
                "evidence_path": str(self.root / "worker-evidence.json"),
                "log_path": str(self.root / "worker.log"),
            },
        )
        script = self.root / "worker.py"
        script.write_text(
            "import hashlib,json,os,pathlib,sys,time\n"
            "p=pathlib.Path(sys.argv[1]); c=json.loads(p.read_text()); d=pathlib.Path(c['coordination_dir'])\n"
            "ready={'schema_version':1,'type':'worker_ready','session_id':c['session_id'],'cell_id':c['cell_id'],'session_input_sha256':c['session_input_sha256'],'instance_nonce':c['instance_nonce'],'sequence':1,'actor_id':c['actor_id'],'phase':'bootstrap_pair','observation':{'session_sequence':7,'proof_sha256':'c'*64},'evidence':{'path':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}}\n"
            "raw=(json.dumps(ready,sort_keys=True,separators=(',',':'))+'\\n').encode(); r=d/'worker-ready-000001.json'; r.write_bytes(raw); r.chmod(0o600)\n"
            "ack=d/'worker-ack-000001.json'; end=time.monotonic()+1.5\n"
            "while not ack.exists() and time.monotonic()<end: time.sleep(.005)\n"
            "value=json.loads(ack.read_text()); assert value['ready_sha256']==hashlib.sha256(raw).hexdigest()\n"
            "e=pathlib.Path(c['evidence_path']); e.write_text('{}\\n'); e.chmod(0o600); print('worker completed')\n",
            encoding="utf-8",
        )
        callback_ready = []

        def accept(ready_binding):
            callback_ready.append(ready_binding)
            return {
                "schema_version": 1, "type": "worker_ack",
                "session_id": "11111111-1111-4111-8111-111111111111",
                "cell_id": "swift__macos_arm64", "session_input_sha256": "b" * 64,
                "instance_nonce": "a" * 64, "sequence": 1,
                "ready_sha256": ready_binding["sha256"], "actor_id": "swift_macos",
                "phase": "bootstrap_pair", "status": "accepted", "action": "continue",
                "result": config,
            }

        outcome = swift_installed.run_worker_process(
            (sys.executable, str(script), config["path"]), self.root, {},
            config, accept, {"runtime_ref": "swift_macos"}, timeout_seconds=2,
        )

        self.assertEqual(0, outcome["framework_exit_code"])
        self.assertEqual("passed", outcome["status"])
        self.assertEqual({"runtime_ref": "swift_macos"}, outcome["runtime"])
        ready_path = coordination / "worker-ready-000001.json"
        self.assertEqual(
            hashlib.sha256(ready_path.read_bytes()).hexdigest(),
            callback_ready[0]["sha256"],
        )
        ack_path = coordination / "worker-ack-000001.json"
        self.assertEqual(0o600, stat.S_IMODE(ack_path.stat().st_mode))
        self.assertIn(b"worker completed", Path(outcome["log"]["path"]).read_bytes())


if __name__ == "__main__":
    unittest.main()
