#!/usr/bin/env python3
"""Behavior tests for the fail-closed Hub compatibility matrix runner."""

import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
import tarfile
from types import SimpleNamespace

try:
    from . import run as matrix_run
except ImportError:
    import run as matrix_run


ROOT = Path(__file__).resolve().parents[2]
MATRIX_PATH = ROOT / "docs/compatibility/matrix.json"


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def private_json(path, value):
    path = Path(path)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        json.dump(value, stream, sort_keys=True)
        stream.write("\n")
    return path


class MatrixRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.profile = self.root / "profile"
        self.profile.mkdir()
        private_json(
            self.profile / "profile.json",
            {"profile_id": "hub-http-v1@1.0.0"},
        )
        profile_digest = sha256(self.profile / "profile.json")
        (self.profile / "SHA256SUMS").write_text(
            f"{profile_digest}  profile.json\n", encoding="ascii"
        )
        self.repo = self.root / "repo"
        self.repo.mkdir()
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        (self.repo / "source.txt").write_text("bound source\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(self.repo), "add", "source.txt"], check=True)
        subprocess.run(
            [
                "git",
                "-C",
                str(self.repo),
                "-c",
                "user.name=Matrix Test",
                "-c",
                "user.email=matrix@example.invalid",
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
            check=True,
        )
        self.head = subprocess.check_output(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"], text=True
        ).strip()
        self.artifact = self.root / "candidate.bin"
        self.artifact.write_bytes(b"candidate 2026.36.2\n")
        self.env_path = private_json(self.root / "environment.json", {})
        self.adapter = self.root / "adapter.py"
        self.adapter.write_text(self._adapter_source(), encoding="utf-8")
        self.adapter.chmod(0o500)
        self.matrix = json.loads(MATRIX_PATH.read_text(encoding="utf-8"))
        self.matrix_hash = sha256(MATRIX_PATH)
        self.source_identity = matrix_run.observe_source_identity(self.repo, "deterministic_source")

    def tearDown(self):
        self.temporary.cleanup()

    def _adapter_source(self):
        return """#!/usr/bin/env python3
import json, os, subprocess, sys, time
spec=json.load(open(os.environ['MATRIX_TEST_SPEC'], encoding='utf-8'))
mode=spec.get('mode','pass')
if mode=='nonzero': sys.exit(7)
if mode=='timeout':
    marker=spec['marker']
    subprocess.Popen([sys.executable,'-c',f\"import time,pathlib;time.sleep(3);pathlib.Path({marker!r}).write_text('leaked')\"])
    time.sleep(30)
if mode in ('normal_descendant','term_resistant'):
    marker=spec['marker']
    signal_code="import signal,time,pathlib;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(3);pathlib.Path(%r).write_text('leaked')"%marker
    ordinary_code="import time,pathlib;time.sleep(3);pathlib.Path(%r).write_text('leaked')"%marker
    subprocess.Popen([sys.executable,'-c',signal_code if mode=='term_resistant' else ordinary_code])
if mode=='empty_legacy':
    print('{}')
    sys.exit(0)
payload=spec['payload']
if mode=='wrong_actual': payload['cases'][0]['actual']={'value':'wrong'}
if mode=='illegal_na': payload['cases'][0].update(status='not_applicable', not_applicable_reason='made up')
if mode=='duplicate_case': payload['cases'].append(dict(payload['cases'][0]))
if mode=='meaningless': payload['cases'][0].update(expected={'value':None},actual={'value':None})
print(json.dumps(payload,sort_keys=True))
"""

    def _identity(self):
        return {
            "source_identities": [self.source_identity],
            "artifacts": [
                {
                    "role": "deterministic_fixture",
                    "name": "candidate",
                    "path": str(self.artifact),
                    "embedded_version": "2026.36.2",
                    "sha256": sha256(self.artifact),
                }
            ],
            "runtime": {
                "hub": {
                    "os": "deterministic-hub-os",
                    "architecture": "deterministic-hub-arch",
                    "native_or_emulated": "native",
                    "service_mode": "owned-test-child",
                    "tool_versions": {"hub": "deterministic"},
                },
                "client": {
                    "os": "deterministic-client-os",
                    "architecture": "deterministic-client-arch",
                    "native_or_emulated": "native",
                    "service_mode": "test-process",
                    "tool_versions": {"python": sys.version.split()[0]},
                },
                "browser_engines": [],
                "client_transports": ["deterministic"],
            },
        }

    def _case(self, case_id):
        expected = {"case": case_id, "value": "verified"}
        process_evidence = None
        if case_id.endswith("zero_requests"):
            expected = {"outgoing_requests": 0}
            transcript = []
            evidence_kind = "zero_request"
        elif case_id in ("candidate_artifact_identity", "installed_service_runtime"):
            transcript = []
            evidence_kind = "identity"
            process_evidence = {"identity": case_id, "sha256": "a" * 64}
        else:
            evidence_kind = "http"
            transcript = [
                {
                    "method": "GET",
                    "route": f"/deterministic/{case_id}",
                    "status": 200,
                    "request_id": f"req-{case_id}",
                }
            ]
        value = {
            "id": case_id,
            "status": "passed",
            "expected": expected,
            "actual": expected,
            "evidence_kind": evidence_kind,
            "request_transcript": transcript,
        }
        if process_evidence is not None:
            value["process_evidence"] = process_evidence
        return value

    def _required_cases(self, cell):
        return self.matrix["clients"][cell["client_id"]]["required_cases"]

    def _job(self, cell, *, mode="pass", timeout=5):
        evidence = self.root / f"{cell['id']}.json"
        spec_path = self.root / f"{cell['id']}-spec.json"
        identity = self._identity()
        payload = {
            "schema_version": 1,
            "execution_kind": "deterministic_runner_test",
            "adapter": cell["adapter"],
            "cell_id": cell["id"],
            "product_version": "2026.36.2",
            "profile_id": "hub-http-v1",
            "profile_revision": "1.0.0",
            "profile_sha256": sha256(self.profile / "SHA256SUMS"),
            **identity,
            "cases": [self._case(case_id) for case_id in self._required_cases(cell)],
        }
        spec = {"mode": mode, "payload": payload}
        if mode in ("timeout", "normal_descendant", "term_resistant"):
            spec["marker"] = str(self.root / "descendant-leak.txt")
        private_json(spec_path, spec)
        env = {"MATRIX_TEST_SPEC": str(spec_path)}
        env_path = private_json(self.root / f"{cell['id']}-env.json", env)
        return {
            "cell_id": cell["id"],
            "adapter": cell["adapter"],
            "argv": [sys.executable, str(self.adapter)],
            "cwd": str(self.root),
            "environment_file": str(env_path),
            "evidence_path": str(evidence),
            "evidence_mode": "stdout_json",
            "timeout_seconds": timeout,
            "max_output_bytes": 262144,
            "command_files": [
                {"path": sys.executable, "sha256": sha256(sys.executable)},
                {"path": str(self.adapter), "sha256": sha256(self.adapter)},
            ],
            **identity,
        }

    def _config(self, jobs=None):
        if jobs is None:
            jobs = [self._job(cell) for cell in self.matrix["cells"]]
        config = {
            "schema_version": 1,
            "execution_kind": "deterministic_runner_test",
            "matrix_sha256": self.matrix_hash,
            "product_version": "2026.36.2",
            "profile": {
                "id": "hub-http-v1",
                "revision": "1.0.0",
                "path": str(self.profile),
                "sha256": sha256(self.profile / "SHA256SUMS"),
            },
            "jobs": jobs,
        }
        path = private_json(self.root / "config.json", config)
        return config, path

    def _run(self, config_path):
        receipt = self.root / "receipt.json"
        code = matrix_run.run_matrix(config_path, receipt, require_complete=True)
        return code, json.loads(receipt.read_text(encoding="utf-8"))

    @staticmethod
    def _spec_path(job):
        environment = json.loads(Path(job["environment_file"]).read_text(encoding="utf-8"))
        return Path(environment["MATRIX_TEST_SPEC"])

    def test_matrix_declares_exact_twenty_one_required_cells_and_client_cases(self):
        self.assertEqual(1, self.matrix["schema_version"])
        self.assertEqual(21, len(self.matrix["cells"]))
        self.assertEqual(21, len({cell["id"] for cell in self.matrix["cells"]}))
        self.assertEqual(
            {
                "protocol_actual_hub",
                "typescript_node",
                "typescript_browser",
                "viewer",
                "swift",
                "home_assistant",
                "edge_v2",
            },
            {cell["client_id"] for cell in self.matrix["cells"]},
        )
        self.assertEqual(
            {"macos_arm64", "debian13_amd64", "debian13_arm64"},
            {cell["hub_target"] for cell in self.matrix["cells"]},
        )
        self.assertTrue(all(cell["required"] for cell in self.matrix["cells"]))
        self.assertTrue(
            all(self._required_cases(cell) for cell in self.matrix["cells"])
        )

    def test_published_config_and_receipt_schemas_are_valid(self):
        from jsonschema import Draft202012Validator

        config_schema = json.loads(
            (Path(__file__).with_name("matrix_runner") / "config.schema.json").read_text(
                encoding="utf-8"
            )
        )
        receipt_schema = json.loads(
            (ROOT / "docs/compatibility/receipt.schema.json").read_text(encoding="utf-8")
        )
        Draft202012Validator.check_schema(config_schema)
        Draft202012Validator.check_schema(receipt_schema)
        config, _ = self._config()
        self.assertEqual([], list(Draft202012Validator(config_schema).iter_errors(config)))

    def test_complete_deterministic_fixture_passes_admission_but_not_actual_acceptance(self):
        _, path = self._config()
        code, receipt = self._run(path)
        self.assertEqual(0, code)
        self.assertEqual("passed", receipt["status"])
        self.assertTrue(receipt["complete"])
        self.assertEqual("deterministic_runner_test", receipt["execution_kind"])
        self.assertEqual("schema-and-admission-only", receipt["acceptance_scope"])
        self.assertEqual({"passed"}, {cell["status"] for cell in receipt["cells"]})

    def test_absent_and_duplicate_cells_fail_complete_aggregate(self):
        jobs = [self._job(cell) for cell in self.matrix["cells"]]
        jobs.pop()
        duplicate = dict(jobs[0])
        duplicate["evidence_path"] = str(self.root / "duplicate.json")
        duplicate["environment_file"] = str(private_json(
            self.root / "duplicate-env.json",
            json.loads(Path(jobs[0]["environment_file"]).read_text(encoding="utf-8")),
        ))
        jobs.append(duplicate)
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        self.assertFalse(receipt["complete"])
        self.assertIn("duplicate configured cell", receipt["errors"])
        self.assertIn("missing required cell", receipt["errors"])

    def test_missing_or_duplicate_required_case_is_not_admitted(self):
        jobs = [self._job(cell) for cell in self.matrix["cells"]]
        spec = json.loads(self._spec_path(jobs[0]).read_text(encoding="utf-8"))
        spec["payload"]["cases"].pop()
        private_path = self._spec_path(jobs[0])
        private_path.unlink()
        private_json(private_path, spec)
        duplicate_spec_path = self._spec_path(jobs[1])
        duplicate_spec = json.loads(duplicate_spec_path.read_text())
        duplicate_spec["mode"] = "duplicate_case"
        duplicate_spec_path.unlink()
        private_json(duplicate_spec_path, duplicate_spec)
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        by_id = {cell["cell_id"]: cell for cell in receipt["cells"]}
        self.assertEqual("pending", by_id[jobs[0]["cell_id"]]["status"])
        self.assertEqual("failed", by_id[jobs[1]["cell_id"]]["status"])

    def test_preexisting_evidence_is_rejected_without_overwrite(self):
        jobs = [self._job(cell) for cell in self.matrix["cells"]]
        Path(jobs[0]["evidence_path"]).write_text("stale\n", encoding="utf-8")
        os.chmod(jobs[0]["evidence_path"], 0o600)
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        self.assertEqual("stale\n", Path(jobs[0]["evidence_path"]).read_text())
        self.assertIn("evidence path already exists", receipt["cells"][0]["errors"])

    def test_zero_exit_empty_legacy_evidence_is_rejected(self):
        jobs = [
            self._job(cell, mode="empty_legacy" if index == 0 else "pass")
            for index, cell in enumerate(self.matrix["cells"])
        ]
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        self.assertEqual("failed", receipt["cells"][0]["status"])
        self.assertIn("malformed protocol actual hub adapter evidence", receipt["cells"][0]["errors"])

    def test_nonzero_and_timeout_child_fail_and_timeout_cleans_owned_descendants(self):
        jobs = [
            self._job(
                cell,
                mode="nonzero" if index == 0 else "timeout" if index == 1 else "pass",
                timeout=1 if index == 1 else 5,
            )
            for index, cell in enumerate(self.matrix["cells"])
        ]
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        self.assertEqual(7, receipt["cells"][0]["exit_code"])
        self.assertIn("child exited nonzero", receipt["cells"][0]["errors"])
        self.assertIn("child timed out", receipt["cells"][1]["errors"])
        for cell in receipt["cells"][:2]:
            self.assertIsNotNone(cell["execution_log"])
            for key in ("stdout", "stderr"):
                self.assertTrue(Path(cell["execution_log"][key]["path"]).is_file())
        time.sleep(3.2)
        self.assertFalse((self.root / "descendant-leak.txt").exists())

    def test_normal_exit_and_term_resistant_descendants_are_owned_and_killed(self):
        jobs = [
            self._job(cell, mode="normal_descendant" if index == 0 else "term_resistant" if index == 1 else "pass")
            for index, cell in enumerate(self.matrix["cells"])
        ]
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(0, code)
        self.assertEqual("passed", receipt["cells"][0]["status"])
        self.assertEqual("passed", receipt["cells"][1]["status"])
        time.sleep(3.2)
        self.assertFalse((self.root / "descendant-leak.txt").exists())

    def test_meaningless_equal_assertion_is_rejected(self):
        jobs = [self._job(cell, mode="meaningless" if index == 0 else "pass") for index, cell in enumerate(self.matrix["cells"])]
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        self.assertIn("case assertion is empty or meaningless", receipt["cells"][0]["errors"])

    def test_malformed_nested_config_produces_schema_valid_failure_receipt(self):
        config, _ = self._config([])
        config["jobs"] = [None]
        path = private_json(self.root / "malformed-config.json", config)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        self.assertEqual("failed", receipt["status"])
        self.assertIn("config violates schema", receipt["errors"])

    def test_null_command_file_is_rejected_before_any_adapter_runs(self):
        jobs = [self._job(cell) for cell in self.matrix["cells"]]
        jobs[0]["command_files"] = [None]
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        self.assertIn("config violates schema", receipt["errors"])
        self.assertFalse(any(Path(job["evidence_path"]).exists() for job in jobs))

    def test_rejected_secret_argv_is_never_copied_to_public_receipt(self):
        secret = "Bearer sentinel-secret-7781"
        jobs = [self._job(cell) for cell in self.matrix["cells"]]
        jobs[0]["argv"].append(secret)
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        self.assertNotIn(secret, json.dumps(receipt))

    def test_receipt_inside_workspace_source_is_rejected_without_write(self):
        _, config_path = self._config([])
        forbidden = Path(matrix_run.__file__).with_name("forbidden-receipt.json")
        self.addCleanup(lambda: forbidden.unlink(missing_ok=True))
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run.run_matrix(config_path, forbidden)
        self.assertFalse(forbidden.exists())

    def test_identity_mismatch_and_wrong_expected_actual_are_rejected(self):
        jobs = [self._job(cell) for cell in self.matrix["cells"]]
        first_spec = self._spec_path(jobs[0])
        first = json.loads(first_spec.read_text())
        first["payload"]["runtime"]["hub"]["architecture"] = "wrong-arch"
        first_spec.unlink()
        private_json(first_spec, first)
        second_spec = self._spec_path(jobs[1])
        second = json.loads(second_spec.read_text())
        second["mode"] = "wrong_actual"
        second_spec.unlink()
        private_json(second_spec, second)
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        self.assertIn("runtime identity mismatch", receipt["cells"][0]["errors"])
        self.assertEqual("failed", receipt["cells"][1]["case_results"][0]["status"])

    def test_illegal_not_applicable_reaches_case_admission_and_fails(self):
        jobs = [self._job(cell) for cell in self.matrix["cells"]]
        first_spec = self._spec_path(jobs[0])
        first = json.loads(first_spec.read_text())
        first["mode"] = "illegal_na"
        first_spec.unlink()
        private_json(first_spec, first)
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(1, code)
        self.assertEqual("failed", receipt["cells"][0]["case_results"][0]["status"])
        self.assertIn("illegal not-applicable required case", receipt["cells"][0]["errors"])

    def test_unknown_config_fields_fail_closed(self):
        config, _ = self._config([])
        config["surprise"] = True
        config_path = self.root / "unknown-config.json"
        private_json(config_path, config)
        code, receipt = self._run(config_path)
        self.assertEqual(1, code)
        self.assertIn("config violates schema", receipt["errors"])

    def test_exact_supported_case_contract_rejects_wrong_values_types_and_fields(self):
        job = {"artifacts": [
            {"role": "hub_executable", "sha256": "a" * 64},
            {"role": "typescript_sdk_tarball", "sha256": "b" * 64},
        ]}
        valid = {
            "hub_sha256": "a" * 64, "tarball_sha256": "b" * 64,
            "package_version": "2026.36.2", "installed_members": 81,
        }
        matrix_run._validate_actual_assertion("candidate_artifact_identity", valid, valid, job)
        mutations = [
            ("drives_three_page_order", {"pages": [[105, 104], [103, 102], [999]]}),
            ("drives_terminal_cursor", {"next_cursor": "still-open", "ids": [101]}),
            ("exact_current_values", dict(matrix_run.SCENARIO_CURRENT, battery_level=1)),
            ("endpoint_restart", {"same_hub": True, "new_process": False, "vehicles": 200}),
            ("outage_recovery", {"outage_observed": False, "vehicles": 200}),
            ("real_auth", {"claimed": "200", "vehicles": matrix_run.SCENARIO_VEHICLES}),
            ("revocation", {"typed_error": "hub_http_error", "http_status": "401"}),
            ("revocation", {"typed_error": "hub_http_error", "http_status": True}),
            ("drives_terminal_cursor", {"next_cursor": None, "ids": [101], "extra": "public-leak"}),
        ]
        for case_id, value in mutations:
            with self.subTest(case_id=case_id, value=value):
                with self.assertRaises(matrix_run.MatrixError):
                    matrix_run._validate_actual_assertion(case_id, value, value, job)
        wrong_status = [{"method": "POST", "route": "/v1/pairings/11111111-1111-4111-8111-111111111111/claim",
                         "status": 400, "request_id": "bad-invitation"}]
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._validate_transcript("bad_invitation", wrong_status, True)
        wrong_status[0]["status"] = 401
        matrix_run._validate_transcript("bad_invitation", wrong_status, True)

    def test_installed_service_label_and_wrong_type_proof_remain_pending(self):
        identity = self._identity()
        cell = next(item for item in self.matrix["cells"] if item["client_id"] == "typescript_node")
        job = {"adapter": "typescript_node", "cell_id": cell["id"], "argv": ["redacted"],
               "evidence_path": str(self.root / "never-created.json"), **identity}
        case = {"id": "installed_service_runtime", "status": "passed",
                "expected": {"service_mode": "installed-app-launchagent"},
                "actual": {"service_mode": "installed-app-launchagent"},
                "evidence_kind": "identity", "request_transcript": [],
                "process_evidence": {"verifier": "installed-service-v1", "config_sha256": ["wrong-type"]}}
        raw = {"schema_version": 1, "execution_kind": "actual_hub_acceptance",
               "adapter": job["adapter"], "cell_id": job["cell_id"], "product_version": "2026.36.2",
               "profile_id": "hub-http-v1", "profile_revision": "1.0.0",
               "profile_sha256": "c" * 64, **identity, "cases": [case]}
        config = {"execution_kind": "actual_hub_acceptance", "product_version": "2026.36.2",
                  "profile": {"id": "hub-http-v1", "revision": "1.0.0", "sha256": "c" * 64}}
        results, errors = matrix_run._normalize_evidence(raw, job, config, [case["id"]], "d" * 64)
        self.assertEqual("pending", results[0]["status"])
        self.assertIn("pending: installed service verifier is unavailable", errors)

    def test_node_launcher_is_fixed_and_unrelated_executable_is_not_run(self):
        fake = self.root / "printer"
        fake.write_text("ignored", encoding="utf-8")
        job = {"argv": [str(fake)], "command_files": [{"sha256": "a" * 64}]}
        client = {"actual_node_sha256": "a" * 64, "actual_node_version": "v26.7.0"}
        with mock.patch.object(matrix_run.subprocess, "run") as run:
            with self.assertRaises(matrix_run.MatrixError):
                matrix_run._node_launcher_identity(job, client, {"node_sha256": "a" * 64})
            run.assert_not_called()
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._require_cell_adapter(
                {"adapter": "typescript_browser"}, {"adapter": "typescript_node"}
            )

    def test_v2_node_launcher_identity_uses_matrix_pin_for_runner_descriptor(self):
        executable = self.root / "node"
        executable.write_bytes(b"reviewed node fixture")
        job = {"argv": [str(executable)], "command_files": [{"sha256": "a" * 64}]}
        client = {"actual_node_sha256": "a" * 64, "actual_node_version": "v26.7.0"}
        with mock.patch.object(matrix_run, "sha256_file", return_value="a" * 64), \
                mock.patch.object(matrix_run.subprocess, "run", return_value=SimpleNamespace(stdout="v26.7.0\n")) as probe, \
                mock.patch.object(matrix_run, "_local_host_identity", return_value=("macOS", "arm64")):
            identity = matrix_run._node_launcher_identity(
                job, client, {"schema_version": 2, "kind": "installed-client-lane"}
            )
        self.assertEqual("a" * 64, identity["sha256"])
        self.assertEqual([str(executable), "--version"], probe.call_args.args[0])

    def test_linux_node_runtime_normalizes_distribution_and_x64(self):
        hub_hash, sdk_hash = "a" * 64, "b" * 64
        proof = {
            "hub": {"status": "verified", "hub_pid": 10, "launcher_pid": 9,
                    "hub_started_at": "now", "binary_sha256": hub_hash,
                    "seed_binary_sha256": "c" * 64, "cleanup_sha256": "d" * 64},
            "packed": {"packageName": "@teslatlas/sdk", "packageVersion": "2026.36.2",
                       "entry": "dist/node.js", "entrySha256": "e" * 64,
                       "tarballSha256": sdk_hash, "installedContentManifestSha256": "f" * 64,
                       "installedMemberCount": 81},
            "client": {"os": "linux", "distribution": "Debian GNU/Linux 13 (trixie)",
                       "architecture": "x64", "node": "v26.7.0", "pid": 11},
        }
        job = {"adapter": "typescript_node",
               "artifacts": [{"role": "hub_executable", "sha256": hub_hash},
                             {"role": "typescript_sdk_tarball", "sha256": sdk_hash}],
               "runtime": {"client": {"os": "Debian GNU/Linux 13 (trixie)", "architecture": "amd64",
                                        "tool_versions": {"node": "v26.7.0"}}}}
        matrix_run._validate_identity_proof("candidate_artifact_identity", proof, job)
        proof["packed"]["entrySha256"] = ["wrong-type"]
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._validate_identity_proof("candidate_artifact_identity", proof, job)

        browser_proof = json.loads(json.dumps(proof))
        browser_proof["packed"].update(entry="dist/browser.js", entrySha256="e" * 64)
        browser_proof["client"] = {
            "os": "linux", "distribution": "Debian GNU/Linux 13 (trixie)",
            "architecture": "aarch64", "kernel": "6.12.95+deb13-cloud-arm64",
            "uid": 501, "pid": 12, "browser": "152.0.7977.75",
        }
        browser_job = json.loads(json.dumps(job))
        browser_job["adapter"] = "typescript_browser"
        browser_job["runtime"]["client"].update(
            architecture="arm64", tool_versions={"chromium": "152.0.7977.75"}
        )
        matrix_run._validate_identity_proof("candidate_artifact_identity", browser_proof, browser_job)
        for alias in ("arm64", "aarch64"):
            with self.subTest(adapter="typescript_browser", valid_architecture=alias):
                candidate = json.loads(json.dumps(browser_proof))
                candidate["client"]["architecture"] = alias
                matrix_run._validate_identity_proof("candidate_artifact_identity", candidate, browser_job)
        for alias in ("x64", "x86_64", "amd64"):
            with self.subTest(adapter="typescript_node", valid_architecture=alias):
                candidate = json.loads(json.dumps(proof))
                candidate["packed"]["entrySha256"] = "e" * 64
                candidate["client"]["architecture"] = alias
                matrix_run._validate_identity_proof("candidate_artifact_identity", candidate, job)
        for adapter, candidate_job, template in (
            ("typescript_node", job, proof), ("typescript_browser", browser_job, browser_proof)
        ):
            for malformed in ([], {}, None, 7):
                with self.subTest(adapter=adapter, malformed_architecture=malformed):
                    candidate = json.loads(json.dumps(template))
                    candidate["packed"]["entrySha256"] = "e" * 64
                    candidate["client"]["architecture"] = malformed
                    with self.assertRaises(matrix_run.MatrixError):
                        matrix_run._validate_identity_proof(
                            "candidate_artifact_identity", candidate, candidate_job
                        )
        browser_proof["client"]["kernel"] = ["wrong-type"]
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._validate_identity_proof("candidate_artifact_identity", browser_proof, browser_job)

    def test_malformed_candidate_architecture_reaches_schema_valid_aggregate_failure(self):
        cell = next(item for item in self.matrix["cells"] if item["client_id"] == "typescript_node")
        job = self._job(cell)
        descriptor = self._spec_path(job)
        job["argv"].append(str(descriptor))
        digest = sha256(self.artifact)
        job["artifacts"] = [
            {"role": "hub_executable", "name": "hub", "path": str(self.artifact),
             "embedded_version": "2026.36.2", "sha256": digest},
            {"role": "typescript_sdk_tarball", "name": "sdk", "path": str(self.artifact),
             "embedded_version": "2026.36.2", "sha256": digest},
        ]
        job["runtime"]["client"] = {
            "os": "Debian GNU/Linux 13 (trixie)", "architecture": "amd64",
            "native_or_emulated": "native", "service_mode": "owned-node-process",
            "tool_versions": {"node": "v26.7.0"},
        }
        expected = {"hub_sha256": digest, "tarball_sha256": digest,
                    "package_version": "2026.36.2", "installed_members": 81}
        proof = {
            "hub": {"status": "verified", "hub_pid": 10, "launcher_pid": 9,
                    "hub_started_at": "now", "binary_sha256": digest,
                    "seed_binary_sha256": "c" * 64, "cleanup_sha256": "d" * 64},
            "packed": {"packageName": "@teslatlas/sdk", "packageVersion": "2026.36.2",
                       "entry": "dist/node.js", "entrySha256": "e" * 64,
                       "tarballSha256": digest, "installedContentManifestSha256": "f" * 64,
                       "installedMemberCount": 81},
            "client": {"os": "linux", "distribution": "Debian GNU/Linux 13 (trixie)",
                       "architecture": [], "node": "v26.7.0", "pid": 11},
        }
        raw = {
            "schema_version": 1, "execution_kind": "actual_hub_acceptance",
            "adapter": job["adapter"], "cell_id": job["cell_id"],
            "product_version": "2026.36.2", "profile_id": "hub-http-v1",
            "profile_revision": "1.0.0", "profile_sha256": self.matrix["profile"]["manifest_sha256"],
            "source_identities": job["source_identities"], "artifacts": job["artifacts"],
            "runtime": job["runtime"],
            "cases": [{"id": "candidate_artifact_identity", "status": "passed",
                       "expected": expected, "actual": expected, "evidence_kind": "identity",
                       "request_transcript": [], "process_evidence": proof}],
        }
        config = {
            "schema_version": 1, "execution_kind": "actual_hub_acceptance",
            "matrix_sha256": self.matrix_hash, "product_version": "2026.36.2",
            "profile": {"id": "hub-http-v1", "revision": "1.0.0",
                        "path": str(ROOT.parent / "teslatlas-protocol/profiles/hub-http-v1/1.0.0"),
                        "sha256": self.matrix["profile"]["manifest_sha256"]},
            "jobs": [job],
        }
        config_path = private_json(self.root / "malformed-architecture-config.json", config)
        receipt_path = self.root / "malformed-architecture-receipt.json"

        def execute(candidate_job):
            private_json(candidate_job["evidence_path"], raw)
            return {"started_at": "2026-09-05T00:00:00Z", "ended_at": "2026-09-05T00:00:01Z",
                    "exit_code": 0, "error": None, "logs": None}

        observed = (job["source_identities"], job["artifacts"])
        launcher = {"kind": "node", "path": sys.executable, "sha256": sha256(sys.executable),
                    "version": "v26.7.0", "os": "macOS", "architecture": "arm64"}
        with mock.patch.object(matrix_run, "_validate_job"), \
                mock.patch.object(matrix_run, "_observe_job_identities", return_value=observed), \
                mock.patch.object(matrix_run, "_node_launcher_identity", return_value=launcher), \
                mock.patch.object(matrix_run, "_execute", side_effect=execute), \
                mock.patch.object(matrix_run, "_validate_profile"):
            code = matrix_run.run_matrix(config_path, receipt_path, require_complete=True)
        self.assertEqual(1, code)
        receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
        matrix_run._validate_receipt_schema(receipt)
        result = next(item for item in receipt["cells"] if item["cell_id"] == cell["id"])
        self.assertEqual("failed", result["status"])
        self.assertIn("Node runtime identity is not process-bound", result["errors"])

    def test_home_assistant_archive_uses_real_domain_and_root(self):
        source = self.root / "manifest.json"
        source.write_text(json.dumps({"domain": "teslatlas_hub", "version": "2026.36.2"}), encoding="utf-8")
        archive = self.root / "ha.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            output.add(source, arcname="custom_components/teslatlas_hub/manifest.json")
        artifact = {"role": "home_assistant_integration_archive", "name": "ha", "path": str(archive),
                    "embedded_version": "2026.36.2", "sha256": sha256(archive)}
        matrix_run._verify_artifact_version(artifact, "2026.36.2", True)
        wrong = self.root / "wrong-ha.tar.gz"
        with tarfile.open(wrong, "w:gz") as output:
            output.add(source, arcname="custom_components/teslatlas/manifest.json")
        artifact.update(path=str(wrong), sha256=sha256(wrong))
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._verify_artifact_version(artifact, "2026.36.2", True)

    def test_swift_product_archive_binds_package_version_and_matrix_sources(self):
        source = self.root / "swift-product"
        (source / "tools").mkdir(parents=True)
        (source / "Package.swift").write_text(
            "let package = Package(name: \"teslatlas-sdk-swift\")\n", encoding="utf-8"
        )
        (source / "VERSION").write_text("2026.36.2\n", encoding="ascii")
        for name in ("matrix-contract.json", "matrix_contract.py", "matrix_live.py", "matrix_wire.py"):
            (source / "tools" / name).write_text("{}\n", encoding="utf-8")
        archive = self.root / "swift.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            output.add(source, arcname="teslatlas-sdk-swift")
        artifact = {
            "role": "swift_sdk_product", "name": "swift", "path": str(archive),
            "embedded_version": "2026.36.2", "sha256": sha256(archive),
        }
        matrix_run._verify_artifact_version(artifact, "2026.36.2", True)
        (source / "Package.swift").write_text("// foreign package\n", encoding="utf-8")
        wrong_package = self.root / "swift-wrong-package.tar.gz"
        with tarfile.open(wrong_package, "w:gz") as output:
            output.add(source, arcname="teslatlas-sdk-swift")
        artifact.update(path=str(wrong_package), sha256=sha256(wrong_package))
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._verify_artifact_version(artifact, "2026.36.2", True)
        (source / "Package.swift").write_text(
            "let package = Package(name: \"teslatlas-sdk-swift\")\n", encoding="utf-8"
        )
        (source / "VERSION").write_text("2026.36.1\n", encoding="ascii")
        wrong = self.root / "swift-wrong.tar.gz"
        with tarfile.open(wrong, "w:gz") as output:
            output.add(source, arcname="teslatlas-sdk-swift")
        artifact.update(path=str(wrong), sha256=sha256(wrong))
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._verify_artifact_version(artifact, "2026.36.2", True)

    def test_foreign_hub_runtime_is_pending_before_any_probe(self):
        cell = next(item for item in self.matrix["cells"] if item["hub_target"] == "debian13_amd64")
        job = self._job(cell)
        target = self.matrix["hub_targets"][cell["hub_target"]]
        job["runtime"]["hub"].update(os=target["os"], architecture=target["architecture"],
                                     service_mode=target["required_service_mode"])
        with mock.patch.object(matrix_run, "_local_host_identity", return_value=("macOS", "arm64")), \
                mock.patch.object(matrix_run.subprocess, "run") as run:
            with self.assertRaises(matrix_run.PendingCapability):
                matrix_run._validate_job(job, self.matrix, "actual_hub_acceptance")
            run.assert_not_called()

        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._verify_artifact_version(
                {"role": "swift_sdk_product", "path": str(Path(matrix_run.__file__))},
                "2026.36.2", True,
            )

    def test_protocol_descriptor_uses_private_boundary_and_collision_preflight(self):
        job = self._job(self.matrix["cells"][0])
        job["adapter"] = "protocol_actual_hub"
        job["argv"] = [sys.executable, "--profile", "x", "--adapter", "x", "--config", str(Path(matrix_run.__file__)), "--json"]
        config_path = private_json(self.root / "boundary-config.json", {})
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._preflight_private_boundaries({"jobs": [job]}, config_path, self.root / "boundary-receipt.json")
        job["argv"][6] = job["environment_file"]
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._preflight_private_boundaries({"jobs": [job]}, config_path, self.root / "collision-receipt.json")

    def test_typed_client_preflight_may_prove_zero_requests_but_row_still_needs_transport(self):
        jobs = [self._job(cell) for cell in self.matrix["cells"]]
        spec_path = self._spec_path(jobs[0])
        spec = json.loads(spec_path.read_text(encoding="utf-8"))
        case = next(item for item in spec["payload"]["cases"] if item["id"] == "bad_invitation")
        case.update(
            evidence_kind="zero_request",
            expected={"outgoing_requests": 0, "typed_error": "invalidInvitation"},
            actual={"outgoing_requests": 0, "typed_error": "invalidInvitation"},
            request_transcript=[],
        )
        spec_path.unlink()
        private_json(spec_path, spec)
        _, path = self._config(jobs)
        code, receipt = self._run(path)
        self.assertEqual(0, code)
        first = receipt["cells"][0]
        admitted = next(item for item in first["case_results"] if item["id"] == "bad_invitation")
        self.assertEqual("passed", admitted["status"])
        self.assertEqual([], admitted["request_transcript"])

    def test_failure_output_does_not_echo_private_values(self):
        secret = "secret-do-not-print-4bd742"
        config_path = self.root / "bad-config.json"
        private_json(config_path, {"schema_version": 1, "secret": secret})
        receipt = self.root / "bad-receipt.json"
        result = subprocess.run(
            [
                sys.executable,
                str(Path(matrix_run.__file__)),
                "--config",
                str(config_path),
                "--require-complete",
                "--receipt",
                str(receipt),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(1, result.returncode)
        self.assertNotIn(secret, result.stdout + result.stderr + receipt.read_text())
        self.assertEqual("failed", json.loads(receipt.read_text())["status"])

    def test_actual_mode_rejects_deterministic_fixture_configuration(self):
        jobs = [self._job(cell) for cell in self.matrix["cells"]]
        config, _ = self._config(jobs)
        config["execution_kind"] = "actual_hub_acceptance"
        actual_path = self.root / "actual-config.json"
        private_json(actual_path, config)
        code, receipt = self._run(actual_path)
        self.assertEqual(1, code)
        self.assertFalse(receipt["complete"])
        self.assertEqual("failed", receipt["status"])
        self.assertFalse(any(Path(job["evidence_path"]).exists() for job in jobs))

    def test_config_and_receipt_versions_dispatch_without_reinterpreting_v1(self):
        config, path = self._config([])
        loaded = matrix_run.load_config(path, self.matrix, ROOT / "docs/compatibility/matrix.json")
        self.assertEqual(1, loaded["schema_version"])
        config["execution_kind"] = "actual_hub_acceptance"
        config["profile"] = {
            "id": "hub-http-v1", "revision": "1.0.0",
            "path": str(ROOT.parent / "teslatlas-protocol/profiles/hub-http-v1/1.0.0"),
            "sha256": self.matrix["profile"]["manifest_sha256"],
        }
        private_json(self.root / "actual-v1.json", config)
        # Exercise schema-version dispatch independently of the reviewed
        # profile gate. The sibling checkout may contain a local candidate
        # whose manifest is intentionally not the matrix's accepted digest.
        with mock.patch.object(matrix_run, "_validate_profile"):
            loaded_v1 = matrix_run.load_config(
                self.root / "actual-v1.json", self.matrix, ROOT / "docs/compatibility/matrix.json"
            )
        self.assertEqual(1, loaded_v1["schema_version"])
        v2 = dict(config, schema_version=2, cohort_inputs={"path": str(self.artifact), "sha256": sha256(self.artifact)})
        private_json(self.root / "actual-v2.json", v2)
        with mock.patch.object(matrix_run, "_validate_profile"):
            loaded = matrix_run.load_config(self.root / "actual-v2.json", self.matrix, ROOT / "docs/compatibility/matrix.json")
        self.assertEqual(2, loaded["schema_version"])
        receipt = matrix_run._base_receipt(self.matrix, ROOT / "docs/compatibility/matrix.json", "actual_hub_acceptance", v2["cohort_inputs"])
        self.assertEqual(2, receipt["schema_version"])
        self.assertEqual(v2["cohort_inputs"], receipt["cohort_inputs"])

    def test_installed_registration_is_admitted_before_foreign_artifact_probe(self):
        cell = next(item for item in self.matrix["cells"] if item["hub_target"] == "debian13_amd64")
        session_path = private_json(self.root / "installed-session.json", {"closed": "fixture"})
        inventory_path = private_json(self.root / "registration-inventory.json", {"closed": "fixture"})
        job = self._job(cell)
        job["runtime"]["hub"].update(
            os="Debian 13", architecture="amd64", native_or_emulated="native",
            service_mode="installed-deb-systemd",
        )
        job["artifacts"][0]["role"] = "hub_executable"
        job.update(
            installed_session={
                "config": {"path": str(session_path), "sha256": sha256(session_path)},
                "registration_inventory": {"path": str(inventory_path), "sha256": sha256(inventory_path)},
            },
            client_execution={"kind": "local", "registration": None, "relay": None},
        )
        registered = SimpleNamespace(config={
            "cell_id": cell["id"], "adapter_id": cell["adapter"], "client_id": cell["client_id"],
            "expected": {
                "os": "Debian 13", "architecture": "amd64", "native_or_emulated": "native",
                "service_mode": "installed-deb-systemd", "product_version": "2026.36.2",
                "hub_executable_sha256": job["artifacts"][0]["sha256"],
            },
        })
        with mock.patch.object(matrix_run, "_read_registered_config", return_value=registered) as read, \
                mock.patch.object(matrix_run, "_verify_artifact_version") as probe:
            matrix_run._validate_installed_job(job, cell, self.matrix)
        read.assert_called_once()
        probe.assert_not_called()

    def test_cohort_binding_is_rehashed_and_semantically_bound_to_jobs(self):
        cell = self.matrix["cells"][0]
        job = self._job(cell)
        manifest_path = private_json(self.root / "cohort-inputs.json", {"fixture": True})
        binding = {"path": str(manifest_path), "sha256": sha256(manifest_path)}
        cohort = {
            "schema_version": 2, "cohort_id": "cohort", "product_version": "2026.36.2",
            "repository_observations": [
                {"source_identity": source, "snapshot": {"path": str(manifest_path), "sha256": binding["sha256"]}}
                for source in job["source_identities"]
            ],
            "builds": [{"outputs": [dict(artifact, type="file", members=[] ) for artifact in job["artifacts"]]}],
        }
        with mock.patch.object(matrix_run._source_evidence, "validate_cohort_inputs", return_value=cohort) as validate:
            admitted = matrix_run._validate_cohort_binding(binding, "2026.36.2", [job])
        self.assertEqual(cohort, admitted)
        validate.assert_called_once_with(str(manifest_path.resolve()), validation_mode="live")
        manifest_path.write_text("changed\n", encoding="utf-8")
        with self.assertRaises(matrix_run.MatrixError):
            matrix_run._validate_cohort_binding(binding, "2026.36.2", [job])


if __name__ == "__main__":
    unittest.main()
