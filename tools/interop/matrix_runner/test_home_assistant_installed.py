# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for the source-fixed Home Assistant installed seam."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import io
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
            "08ad12cfee7e05aaca92c61ea81b1c5e169328416c88e0b46944a0e106db5f11",
            home_assistant_installed.HANDOFF.sha256,
        )
        self.assertEqual(
            "cc1f5e37e8678e492b0fdf664ab42ef0e1c5cb82a2ebf7a1cef0b1934b7966bc",
            home_assistant_installed.SOURCE_MANIFEST.sha256,
        )
        self.assertEqual(
            "522b48ab73f6f36ffc74c279a6b49254abcfbb710cc492c6af599f71e896619a",
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
        self.assertEqual(32, len(home_assistant_installed.COMPONENT_ROWS))

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
        self.assertEqual(32, len(manifest["files"]))
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


class HomeAssistantSourceSuccessorTests(unittest.TestCase):
    """Actual adopted source gate, plus labelled byte/parsed-I/O refusals."""

    def setUp(self):
        self.receiver = home_assistant_installed
        self.handoff_raw = self.receiver.HANDOFF.path.read_bytes()
        self.manifest_raw = self.receiver.SOURCE_MANIFEST.path.read_bytes()
        self.handoff = json.loads(self.handoff_raw)
        self.manifest = json.loads(self.manifest_raw)

    def test_actual_source_entry_and_contract_loader_accept_frozen_successor(self):
        # No patched constants, readers, guard or validator in this positive.
        contract = self.receiver.source_entry("home_assistant")
        self.assertIs(contract, self.receiver.CONTRACT)
        loaded = load_reviewed_contract("home_assistant", {"home_assistant": contract}, private=False)
        self.assertEqual("home_assistant", loaded.manifest["adapter_id"])
        self.assertEqual(("ha_flow", "ha_client"), tuple(r["id"] for r in loaded.manifest["actors"]))
        self.assertEqual(18, len(loaded.manifest["required_cases"]))
        self.assertEqual("hub-admission-handoff-2026-10-07.json", self.receiver.HANDOFF.path.name)
        self.assertEqual("ha-admission-source-manifest-2026-10-07.json", self.receiver.SOURCE_MANIFEST.path.name)
        refresh = self.handoff["adapter"]["source_manifest_refresh"]
        self.assertEqual((7, 3), (refresh["changed_file_count"], refresh["unchanged_file_count"]))

    def assert_raw_refusal(self, path, raw, message):
        read_regular = self.receiver._read_regular

        def read(candidate, *args, **kwargs):
            return raw if candidate == path else read_regular(candidate, *args, **kwargs)

        # Byte I/O is replaced for one exact path; all authority and guards run.
        with (mock.patch.object(self.receiver, "_read_regular", side_effect=read),
              self.assertRaises(self.receiver.HomeAssistantInstalledPending) as result):
            self.receiver.source_entry("home_assistant")
        self.assertEqual(message, str(result.exception))

    def test_document_provenance_mutation_and_wrong_pin_refuse(self):
        for before, after in ((b'"changed_file_count": 7', b'"changed_file_count": 6'),
                              (b'"unchanged_file_count": 3', b'"unchanged_file_count": 2'),
                              (b'"bytes": 18984', b'"bytes": 18983')):
            with self.subTest(fact=before):
                self.assertEqual(1, self.handoff_raw.count(before))
                self.assert_raw_refusal(self.receiver.HANDOFF.path,
                                        self.handoff_raw.replace(before, after, 1),
                                        "pending: reviewed HA handoff changed")
        with self.assertRaises(self.receiver.HomeAssistantInstalledPending) as result:
            # A local reader argument tests pin refusal; production pins stay fixed.
            self.receiver._reviewed_json(self.receiver.ReviewedFile(
                self.receiver.HANDOFF.path, "0" * 64), "reviewed HA handoff")
        self.assertEqual("pending: reviewed HA handoff changed", str(result.exception))
        self.assert_raw_refusal(self.receiver.SOURCE_MANIFEST.path,
                                self.manifest_raw.replace(b'"bytes": 52149', b'"bytes": 52148', 1),
                                "pending: reviewed HA source manifest changed")

    def test_historical_documents_remain_history_and_refuse_current_authority(self):
        for kind, reviewed in (("hub-admission-handoff", self.receiver.HANDOFF),
                               ("ha-admission-source-manifest", self.receiver.SOURCE_MANIFEST)):
            historical = self.receiver.HA_ROOT / "docs" / f"{kind}-2026-09-08.json"
            raw = historical.read_bytes()
            expected = self.handoff["supersedes"]["handoff" if kind == "hub-admission-handoff" else "source_manifest"]
            self.assertEqual(expected["bytes"], len(raw))
            self.assertEqual(expected["sha256"], digest(raw))
            self.assertNotEqual(raw, reviewed.path.read_bytes())
            with self.subTest(document=kind):
                self.assert_raw_refusal(reviewed.path, raw,
                                        "pending: reviewed HA " + ("handoff" if kind == "hub-admission-handoff" else "source manifest") + " changed")

    def assert_relation_refusal(self, handoff, manifest, message):
        def parsed(reviewed, _label):
            return handoff if reviewed is self.receiver.HANDOFF else manifest

        # Decoded-document relation tests only. They deliberately do not claim
        # byte-pin/provenance proof; real constants and source guard are retained.
        with (mock.patch.object(self.receiver, "_reviewed_json", side_effect=parsed),
              self.assertRaises(self.receiver.HomeAssistantInstalledPending) as result):
            self.receiver.source_entry("home_assistant")
        self.assertEqual(message, str(result.exception))

    def test_decoded_binding_relations_refuse_one_fact_mutations(self):
        mutations = (
            lambda d: d["adapter"]["source_manifest"].update(path="/foreign/manifest.json"),
            lambda d: d["adapter"]["source_manifest"].update(sha256="0" * 64),
            lambda d: d["adapter"]["source_manifest"].update(file_count=9),
            lambda d: d["adapter"]["source_manifest_refresh"].update(current_file_sha256="0" * 64),
            lambda d: d.update(kind="foreign-handoff"),
        )
        for index, mutate in enumerate(mutations):
            document = copy.deepcopy(self.handoff)
            mutate(document)
            with self.subTest(fact=index):
                self.assert_relation_refusal(document, self.manifest,
                                            "reviewed HA handoff binding is invalid")

    def test_decoded_manifest_closure_and_row_metadata_refuse(self):
        mutations = (
            lambda d: d["files"].pop(),
            lambda d: d["files"].append(copy.deepcopy(d["files"][0])),
            lambda d: d["files"].append({"path": "teslatlas-home-assistant/foreign.py"}),
            lambda d: d.update(files=[*d["files"][:-1], copy.deepcopy(d["files"][0])]),
            lambda d: d["files"][-1].update(path="teslatlas-home-assistant/foreign.py"),
            lambda d: d["files"][0].update(mode="0600"),
            lambda d: d["files"][0].update(bytes=d["files"][0]["bytes"] + 1),
            lambda d: d["files"][0].update(sha256="0" * 64),
        )
        for index, mutate in enumerate(mutations):
            document = copy.deepcopy(self.manifest)
            mutate(document)
            with self.subTest(fact=index):
                self.assert_relation_refusal(self.handoff, document,
                    "reviewed HA source manifest is invalid" if index < 5 else "reviewed HA source manifest differs from source")

    def test_equal_length_source_substitution_refuses_before_handoff(self):
        path = self.receiver.HA_ROOT / "tools/matrix_live.py"
        raw = path.read_bytes()
        changed = bytes([raw[0] ^ 1]) + raw[1:]
        self.assertEqual(len(raw), len(changed))
        self.assert_raw_refusal(path, changed, "pending: reviewed HA source changed")

    def test_actual_current32_archive_and_copied_tree_accept_without_pin_override(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tree = root / "custom_components/teslatlas_hub"
            archive_path = root / "current32.tar.gz"
            with tarfile.open(archive_path, "w:gz") as archive:
                for row in self.receiver.COMPONENT_ROWS:
                    raw = (self.receiver.COMPONENT_ROOT / row["path"]).read_bytes()
                    path = tree / row["path"]
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(raw)
                    path.chmod(row["mode"])
                    member = tarfile.TarInfo("custom_components/teslatlas_hub/" + row["path"])
                    member.size, member.mode = len(raw), row["mode"]
                    archive.addfile(member, io.BytesIO(raw))
            archive_path.chmod(0o600)
            artifact = {"role": "home_assistant_integration_archive", "embedded_version": "2026.36.2",
                        "path": str(archive_path), "sha256": digest(archive_path.read_bytes())}
            actual, raw = self.receiver._artifact({"artifacts": [artifact]})
            self.assertEqual(artifact, actual)
            self.assertEqual(archive_path.read_bytes(), raw)
            manifest = self.receiver._installed_manifest(tree, actual)
            self.assertEqual(list(self.receiver.COMPONENT_ROWS), manifest["files"])
            self.assertEqual(32, len(manifest["files"]))


class HomeAssistantReceiverCensusTests(unittest.TestCase):
    """Exercise the real intake on approved synthetic content, never a live lane."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)
        self.names = tuple(
            row["path"] for row in home_assistant_installed.COMPONENT_ROWS
        )
        self.assertEqual(32, len(self.names))
        self.assertEqual(32, len(set(self.names)))
        self.assertIn("credentials.py", self.names)
        self.assertIn("diagnostics.py", self.names)
        self.content = {
            name: ("approved fixture " + name).encode() for name in self.names
        }
        self.rows = tuple(
            {
                "path": name,
                "bytes": len(self.content[name]),
                "mode": 0o644,
                "sha256": digest(self.content[name]),
            }
            for name in self.names
        )
        self.binding = mock.patch.object(
            home_assistant_installed, "COMPONENT_ROWS", self.rows
        )
        self.binding.start()
        self.addCleanup(self.binding.stop)

    def archive(self, names=None, *, substituted=None):
        archive_path = self.root / "candidate.tar.gz"
        with tarfile.open(archive_path, "w:gz") as output:
            for name in self.names if names is None else names:
                raw = (substituted or {}).get(name, self.content.get(name, b"foreign"))
                member = tarfile.TarInfo("custom_components/teslatlas_hub/" + name)
                member.size = len(raw)
                member.mode = 0o644
                output.addfile(member, io.BytesIO(raw))
        archive_path.chmod(0o600)
        return {
            "artifacts": [
                {
                    "role": "home_assistant_integration_archive",
                    "embedded_version": "2026.36.2",
                    "path": str(archive_path),
                    "sha256": digest(archive_path.read_bytes()),
                }
            ]
        }

    def installed_root(self, names=None, *, substituted=None):
        root = self.root / "custom_components" / "teslatlas_hub"
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        for name in self.names if names is None else names:
            path = root / name
            path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            path.write_bytes(
                (substituted or {}).get(name, self.content.get(name, b"foreign"))
            )
            path.chmod(0o644)
        return root

    def test_current32_archive_and_installed_inventory_are_admitted(self):
        job = self.archive()
        artifact, raw = home_assistant_installed._artifact(job)
        self.assertEqual(Path(artifact["path"]).read_bytes(), raw)
        manifest = home_assistant_installed._installed_manifest(
            self.installed_root(), artifact
        )
        self.assertEqual(list(self.rows), manifest["files"])
        self.assertEqual(artifact["sha256"], manifest["artifact_sha256"])

    def test_legacy31_missing_credentials_is_rejected(self):
        names = tuple(name for name in self.names if name != "credentials.py")
        self.assertEqual(31, len(names))
        with self.assertRaisesRegex(
            home_assistant_installed.HomeAssistantInstalledPending,
            "inventory is incomplete",
        ):
            home_assistant_installed._artifact(self.archive(names))
        with self.assertRaisesRegex(
            home_assistant_installed.HomeAssistantInstalledPending,
            "inventory is incomplete",
        ):
            home_assistant_installed._installed_manifest(
                self.installed_root(names), {"sha256": "a" * 64}
            )

    def test_extra_and_cache_members_are_rejected(self):
        for extra in ("extra.py", "__pycache__/credentials.cpython-314.pyc"):
            with self.subTest(extra=extra):
                names = self.names + (extra,)
                with self.assertRaisesRegex(
                    home_assistant_installed.HomeAssistantInstalledPending,
                    "inventory is foreign",
                ):
                    home_assistant_installed._artifact(self.archive(names))
                root = self.installed_root(names)
                with self.assertRaisesRegex(
                    home_assistant_installed.HomeAssistantInstalledPending,
                    "differs from reviewed component",
                ):
                    home_assistant_installed._installed_manifest(
                        root, {"sha256": "a" * 64}
                    )
                (root / extra).unlink()

    def test_duplicate_archive_member_is_rejected(self):
        with self.assertRaisesRegex(
            home_assistant_installed.HomeAssistantInstalledPending,
            "inventory is foreign",
        ):
            home_assistant_installed._artifact(
                self.archive(self.names + ("credentials.py",))
            )

    def test_substituted_credentials_content_is_rejected(self):
        changed = {"credentials.py": b"x" * len(self.content["credentials.py"])}
        with self.assertRaisesRegex(
            home_assistant_installed.HomeAssistantInstalledPending,
            "archive member changed",
        ):
            home_assistant_installed._artifact(self.archive(substituted=changed))
        with self.assertRaisesRegex(
            home_assistant_installed.HomeAssistantInstalledPending,
            "differs from reviewed component",
        ):
            home_assistant_installed._installed_manifest(
                self.installed_root(substituted=changed), {"sha256": "a" * 64}
            )

    def test_wrong_root_mode_and_symlink_are_rejected(self):
        wrong = self.root / "custom_components" / "foreign_integration"
        wrong.mkdir(mode=0o700, parents=True)
        with self.assertRaisesRegex(
            home_assistant_installed.HomeAssistantInstalledPending, "layout is invalid"
        ):
            home_assistant_installed._installed_manifest(wrong, {"sha256": "a" * 64})
        root = self.installed_root()
        credentials = root / "credentials.py"
        credentials.chmod(0o600)
        with self.assertRaisesRegex(
            home_assistant_installed.HomeAssistantInstalledPending,
            "differs from reviewed component",
        ):
            home_assistant_installed._installed_manifest(root, {"sha256": "a" * 64})
        credentials.unlink()
        credentials.symlink_to(root / "client.py")
        with self.assertRaisesRegex(
            home_assistant_installed.HomeAssistantInstalledPending, "contains a symlink"
        ):
            home_assistant_installed._installed_manifest(root, {"sha256": "a" * 64})


if __name__ == "__main__":
    unittest.main()
