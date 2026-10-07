"""Regression tests for the Protocol installed v2 adapter surface."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
import unittest
from unittest import mock

from conformance import hub_matrix


ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles" / "hub-http-v1" / "1.1.0"
CONTRACT = ROOT / "tools" / "matrix-contract.json"
SESSION_ID = "11111111-1111-4111-8111-111111111111"
HUB_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"


def digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


class ProtocolV2Tests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.private = self.root / "run"
        self.private.mkdir(mode=0o700)

    def _write(self, relative: str, raw: bytes, *, mode: int = 0o600) -> Path:
        path = self.private / relative
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        path.write_bytes(raw)
        path.chmod(mode)
        return path

    def _binding(self, path: Path) -> dict[str, str]:
        return {"path": str(path), "sha256": digest(path.read_bytes())}

    def _staged(self, name: str, path: Path) -> dict[str, object]:
        binding = self._binding(path)
        return {"id": name, "root": binding, "local": dict(binding)}

    def _certificate(self) -> tuple[Path, str]:
        config = self._write(
            "openssl.cnf",
            b"[req]\nprompt=no\ndistinguished_name=dn\n[v3]\n"
            b"basicConstraints=critical,CA:TRUE\nkeyUsage=critical,digitalSignature,keyCertSign\n"
            b"subjectAltName=IP:127.0.0.1\n[dn]\nCN=127.0.0.1\n",
        )
        key = self.private / "key.pem"
        cert = self.private / "cert.pem"
        result = subprocess.run(
            [
                "openssl", "req", "-x509", "-nodes", "-newkey", "rsa:2048",
                "-keyout", str(key), "-out", str(cert), "-days", "1",
                "-config", str(config),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        cert.chmod(0o600)
        key.chmod(0o600)
        return cert, digest(__import__("ssl").PEM_cert_to_DER_cert(cert.read_text()))

    def _config_and_session(self) -> tuple[dict, Path]:
        profile_dir = self.private / "profile" / "hub-http-v1" / "1.1.0"
        shutil.copytree(PROFILE, profile_dir)
        for path in profile_dir.rglob("*"):
            if path.is_file():
                path.chmod(0o600)
        manifest_lines = (profile_dir / "SHA256SUMS").read_text(encoding="ascii").splitlines()
        profile_members = [self._staged("profile-manifest", profile_dir / "SHA256SUMS")]
        for index, line in enumerate(manifest_lines, start=1):
            name = line.split("  ", 1)[1]
            profile_members.append(self._staged(f"profile-member-{index}", profile_dir / name))
        certificate, certificate_der_sha256 = self._certificate()
        scenario = self._write("scenario.json", b'{"name":"two-vehicles-five-drives","provenance":"synthetic-only"}\n')
        build_record = self._write("build-record.json", b'{"schema_version":1}\n')
        actor_manifest = self._write(
            "actor-manifest.json",
            json.dumps(
                {
                    "schema_version": 1,
                    "build_record": self._binding(build_record),
                    "files": [
                        {"path": "conformance/run", "bytes": 1, "mode": 0o500, "sha256": "0" * 64}
                    ],
                },
                separators=(",", ":"),
            ).encode()
            + b"\n",
        )
        header_value = {
            "schema_version": 1,
            "execution_kind": "actual_hub_acceptance",
            "adapter": "protocol_actual_hub",
            "cell_id": "protocol_actual_hub__macos_arm64",
            "product_version": "2026.36.2",
            "profile_id": "hub-http-v1",
            "profile_revision": "1.1.0",
            "profile_sha256": digest((profile_dir / "SHA256SUMS").read_bytes()),
            "source_identities": [
                {"role": "hub_source", "repo": "/workspace/hub", "head": "a" * 40, "dirty_patch_sha256": "b" * 64, "untracked_source_manifest_sha256": "c" * 64},
                {"role": "protocol_source", "repo": str(ROOT), "head": "d" * 40, "dirty_patch_sha256": "e" * 64, "untracked_source_manifest_sha256": "f" * 64},
            ],
            "artifacts": [
                {"role": "hub_executable", "name": "teslatlas-hub", "path": "/workspace/hub", "embedded_version": "2026.36.2", "sha256": "1" * 64},
                {"role": "protocol_fixture_seed", "name": "interop-seed", "path": "/workspace/seed", "embedded_version": "tooling", "sha256": "2" * 64},
            ],
            "runtime": {
                "hub": {"os": "Debian 13", "architecture": "arm64", "native_or_emulated": "native", "service_mode": "installed-deb-systemd", "tool_versions": {"hub": "2026.36.2"}},
                "client": {"os": "macOS", "architecture": "arm64", "native_or_emulated": "native", "service_mode": "protocol-conformance-process", "tool_versions": {"python": "3.11"}},
                "browser_engines": [],
                "client_transports": ["http.client"],
            },
        }
        header = self._write("header.json", json.dumps(header_value, sort_keys=True, separators=(",", ":")).encode() + b"\n")
        contract = self._write("case-contract.json", CONTRACT.read_bytes())
        outputs = {
            "normalized": str(self.private / "normalized.json"),
            "actor_evidence": str(self.private / "actor-evidence.json"),
            "coordination_dir": str(self.private / "coordination"),
            "framework_log": str(self.private / "framework.log"),
        }
        session = {
            "schema_version": 1,
            "kind": "matrix-adapter-session",
            "run_id": "run-v2",
            "cell_id": header_value["cell_id"],
            "adapter_id": "protocol_actual_hub",
            "client_id": "protocol_actual_hub",
            "session_id": SESSION_ID,
            "instance_nonce": "3" * 64,
            "header": self._staged("header", header),
            "case_contract": self._staged("case-contract", contract),
            "host_session": {
                "schema_version": 1,
                "kind": "installed-host",
                "broker_socket": str(self.private / "broker.sock"),
                "session_id": SESSION_ID,
                "registration_sha256": "4" * 64,
            },
            "broker": {"kind": "unix", "socket_path": str(self.private / "broker.sock")},
            "inputs": {
                "profile_manifest": profile_members[0],
                "profile_members": profile_members,
                "scenario": self._staged("scenario", scenario),
                "certificate": self._staged("certificate", certificate),
                "certificate_der_sha256": certificate_der_sha256,
                "product_inputs": [],
            },
            "actors": [
                {
                    "id": "protocol_http",
                    "kind": "protocol_http",
                    "execution": "coordinator",
                    "runtime_ref": "root_python",
                    "artifact_roles": [],
                    "source_roles": ["protocol_source"],
                    "entrypoint_ref": "protocol_actual_hub",
                    "input_manifest": self._staged("actor-input", actor_manifest),
                    "phase_contract": None,
                }
            ],
            "outputs": outputs,
            "bounds": {
                "cell_timeout_ms": 60_000,
                "cleanup_timeout_ms": 45_000,
                "frame_bytes": 1_048_576,
                "evidence_bytes": 8_388_608,
                "framework_log_bytes": 8_388_608,
            },
        }
        session_path = self._write("session.json", json.dumps(session, sort_keys=True, separators=(",", ":")).encode() + b"\n")
        config = {
            "schema_version": 2,
            "kind": "protocol-actual-hub-matrix",
            "execution_kind": "actual_hub_acceptance",
            "adapter": "protocol_actual_hub",
            "cell_id": header_value["cell_id"],
            "product_version": "2026.36.2",
            "profile": {"id": "hub-http-v1", "revision": "1.1.0", "path": str(PROFILE), "sha256": profile_sha256(PROFILE)},
            "source_identities": header_value["source_identities"],
            "artifacts": header_value["artifacts"],
            "runtime": header_value["runtime"],
            "host_session": session["host_session"],
            "session_input": self._binding(session_path),
        }
        return config, session_path

    def test_v2_config_accepts_closed_session_input_and_rejects_mutation(self):
        config, session_path = self._config_and_session()
        admitted = hub_matrix.validate_matrix_config(config)
        self.assertEqual(admitted["schema_version"], 2)
        self.assertEqual(admitted["session_input"]["path"], str(session_path))
        from_session = hub_matrix._config_from_session_input(session_path)
        self.assertEqual(from_session["schema_version"], admitted["schema_version"])
        self.assertEqual(from_session["profile"]["sha256"], admitted["profile"]["sha256"])
        self.assertEqual(from_session["host_session"], admitted["host_session"])
        mutated = json.loads(session_path.read_text())
        mutated["actors"][0]["entrypoint_ref"] = "other"
        session_path.write_text(json.dumps(mutated, sort_keys=True, separators=(",", ":")) + "\n")
        with self.assertRaises(hub_matrix.MatrixAcceptanceError):
            hub_matrix.validate_matrix_config(config)

    def test_v2_config_rejects_unknown_session_fields_and_stale_binding(self):
        config, session_path = self._config_and_session()
        value = json.loads(session_path.read_text())
        value["unexpected"] = True
        session_path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")
        with self.assertRaises(hub_matrix.MatrixAcceptanceError):
            hub_matrix.validate_matrix_config(config)
        value.pop("unexpected")
        session_path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")
        config["session_input"]["sha256"] = "0" * 64
        with self.assertRaises(hub_matrix.MatrixAcceptanceError):
            hub_matrix.validate_matrix_config(config)

    def test_close_ack_is_waited_for_and_bound_to_ready(self):
        coordination = self.private / "coordination"
        ready_binding = hub_matrix._write_exclusive_json(coordination / "ready.json", {"phase": "evidence_ready"})
        close_binding = hub_matrix._write_exclusive_json(coordination / "close-evidence.json", {"state": "closed"})
        session = {
            "session_id": SESSION_ID,
            "cell_id": "protocol_actual_hub__macos_arm64",
            "_session_input_sha256": "a" * 64,
            "instance_nonce": "b" * 64,
        }

        def publish():
            time.sleep(0.03)
            hub_matrix._write_exclusive_json(
                coordination / "ack-000001.json",
                {
                    "schema_version": 1,
                    "type": "ack",
                    "session_id": SESSION_ID,
                    "cell_id": session["cell_id"],
                    "session_input_sha256": session["_session_input_sha256"],
                    "instance_nonce": session["instance_nonce"],
                    "sequence": 1,
                    "ready_sha256": ready_binding["sha256"],
                    "phase": "evidence_ready",
                    "status": "accepted",
                    "action": "close_completed",
                    "result": close_binding,
                },
            )

        thread = threading.Thread(target=publish)
        thread.start()
        acknowledged = hub_matrix._wait_for_close_ack(
            coordination, ready_binding, session, timeout_seconds=1.0
        )
        thread.join(timeout=1)
        self.assertEqual(acknowledged["action"], "close_completed")

    def _synthetic_control(self):
        from test_hub_matrix import SyntheticControl, SyntheticHub

        server_root = self.root / "synthetic-server"
        server_root.mkdir()
        hub = SyntheticHub(server_root)
        self.addCleanup(hub.close)

        class Control(SyntheticControl):
            def _running(self, invitation=None):
                value = super()._running(invitation)
                value["descriptor"]["binary_sha256"] = "1" * 64
                value["descriptor"]["seed_binary_sha256"] = "2" * 64
                value["proof"]["service"]["hub"]["executable_sha256"] = "1" * 64
                value["proof"]["config"]["seed_sha256"] = "2" * 64
                self.observations[self.sequence] = copy.deepcopy(value["proof"])
                return value

        return Control(hub)

    def _assert_outputs_absent(self, session):
        coordination = Path(session["outputs"]["coordination_dir"])
        for path in (
            Path(session["outputs"]["normalized"]),
            Path(session["outputs"]["actor_evidence"]),
            coordination / "raw", coordination / "adapter-completion.json",
            coordination / "ready-000001.json",
        ):
            self.assertFalse(path.exists(), str(path))

    def test_v2_completion_refuses_unobserved_cleanup_before_publication(self):
        from test_hub_matrix import FIRST_DEVICE, SECOND_DEVICE

        config, session_path = self._config_and_session()
        session = json.loads(session_path.read_text())
        control = self._synthetic_control()
        matrix = hub_matrix.MatrixCases(config, control)
        cases = matrix.run()
        self.assertEqual({case["status"] for case in cases}, {"passed"})
        for case in cases:
            if "process_evidence" in case:
                self.assertEqual(case["process_evidence"]["installed_observation"], control.observations[matrix.anchors[case["id"]][1]])
        self.assertIsNone(matrix.case_device_ids["unauthenticated_discovery"])
        self.assertEqual(matrix.case_device_ids["real_auth"], FIRST_DEVICE)
        self.assertEqual(matrix.case_device_ids["revocation"], FIRST_DEVICE)
        self.assertEqual(matrix.case_device_ids["credential_lifecycle_reauth"], SECOND_DEVICE)
        with self.assertRaisesRegex(hub_matrix.MatrixAcceptanceError, "raw cleanup contract lacks an observed actor-exit witness"):
            hub_matrix._complete_v2(config, session, matrix, cases)
        self._assert_outputs_absent(session)

    def test_owned_v2_refusal_closes_broker_without_waiting_for_ack(self):
        config, session_path = self._config_and_session()
        session = json.loads(session_path.read_text())
        control = self._synthetic_control()
        with (
            mock.patch.object(hub_matrix.hub_control.ControlClient, "connect", return_value=control),
            mock.patch.object(control, "close", wraps=control.close) as closed,
            mock.patch.object(hub_matrix, "_wait_for_close_ack", side_effect=AssertionError("unpublished evidence cannot request close Ack")) as acknowledgement,
            self.assertRaisesRegex(hub_matrix.MatrixAcceptanceError, "raw cleanup contract lacks an observed actor-exit witness"),
        ):
            hub_matrix.run_matrix(config)
        closed.assert_called_once_with()
        acknowledgement.assert_not_called()
        self._assert_outputs_absent(session)

    def test_injected_v2_refusal_preserves_caller_owned_broker(self):
        config, session_path = self._config_and_session()
        session = json.loads(session_path.read_text())
        control = self._synthetic_control()
        with (
            mock.patch.object(control, "close", wraps=control.close) as closed,
            self.assertRaisesRegex(hub_matrix.MatrixAcceptanceError, "raw cleanup contract lacks an observed actor-exit witness"),
        ):
            hub_matrix.run_matrix(config, control=control)
        closed.assert_not_called()
        self._assert_outputs_absent(session)

    def test_v2_completion_rejects_unrecorded_context_before_publishing(self):
        config, session_path = self._config_and_session()
        session = json.loads(session_path.read_text())
        cases = [{"id": case_id, "status": "passed"} for case_id in hub_matrix.CASE_IDS]

        class Matrix:
            proof = {"status": "verified", "sequence": 7}
            device_id = None
            anchors = {case_id: (2, 3) for case_id in hub_matrix.CASE_IDS}
            case_device_ids = {case_id: None for case_id in hub_matrix.CASE_IDS}

        for field, missing in (("anchors", {}), ("case_device_ids", {}), ("anchors", {case_id: (True, 3) for case_id in hub_matrix.CASE_IDS})):
            matrix = Matrix()
            setattr(matrix, field, missing)
            with self.subTest(field=field, missing=missing), self.assertRaisesRegex(hub_matrix.MatrixAcceptanceError, "lacks recorded observation context"):
                hub_matrix._complete_v2(config, session, matrix, cases)
            self.assertFalse((self.private / "normalized.json").exists())
            self.assertFalse((self.private / "coordination" / "raw").exists())


def profile_sha256(path: Path) -> str:
    return digest((path / "SHA256SUMS").read_bytes())


if __name__ == "__main__":
    unittest.main()
