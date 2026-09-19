# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for the source-fixed Protocol installed adapter seam."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import platform
import shutil
import ssl
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

from jsonschema import Draft202012Validator

from . import protocol_installed
from .adapter_wire import load_reviewed_contract


PROFILE = protocol_installed.PROTOCOL_ROOT / "profiles" / "hub-http-v1" / "1.0.0"
SESSION_ID = "11111111-1111-4111-8111-111111111111"


def digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


class ProtocolInstalledTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)
        self.broker_root = self.root / "broker"
        self.broker_root.mkdir(mode=0o700)

    def write(self, relative: str, raw: bytes, mode: int = 0o600) -> Path:
        path = self.root / relative
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        path.write_bytes(raw)
        path.chmod(mode)
        return path

    @staticmethod
    def binding(path: Path) -> dict[str, str]:
        return {"path": str(path), "sha256": digest(path.read_bytes())}

    @staticmethod
    def use_observed_python(job):
        job["runtime"]["client"] = {
            "os": "macOS" if platform.system() == "Darwin" else platform.system(),
            "architecture": {"aarch64": "arm64", "x86_64": "amd64"}.get(
                platform.machine(), platform.machine(),
            ),
            "native_or_emulated": "native",
            "service_mode": "protocol-conformance-process",
            "tool_versions": {"python": platform.python_version()},
        }

    def completed_inputs(self):
        job, cell, config, descriptor, running, _ = self.inputs()
        self.use_observed_python(job)
        contract = load_reviewed_contract(
            "protocol_actual_hub", {"protocol_actual_hub": protocol_installed.CONTRACT},
            private=False,
        )
        session_path, session = protocol_installed.build_session_input(
            job, cell, config, {}, contract, descriptor, running,
        )
        session_sha256 = digest(session_path.read_bytes())
        runtime = protocol_installed.runtime_inventory(job, cell, config, contract, session)
        header = json.loads(Path(session["header"]["local"]["path"]).read_text())
        scenario = json.loads(Path(session["inputs"]["scenario"]["local"]["path"]).read_text())
        actor_claim = {
            "id": "protocol_http", "kind": "protocol_http", "runtime_ref": "root_python",
            "entrypoint_ref": "protocol_actual_hub", "artifact_roles": [],
            "source_roles": ["protocol_source"],
            "installed_manifest": session["actors"][0]["input_manifest"]["local"],
            "raw_evidence": [],
        }
        admitted_actor = contract.module.AdmittedActor(
            id=actor_claim["id"], kind=actor_claim["kind"],
            runtime_ref=actor_claim["runtime_ref"], entrypoint_ref=actor_claim["entrypoint_ref"],
            artifact_roles=(), source_roles=("protocol_source",),
            installed_manifest=actor_claim["installed_manifest"], runtime=runtime["runtime"],
        )
        observations = {
            1: {"sequence": 1, "hub_id": "hub-protocol-test"},
            2: {"sequence": 2, "hub_id": "hub-protocol-test"},
        }
        context = contract.module.AdmissionContext(
            adapter_id=cell["client_id"], cell_id=cell["id"], session_id=session["session_id"],
            header=header, scenario=scenario, actors={"protocol_http": admitted_actor},
            invocations=(), raw={}, controller_observations=observations,
        )
        cases = []
        invocations = []
        coordination = Path(session["outputs"]["coordination_dir"])
        for declaration in contract.manifest["cases"]:
            case_id = declaration["id"]
            operation = declaration["operations"][0]
            evidence_id = "evidence-" + case_id
            facts = contract.module.expected_normalized_facts(case_id, context)
            raw = {
                "schema_version": 1, "session_id": session["session_id"],
                "cell_id": cell["id"], "session_input_sha256": session_sha256,
                "actor_id": "protocol_http", "operation": operation,
                "actor_manifest_sha256": actor_claim["installed_manifest"]["sha256"],
                "session_sequence_before": 1, "session_sequence_after": 2,
                "credential_device_id": None, "facts": facts, "requests": [],
                "request_ids": [],
                "cleanup": {"status": "passed", "transport_resources_closed": True,
                            "auxiliary_fixture_stopped": True, "process_exited": True},
            }
            raw_path = coordination / "raw" / (evidence_id + ".json")
            raw_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            raw_path.write_text(json.dumps(raw, sort_keys=True, separators=(",", ":")) + "\n")
            raw_path.chmod(0o600)
            actor_claim["raw_evidence"].append(
                {"id": evidence_id, "schema_id": "protocol-http-v1", "binding": self.binding(raw_path)}
            )
            invocations.append({
                "id": "invoke-" + case_id, "case_id": case_id,
                "actor_id": "protocol_http", "operation": operation,
                "session_sequence_before": 1, "session_sequence_after": 2,
                "evidence_id": evidence_id, "request_ids": [],
            })
            cases.append({
                "id": case_id,
                "status": "pending" if case_id == "installed_service_runtime" else "passed",
                "expected": facts, "actual": facts,
                "evidence_kind": declaration["evidence_kind"], "request_transcript": [],
            })
        normalized = dict(header, cases=cases)
        actors = {
            "schema_version": 1, "session_id": session["session_id"],
            "cell_id": cell["id"], "session_input_sha256": session_sha256,
            "actors": [actor_claim], "invocations": invocations,
        }
        runtime_context = {
            "session_input": session, "runtime_view": runtime,
            "controller_view": {"observations": observations},
        }
        admission_views = {"controller_observations": {"observations": observations}}
        return job, cell, config, contract, session, normalized, actors, runtime_context, admission_views

    def certificate(self) -> tuple[Path, str]:
        config = self.write(
            "openssl.cnf",
            b"[req]\nprompt=no\ndistinguished_name=dn\n[v3]\n"
            b"basicConstraints=critical,CA:TRUE\n"
            b"keyUsage=critical,digitalSignature,keyCertSign\n"
            b"subjectAltName=IP:127.0.0.1\n[dn]\nCN=127.0.0.1\n",
        )
        key = self.root / "key.pem"
        certificate = self.root / "certificate.pem"
        result = subprocess.run(
            [
                "openssl", "req", "-x509", "-nodes", "-newkey", "rsa:2048",
                "-keyout", str(key), "-out", str(certificate), "-days", "1",
                "-config", str(config),
            ],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, check=False,
        )
        self.assertEqual(0, result.returncode, result.stderr.decode(errors="replace"))
        key.chmod(0o600)
        certificate.chmod(0o600)
        der = ssl.PEM_cert_to_DER_cert(certificate.read_text(encoding="ascii"))
        return certificate, digest(der)

    def inputs(self):
        scenario = self.write(
            "scenario.json",
            b'{"name":"two-vehicles-five-drives","provenance":"synthetic-only"}\n',
        )
        session_config = self.write(
            "installed-session.json",
            json.dumps(
                {"run_id": "protocol-run", "scenario": self.binding(scenario)},
                sort_keys=True, separators=(",", ":"),
            ).encode() + b"\n",
        )
        certificate, certificate_der_sha256 = self.certificate()
        hub = self.write("teslatlas-hub", b"fixed-hub-artifact", 0o500)
        seed = self.write("interop-fixture", b"fixed-seed-artifact", 0o500)
        job = {
            "adapter": "protocol_actual_hub",
            "cell_id": "protocol_actual_hub__macos_arm64",
            "timeout_seconds": 60,
            "installed_session": {"config": self.binding(session_config)},
            "source_identities": [
                {
                    "role": "hub_source", "repo": str(protocol_installed.WORKSPACE / "hub"),
                    "head": "a" * 40, "dirty_patch_sha256": "b" * 64,
                    "untracked_source_manifest_sha256": "c" * 64,
                },
                {
                    "role": "protocol_source", "repo": str(protocol_installed.PROTOCOL_ROOT),
                    "head": "d" * 40, "dirty_patch_sha256": "e" * 64,
                    "untracked_source_manifest_sha256": "f" * 64,
                },
            ],
            "artifacts": [
                {
                    "role": "hub_executable", "name": "teslatlas-hub",
                    "path": str(hub), "embedded_version": "2026.36.2",
                    "sha256": digest(hub.read_bytes()),
                },
                {
                    "role": "protocol_fixture_seed", "name": "interop_fixture",
                    "path": str(seed), "embedded_version": "tooling",
                    "sha256": digest(seed.read_bytes()),
                },
            ],
            "runtime": {
                "hub": {
                    "os": "macOS", "architecture": "arm64",
                    "native_or_emulated": "native", "service_mode": "launchd",
                    "tool_versions": {"hub": "2026.36.2"},
                },
                "client": {
                    "os": "macOS", "architecture": "arm64",
                    "native_or_emulated": "native",
                    "service_mode": "protocol-conformance-process",
                    "tool_versions": {"python": "3.11"},
                },
                "browser_engines": [], "client_transports": ["http.client"],
            },
            # Historical runner fields are intentionally hostile: they have no
            # authority over the reviewed source launcher or its output paths.
            "argv": ["/tmp/foreign-python", "/tmp/foreign-adapter"],
            "cwd": "/tmp/foreign-cwd",
            "evidence_path": "/tmp/foreign-evidence",
        }
        self.use_observed_python(job)
        cell = {
            "id": job["cell_id"], "client_id": "protocol_actual_hub",
            "hub_target": "macos_arm64",
        }
        profile_root = self.root / "profile"
        shutil.copytree(PROFILE, profile_root)
        config = {
            "product_version": "2026.36.2",
            "profile": {
                "id": "hub-http-v1", "revision": "1.0.0",
                "path": str(profile_root),
                "sha256": digest((profile_root / "SHA256SUMS").read_bytes()),
            },
        }
        descriptor = {
            "schema_version": 1, "kind": "installed-host",
            "broker_socket": str(self.broker_root / "controller.sock"),
            "session_id": SESSION_ID, "registration_sha256": "1" * 64,
        }
        running = {"descriptor": {"certificate_path": str(certificate)}}
        return job, cell, config, descriptor, running, certificate_der_sha256

    def test_reviewed_contract_sources_and_topology_are_fixed(self):
        loaded = load_reviewed_contract(
            "protocol_actual_hub",
            {"protocol_actual_hub": protocol_installed.CONTRACT},
            private=False,
        )
        self.assertEqual("protocol_actual_hub", loaded.manifest["adapter_id"])
        self.assertEqual(
            "75bdb6380f70dc30b0f91abf92edbe7024fe0617402c67025bd5ec5dca609ca8",
            digest(Path(protocol_installed.CONTRACT.manifest_path).read_bytes()),
        )
        self.assertEqual(
            {
                "path": "matrix_contract.py",
                "sha256": "eef926e5d732a4c7ad46a9fe02a6089ea28d5c36b4e2523607212603b4994b24",
            },
            dict(loaded.manifest["validator"]),
        )
        self.assertEqual(
            "97241f70538c98e0b851581c4d13f983b0b5f57bf543883abb8a6ede1ac23cd9",
            digest((protocol_installed.PROTOCOL_ROOT / "tools/protocol-http-v1.schema.json").read_bytes()),
        )
        self.assertEqual(
            "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926",
            digest((PROFILE / "SHA256SUMS").read_bytes()),
        )
        self.assertEqual(("protocol_http",), tuple(item["id"] for item in loaded.manifest["actors"]))
        self.assertEqual(
            (("macos_arm64", "local"), ("debian13_amd64", "local"), ("debian13_arm64", "local")),
            protocol_installed.execution_by_target(),
        )
        self.assertEqual(
            (("macos_arm64", "unix"), ("debian13_amd64", "unix"), ("debian13_arm64", "unix")),
            protocol_installed.broker_kind_by_target(),
        )
        self.assertEqual(
            {
                "conformance/hub_matrix.py", "conformance/hub_control.py",
                "conformance/hub_http.py", "conformance/hub_native_evidence.py",
            },
            set(protocol_installed.REVIEWED_SOURCES),
        )

    def test_build_stages_closed_session_and_exact_source_inventory(self):
        job, cell, config, descriptor, running, certificate_der_sha256 = self.inputs()
        contract = load_reviewed_contract(
            "protocol_actual_hub", {"protocol_actual_hub": protocol_installed.CONTRACT},
            private=False,
        )
        path, session = protocol_installed.build_session_input(
            job, cell, config, {}, contract, descriptor, running,
        )
        self.assertEqual(session, json.loads(path.read_text(encoding="utf-8")))
        self.assertEqual("unix", session["broker"]["kind"])
        self.assertEqual(descriptor["broker_socket"], session["broker"]["socket_path"])
        self.assertEqual([], session["inputs"]["product_inputs"])
        self.assertEqual(certificate_der_sha256, session["inputs"]["certificate_der_sha256"])
        self.assertEqual(
            [{
                "id": "protocol_http", "kind": "protocol_http",
                "execution": "coordinator", "runtime_ref": "root_python",
                "artifact_roles": [], "source_roles": ["protocol_source"],
                "entrypoint_ref": "protocol_actual_hub",
                "input_manifest": session["actors"][0]["input_manifest"],
                "phase_contract": None,
            }],
            session["actors"],
        )
        manifest = json.loads(
            Path(session["actors"][0]["input_manifest"]["local"]["path"]).read_text()
        )
        self.assertEqual(list(protocol_installed.REVIEWED_SOURCES), [item["path"] for item in manifest["files"]])
        self.assertEqual(
            list(protocol_installed.REVIEWED_SOURCES.values()),
            [item["sha256"] for item in manifest["files"]],
        )
        self.assertTrue(all(item["mode"] == 0o400 for item in manifest["files"]))
        self.assertNotIn("/tmp/foreign", json.dumps(session, sort_keys=True))

    def test_launch_uses_only_staged_reviewed_source_and_fixed_python(self):
        job, cell, config, descriptor, running, _ = self.inputs()
        contract = load_reviewed_contract(
            "protocol_actual_hub", {"protocol_actual_hub": protocol_installed.CONTRACT},
            private=False,
        )
        path, session = protocol_installed.build_session_input(
            job, cell, config, {}, contract, descriptor, running,
        )
        launched = SimpleNamespace(pid=42)
        with mock.patch.dict(
            protocol_installed.os.environ,
            {"PYTHONPATH": "/tmp/foreign-python", "PYTHONHOME": "/tmp/foreign-home"},
        ):
            with mock.patch.object(
                protocol_installed.subprocess, "Popen", return_value=launched,
            ) as popen:
                result = protocol_installed.launch_adapter(
                    job, cell, config, contract, path, session,
                )
        self.assertIs(launched, result)
        argv = popen.call_args.args[0]
        self.assertEqual(str(Path(protocol_installed.sys.executable).resolve()), argv[0])
        self.assertEqual("hub_matrix.py", Path(argv[1]).name)
        self.assertEqual(str(path), argv[2])
        self.assertNotIn("foreign", " ".join(argv))
        self.assertEqual(
            str(Path(session["actors"][0]["input_manifest"]["local"]["path"]).parent),
            popen.call_args.kwargs["cwd"],
        )
        self.assertNotIn("PYTHONPATH", popen.call_args.kwargs["env"])
        self.assertNotIn("PYTHONHOME", popen.call_args.kwargs["env"])
        self.assertEqual("1", popen.call_args.kwargs["env"]["PYTHONNOUSERSITE"])

    def test_launch_rejects_changed_staged_source_before_process_creation(self):
        job, cell, config, descriptor, running, _ = self.inputs()
        contract = load_reviewed_contract(
            "protocol_actual_hub", {"protocol_actual_hub": protocol_installed.CONTRACT},
            private=False,
        )
        path, session = protocol_installed.build_session_input(
            job, cell, config, {}, contract, descriptor, running,
        )
        source_root = Path(session["actors"][0]["input_manifest"]["local"]["path"]).parent
        changed = source_root / "conformance" / "hub_matrix.py"
        changed.chmod(0o600)
        changed.write_bytes(changed.read_bytes() + b"\n")
        changed.chmod(0o400)
        with mock.patch.object(protocol_installed.subprocess, "Popen") as popen:
            with self.assertRaises(protocol_installed.ProtocolInstalledPending):
                protocol_installed.launch_adapter(job, cell, config, contract, path, session)
        popen.assert_not_called()

    def test_runtime_inventory_is_runner_observed_and_job_mismatch_fails_closed(self):
        job, cell, config, descriptor, running, _ = self.inputs()
        self.use_observed_python(job)
        contract = load_reviewed_contract(
            "protocol_actual_hub", {"protocol_actual_hub": protocol_installed.CONTRACT},
            private=False,
        )
        _path, session = protocol_installed.build_session_input(
            job, cell, config, {}, contract, descriptor, running,
        )

        inventory = protocol_installed.runtime_inventory(
            job, cell, config, contract, session,
        )
        self.assertEqual("root_python", inventory["runtime_ref"])
        self.assertEqual("coordinator_python", inventory["runtime_kind"])
        runtime_document = json.loads(
            Path(inventory["binding"]["path"]).read_text(encoding="utf-8")
        )
        self.assertEqual(inventory["identity_sha256"], runtime_document["identity_sha256"])
        self.assertEqual(job["runtime"]["client"], inventory["runtime_actual"]["client"])

        job["runtime"]["client"]["architecture"] = "foreign"
        with self.assertRaises(protocol_installed.ProtocolInstalledPending):
            protocol_installed.runtime_inventory(job, cell, config, contract, session)

    def test_python_preflight_binds_current_interpreter_and_rejects_pre_311(self):
        observed = protocol_installed._reviewed_python_runtime()
        executable = Path(observed["executable"])
        self.assertEqual(executable, Path(protocol_installed.sys.executable).resolve())
        self.assertEqual(digest(executable.read_bytes()), observed["identity_sha256"])
        self.assertEqual(platform.python_version(), observed["tool_versions"]["python"])

        job, cell, config, descriptor, running, _ = self.inputs()
        contract = load_reviewed_contract(
            "protocol_actual_hub", {"protocol_actual_hub": protocol_installed.CONTRACT},
            private=False,
        )
        with mock.patch.object(protocol_installed.sys, "version_info", (3, 10, 14)):
            with self.assertRaisesRegex(
                protocol_installed.ProtocolInstalledPending, "Python 3.11 or newer",
            ):
                protocol_installed.build_session_input(
                    job, cell, config, {}, contract, descriptor, running,
                )

    def test_admit_uses_raw_and_controller_evidence_and_rejects_normalized_drift(self):
        (job, cell, config, contract, _session, normalized, actors,
         runtime_context, admission_views) = self.completed_inputs()
        admitted = protocol_installed.admit(
            job, cell, config, {}, contract, normalized, actors,
            runtime_context=runtime_context, admission_views=admission_views,
        )
        self.assertEqual(list(contract.manifest["required_cases"]), [
            item["id"] for item in admitted["cases"]
        ])
        self.assertEqual("pending", admitted["cases"][1]["status"])

        changed = json.loads(json.dumps(normalized))
        changed["cases"][0]["actual"]["product_version"] = "foreign"
        with self.assertRaises(protocol_installed.ProtocolInstalledPending):
            protocol_installed.admit(
                job, cell, config, {}, contract, changed, actors,
                runtime_context=runtime_context, admission_views=admission_views,
            )

    def test_admit_rejects_request_ids_detached_from_transcript_and_invocation(self):
        (job, cell, config, contract, _session, normalized, actors,
         runtime_context, admission_views) = self.completed_inputs()
        raw_claim = actors["actors"][0]["raw_evidence"][0]
        raw_path = Path(raw_claim["binding"]["path"])
        raw = json.loads(raw_path.read_text(encoding="utf-8"))
        raw["request_ids"] = ["detached-request"]
        raw_path.write_text(
            json.dumps(raw, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )
        raw_claim["binding"] = self.binding(raw_path)
        with self.assertRaises(protocol_installed.ProtocolInstalledPending):
            protocol_installed.admit(
                job, cell, config, {}, contract, normalized, actors,
                runtime_context=runtime_context, admission_views=admission_views,
            )

    def test_admit_rejects_internally_consistent_substituted_session_input_hash(self):
        (job, cell, config, contract, _session, normalized, actors,
         runtime_context, admission_views) = self.completed_inputs()
        substituted = "9" * 64
        actors["session_input_sha256"] = substituted
        for claim in actors["actors"][0]["raw_evidence"]:
            path = Path(claim["binding"]["path"])
            raw = json.loads(path.read_text(encoding="utf-8"))
            raw["session_input_sha256"] = substituted
            path.write_text(
                json.dumps(raw, sort_keys=True, separators=(",", ":")) + "\n",
                encoding="utf-8",
            )
            claim["binding"] = self.binding(path)

        with self.assertRaisesRegex(
            protocol_installed.ProtocolInstalledPending, "SessionInput",
        ):
            protocol_installed.admit(
                job, cell, config, {}, contract, normalized, actors,
                runtime_context=runtime_context, admission_views=admission_views,
            )

    def test_admit_rejects_unrelated_copy_of_staged_actor_manifest(self):
        (job, cell, config, contract, session, normalized, actors,
         runtime_context, admission_views) = self.completed_inputs()
        staged = session["actors"][0]["input_manifest"]["local"]
        unrelated = self.write("unrelated-actor-manifest.json", Path(staged["path"]).read_bytes())
        actors["actors"][0]["installed_manifest"] = self.binding(unrelated)

        with self.assertRaisesRegex(
            protocol_installed.ProtocolInstalledPending, "manifest",
        ):
            protocol_installed.admit(
                job, cell, config, {}, contract, normalized, actors,
                runtime_context=runtime_context, admission_views=admission_views,
            )

    def completed_result(self):
        (job, cell, config, contract, session, normalized, actors,
         runtime_context, _admission_views) = self.completed_inputs()

        def output(path: Path, value) -> dict[str, str]:
            path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            path.write_text(
                json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
                encoding="utf-8",
            )
            path.chmod(0o600)
            return self.binding(path)

        normalized_binding = output(Path(session["outputs"]["normalized"]), normalized)
        actor_binding = output(Path(session["outputs"]["actor_evidence"]), actors)
        completion = {
            "schema_version": 1, "session_id": session["session_id"],
            "cell_id": cell["id"],
            "session_input_sha256": digest(
                (Path(session["outputs"]["normalized"]).parent.parent / "session-input.json")
                .read_bytes()
            ),
            "normalized": normalized_binding, "actor_evidence": actor_binding,
        }
        coordination = Path(session["outputs"]["coordination_dir"])
        output(coordination / "adapter-completion.json", completion)
        output(coordination / "ready-000001.json", {"kind": "retained-ready"})
        output(coordination / "ack-000001.json", {"kind": "retained-ack"})
        journal = output(coordination / "controller.jsonl", {"kind": "closed"})
        framework = Path(session["outputs"]["framework_log"])
        framework.write_bytes(b"protocol stdout\n")
        framework.chmod(0o600)
        stderr = Path(str(framework) + ".stderr.log")
        stderr.write_bytes(b"protocol stderr\n")
        stderr.chmod(0o600)
        evidence = SimpleNamespace(
            journal_path=journal["path"], journal_sha256=journal["sha256"],
            final_stopped={"status": "stopped"}, cleanup_errors=(),
            local_transport={"kind": "unix", "status": "closed"},
        )
        result = SimpleNamespace(
            completion=completion, exit_code=0, session_evidence=evidence,
        )
        return job, cell, config, contract, session, actors, runtime_context, result

    def test_supplement_consumes_original_completion_and_request_bindings(self):
        (job, cell, config, contract, session, actors,
         runtime_context, result) = self.completed_result()
        original_raw = {
            item["id"]: Path(item["binding"]["path"]).read_bytes()
            for item in actors["actors"][0]["raw_evidence"]
        }
        binding = protocol_installed.build_supplement(
            job, cell, config, {}, contract, result,
            runtime_context=runtime_context,
        )
        supplement = json.loads(Path(binding["path"]).read_text(encoding="utf-8"))
        schema = json.loads(
            (Path(protocol_installed.__file__).with_name("supplement.schema.json"))
            .read_text(encoding="utf-8")
        )
        self.assertIsNone(next(Draft202012Validator(schema).iter_errors(supplement), None))
        self.assertEqual(result.completion["normalized"], supplement["completion"]["normalized"])
        self.assertEqual(result.completion["actor_evidence"], supplement["completion"]["actor_evidence"])
        self.assertEqual(
            actors["invocations"],
            [invocation for case in supplement["case_bindings"]
             for invocation in case["invocations"]],
        )
        self.assertEqual(
            runtime_context["runtime_view"]["binding"],
            supplement["actors"][0]["runtime_evidence"],
        )
        for item in actors["actors"][0]["raw_evidence"]:
            self.assertEqual(original_raw[item["id"]], Path(item["binding"]["path"]).read_bytes())
        self.assertEqual(
            session["host_session"]["registration_sha256"],
            supplement["controller_evidence"]["registration_sha256"],
        )

    def test_supplement_rejects_substituted_session_and_actor_manifest_bindings(self):
        (job, cell, config, contract, session, actors,
         runtime_context, result) = self.completed_result()
        substituted = "9" * 64
        result.completion["session_input_sha256"] = substituted
        actor_path = Path(result.completion["actor_evidence"]["path"])
        actor_evidence = json.loads(actor_path.read_text(encoding="utf-8"))
        actor_evidence["session_input_sha256"] = substituted
        staged = session["actors"][0]["input_manifest"]["local"]
        unrelated = self.write("supplement-unrelated-manifest.json", Path(staged["path"]).read_bytes())
        actor_evidence["actors"][0]["installed_manifest"] = self.binding(unrelated)
        actor_path.write_text(
            json.dumps(actor_evidence, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )
        result.completion["actor_evidence"] = self.binding(actor_path)
        coordination = Path(session["outputs"]["coordination_dir"])
        completion_path = coordination / "adapter-completion.json"
        completion_path.write_text(
            json.dumps(result.completion, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )

        with self.assertRaises(protocol_installed.ProtocolInstalledPending):
            protocol_installed.build_supplement(
                job, cell, config, {}, contract, result,
                runtime_context=runtime_context,
            )

    def test_execution_logs_bind_exact_launcher_streams_and_fixed_command(self):
        (job, cell, config, contract, session, _actors,
         _runtime_context, result) = self.completed_result()
        logs = protocol_installed.execution_logs(job, cell, config, contract, result)
        self.assertEqual(b"protocol stdout\n", Path(logs["stdout"]["path"]).read_bytes())
        self.assertEqual(b"protocol stderr\n", Path(logs["stderr"]["path"]).read_bytes())
        command = json.loads(Path(logs["command_record"]["path"]).read_text(encoding="utf-8"))
        self.assertEqual(str(Path(protocol_installed.sys.executable).resolve()), command["argv"][0])
        self.assertEqual("hub_matrix.py", Path(command["argv"][1]).name)
        self.assertEqual(0, command["exit_code"])
        self.assertEqual("passed", command["outcome"])
        self.assertEqual(0, logs["duration_ms"])

    def test_source_entry_is_closed_to_protocol_adapter(self):
        self.assertIs(protocol_installed.CONTRACT, protocol_installed.source_entry("protocol_actual_hub"))
        with self.assertRaises(protocol_installed.ProtocolInstalledPending):
            protocol_installed.source_entry("foreign")

    def test_job_cannot_add_source_or_launch_authority(self):
        job, cell, config, descriptor, running, _ = self.inputs()
        contract = load_reviewed_contract(
            "protocol_actual_hub", {"protocol_actual_hub": protocol_installed.CONTRACT},
            private=False,
        )
        for field in ("executable", "validator_path", "output_path", "broker_socket"):
            with self.subTest(field=field):
                hostile = dict(job)
                hostile[field] = "/tmp/foreign"
                with self.assertRaises(protocol_installed.ProtocolInstalledPending):
                    protocol_installed.build_session_input(
                        hostile, cell, config, {}, contract, descriptor, running,
                    )


if __name__ == "__main__":
    unittest.main()
