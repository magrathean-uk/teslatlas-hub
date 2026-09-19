# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for the source-fixed Home Assistant installed seam."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import shutil
import ssl
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from types import MappingProxyType, SimpleNamespace
from unittest import mock

from . import home_assistant_installed
from .adapter_wire import load_reviewed_contract

SESSION_ID = "11111111-1111-4111-8111-111111111111"


def digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


class HomeAssistantInstalledTests(unittest.TestCase):
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
    def current_source_bindings():
        return MappingProxyType(
            {
                relative: digest(
                    (home_assistant_installed.HA_ROOT / relative).read_bytes()
                )
                for relative in home_assistant_installed.REVIEWED_SOURCES
            }
        )

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
                "openssl",
                "req",
                "-x509",
                "-nodes",
                "-newkey",
                "rsa:2048",
                "-keyout",
                str(key),
                "-out",
                str(certificate),
                "-days",
                "1",
                "-config",
                str(config),
            ],
            stdin=subprocess.DEVNULL,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, result.returncode, result.stderr.decode(errors="replace"))
        key.chmod(0o600)
        certificate.chmod(0o600)
        der = ssl.PEM_cert_to_DER_cert(certificate.read_text(encoding="ascii"))
        return certificate, digest(bytes.fromhex(der) if isinstance(der, str) else der)

    def archive(self) -> Path:
        path = self.root / "teslatlas-ha.tar.gz"
        component = home_assistant_installed.COMPONENT_ROOT
        with tarfile.open(path, "w:gz") as archive:
            for row in home_assistant_installed.COMPONENT_ROWS:
                relative = Path(row["path"])
                archive.add(
                    component / relative,
                    arcname=str(Path("custom_components/teslatlas_hub") / relative),
                )
        path.chmod(0o600)
        return path

    def inputs(self):
        scenario = self.write(
            "scenario.json",
            b'{"name":"two-vehicles-five-drives","provenance":"synthetic-only"}\n',
        )
        session_config = self.write(
            "installed-session.json",
            json.dumps(
                {"run_id": "ha-run", "scenario": self.binding(scenario)},
                sort_keys=True,
                separators=(",", ":"),
            ).encode()
            + b"\n",
        )
        installed_root = self.root / "installed/custom_components/teslatlas_hub"
        for row in home_assistant_installed.COMPONENT_ROWS:
            target = installed_root / row["path"]
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            shutil.copyfile(
                home_assistant_installed.COMPONENT_ROOT / row["path"], target
            )
            target.chmod(0o644)
        environment = self.write(
            "environment.json",
            json.dumps(
                {"TESLATLAS_HA_INSTALLED_ROOT": str(installed_root)},
                sort_keys=True,
                separators=(",", ":"),
            ).encode()
            + b"\n",
        )
        certificate, certificate_der_sha256 = self.certificate()
        archive = self.archive()
        hub = self.write("teslatlas-hub", b"fixed-hub-artifact", 0o500)
        job = {
            "adapter": "home_assistant",
            "cell_id": "home_assistant__debian13_amd64",
            "timeout_seconds": 60,
            "environment_file": str(environment),
            "installed_session": {"config": self.binding(session_config)},
            "source_identities": [
                {
                    "role": "hub_source",
                    "repo": str(home_assistant_installed.WORKSPACE / "hub"),
                },
                {
                    "role": "protocol_source",
                    "repo": str(
                        home_assistant_installed.WORKSPACE / "teslatlas-protocol"
                    ),
                },
                {
                    "role": "home_assistant_source",
                    "repo": str(home_assistant_installed.HA_ROOT),
                },
            ],
            "artifacts": [
                {
                    "role": "hub_executable",
                    "name": "teslatlas-hub",
                    "path": str(hub),
                    "embedded_version": "2026.36.2",
                    "sha256": digest(hub.read_bytes()),
                },
                {
                    "role": "home_assistant_integration_archive",
                    "name": "teslatlas-ha",
                    "path": str(archive),
                    "embedded_version": "2026.36.2",
                    "sha256": digest(archive.read_bytes()),
                },
            ],
            "runtime": {
                "hub": {
                    "os": "Debian 13",
                    "architecture": "amd64",
                    "native_or_emulated": "native",
                    "service_mode": "installed-deb-systemd",
                    "tool_versions": {"hub": "2026.36.2"},
                },
                "client": {
                    "os": "Linux",
                    "architecture": "amd64",
                    "native_or_emulated": "native",
                    "service_mode": "docker-container",
                    "tool_versions": {"python": "3.14.2", "home_assistant": "2026.8.3"},
                },
                "browser_engines": [],
                "client_transports": ["aiohttp"],
            },
            "argv": ["/tmp/foreign-python", "/tmp/foreign-launcher"],
            "validator_path": "/tmp/foreign-validator",
            "output_path": "/tmp/foreign-output",
            "broker_socket": "/tmp/foreign-broker",
        }
        cell = {
            "id": job["cell_id"],
            "client_id": "home_assistant",
            "hub_target": "debian13_amd64",
        }
        profile_root = (
            home_assistant_installed.COMPONENT_ROOT / "profile/hub-http-v1/1.0.0"
        )
        config = {
            "product_version": "2026.36.2",
            "profile": {
                "id": "hub-http-v1",
                "revision": "1.0.0",
                "path": str(profile_root),
                "sha256": digest((profile_root / "SHA256SUMS").read_bytes()),
            },
        }
        descriptor = {
            "schema_version": 1,
            "kind": "installed-host",
            "broker_socket": str(self.broker_root / "controller.sock"),
            "session_id": SESSION_ID,
            "registration_sha256": "1" * 64,
        }
        running = {"descriptor": {"certificate_path": str(certificate)}}
        return (
            job,
            cell,
            config,
            descriptor,
            running,
            certificate_der_sha256,
            installed_root,
        )

    def test_reviewed_contract_sources_and_topology_are_fixed(self):
        self.assertEqual(
            "6ab5147162a34c61dd8f1fb38361b2dd7db2dc4684864b1c3ac6a3c1076413a6",
            home_assistant_installed.HANDOFF.sha256,
        )
        self.assertEqual(
            "e74a955374003be9820a450174715412b4a9c1d504b47eb33d1f93e6b2173832",
            home_assistant_installed.SOURCE_MANIFEST.sha256,
        )
        self.assertEqual(
            "5935c634766b397162fccfa74348a535bbcb8b70c69bb66223bd9e69f5cce108",
            home_assistant_installed.REVIEWED_SOURCES["tests/test_matrix_live.py"],
        )
        home_assistant_installed._validate_reviewed_sources()
        home_assistant_installed._validate_handoff()
        loaded = load_reviewed_contract(
            "home_assistant",
            {"home_assistant": home_assistant_installed.CONTRACT},
            private=False,
        )
        self.assertEqual("home_assistant", loaded.manifest["adapter_id"])
        self.assertEqual(
            ("ha_flow", "ha_client"),
            tuple(row["id"] for row in loaded.manifest["actors"]),
        )
        self.assertEqual(18, len(loaded.manifest["required_cases"]))
        self.assertEqual(
            (
                ("macos_arm64", "docker_exec_pipe"),
                ("debian13_amd64", "docker_exec_pipe"),
                ("debian13_arm64", "docker_exec_pipe"),
            ),
            home_assistant_installed.execution_by_target(),
        )
        self.assertEqual(
            (
                ("macos_arm64", "stdio"),
                ("debian13_amd64", "stdio"),
                ("debian13_arm64", "stdio"),
            ),
            home_assistant_installed.broker_kind_by_target(),
        )
        self.assertEqual(4, len(home_assistant_installed.RAW_SCHEMAS))
        self.assertEqual(10, len(home_assistant_installed.REVIEWED_SOURCES))
        self.assertEqual(31, len(home_assistant_installed.COMPONENT_ROWS))

    def test_build_stages_exact_component_profile_and_two_actor_session(self):
        (
            job,
            cell,
            config,
            descriptor,
            running,
            certificate_der_sha256,
            installed_root,
        ) = self.inputs()
        job = {
            key: value
            for key, value in job.items()
            if key
            not in {
                "validator_path",
                "output_path",
                "broker_socket",
            }
        }
        contract = load_reviewed_contract(
            "home_assistant",
            {"home_assistant": home_assistant_installed.CONTRACT},
            private=False,
        )
        with mock.patch.object(
            home_assistant_installed,
            "REVIEWED_SOURCES",
            self.current_source_bindings(),
        ):
            path, session = home_assistant_installed.build_session_input(
                job,
                cell,
                config,
                {},
                contract,
                descriptor,
                running,
            )
        specification = importlib.util.spec_from_file_location(
            "_ha_matrix_wire_hub_test",
            home_assistant_installed.HA_ROOT / "tools/matrix_wire.py",
        )
        wire = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(wire)
        loaded = wire.load_session_input(path)
        self.assertEqual(session, loaded)
        self.assertEqual(session, json.loads(path.read_text(encoding="utf-8")))
        self.assertEqual({"kind": "stdio", "socket_path": None}, session["broker"])
        self.assertEqual(18, len(session["inputs"]["profile_members"]))
        self.assertEqual(
            certificate_der_sha256, session["inputs"]["certificate_der_sha256"]
        )
        self.assertEqual(
            ["ha_flow", "ha_client"], [actor["id"] for actor in session["actors"]]
        )
        self.assertEqual(
            ["ha_matrix_flow", "ha_matrix_client"],
            [actor["entrypoint_ref"] for actor in session["actors"]],
        )
        product = session["inputs"]["product_inputs"][0]
        manifest = json.loads(
            Path(product["installed_manifest"]["local"]["path"]).read_text()
        )
        self.assertEqual(job["artifacts"][1]["sha256"], manifest["artifact_sha256"])
        self.assertEqual(31, len(manifest["files"]))
        self.assertEqual("__init__.py", manifest["files"][0]["path"])
        self.assertEqual(str(installed_root), product["local_root"])

    def test_build_fails_closed_when_a_reviewed_source_digest_changes(self):
        job, cell, config, descriptor, running, _, _installed_root = self.inputs()
        job = {
            key: value
            for key, value in job.items()
            if key
            not in {
                "validator_path",
                "output_path",
                "broker_socket",
            }
        }
        changed = dict(self.current_source_bindings())
        changed["tools/matrix_live.py"] = "0" * 64
        contract = load_reviewed_contract(
            "home_assistant",
            {"home_assistant": home_assistant_installed.CONTRACT},
            private=False,
        )
        with (
            mock.patch.object(
                home_assistant_installed, "REVIEWED_SOURCES", MappingProxyType(changed)
            ),
            self.assertRaisesRegex(
                home_assistant_installed.HomeAssistantInstalledPending, "source changed"
            ),
        ):
            home_assistant_installed.build_session_input(
                job,
                cell,
                config,
                {},
                contract,
                descriptor,
                running,
            )

    def test_build_fails_closed_when_reviewed_handoff_digest_changes(self):
        job, cell, config, descriptor, running, _, _installed_root = self.inputs()
        job = {
            key: value
            for key, value in job.items()
            if key not in {"validator_path", "output_path", "broker_socket"}
        }
        contract = load_reviewed_contract(
            "home_assistant",
            {"home_assistant": home_assistant_installed.CONTRACT},
            private=False,
        )
        changed = home_assistant_installed.ReviewedFile(
            home_assistant_installed.HANDOFF.path, "0" * 64
        )
        with (
            mock.patch.object(home_assistant_installed, "HANDOFF", changed),
            self.assertRaisesRegex(
                home_assistant_installed.HomeAssistantInstalledPending,
                "handoff changed",
            ),
        ):
            home_assistant_installed.build_session_input(
                job, cell, config, {}, contract, descriptor, running
            )

    def test_build_rejects_job_selected_adapter_authority(self):
        job, cell, config, descriptor, running, _, _installed_root = self.inputs()
        contract = load_reviewed_contract(
            "home_assistant",
            {"home_assistant": home_assistant_installed.CONTRACT},
            private=False,
        )
        with self.assertRaisesRegex(
            home_assistant_installed.HomeAssistantInstalledPending, "authority"
        ):
            home_assistant_installed.build_session_input(
                job,
                cell,
                config,
                {},
                contract,
                descriptor,
                running,
            )

    def test_source_launcher_is_fixed_and_runtime_registration_is_required(self):
        job, cell, config, _descriptor, _running, _, _installed_root = self.inputs()
        job = {
            key: value
            for key, value in job.items()
            if key
            not in {
                "validator_path",
                "output_path",
                "broker_socket",
            }
        }
        with (
            mock.patch.object(
                home_assistant_installed,
                "REVIEWED_SOURCES",
                self.current_source_bindings(),
            ),
            self.assertRaisesRegex(
                home_assistant_installed.HomeAssistantInstalledPending,
                "runtime registration",
            ),
        ):
            home_assistant_installed.launch_adapter(
                job,
                cell,
                config,
                object(),
                self.root / "session.json",
                {},
            )

    def test_source_callbacks_are_fixed_and_runtime_dependent_paths_remain_pending(self):
        self.assertIs(
            home_assistant_installed.CONTRACT,
            home_assistant_installed.source_entry("home_assistant"),
        )
        with self.assertRaisesRegex(
            home_assistant_installed.HomeAssistantInstalledPending,
            "identity is unavailable",
        ):
            home_assistant_installed.source_entry("foreign")
        for callback in (
            home_assistant_installed.runtime_inventory,
            home_assistant_installed.admit,
            home_assistant_installed.build_supplement,
        ):
            with self.subTest(callback=callback.__name__), self.assertRaisesRegex(
                home_assistant_installed.HomeAssistantInstalledPending,
                "runtime registration",
            ):
                callback()

    def test_execution_logs_hashes_source_fixed_framework_streams(self):
        framework = self.write("outputs/framework.log", b"ha stdout\n")
        stderr = self.write("outputs/framework.log.stderr.log", b"ha stderr\n")
        result = SimpleNamespace(
            session_input={"outputs": {"framework_log": str(framework)}}
        )
        logs = home_assistant_installed.execution_logs(
            {}, {}, {}, object(), result
        )
        self.assertEqual(
            {"sha256": digest(framework.read_bytes()), "bytes": 10, "truncated": False},
            logs["stdout"],
        )
        self.assertEqual(
            {"sha256": digest(stderr.read_bytes()), "bytes": 10, "truncated": False},
            logs["stderr"],
        )
        self.assertEqual(0, logs["duration_ms"])


if __name__ == "__main__":
    unittest.main()
