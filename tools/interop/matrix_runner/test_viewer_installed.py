# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for the source-fixed Viewer installed adapter seam."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

from . import viewer_installed
from .adapter_wire import load_reviewed_contract


PROFILE = viewer_installed.WORKSPACE / "teslatlas-protocol" / "profiles" / "hub-http-v1" / "1.0.0"
SESSION_ID = "11111111-1111-4111-8111-111111111111"


def digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


class ViewerInstalledTests(unittest.TestCase):
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

    def json_file(self, relative: str, value) -> Path:
        return self.write(
            relative,
            (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode(),
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
        return certificate, digest(ssl.PEM_cert_to_DER_cert(certificate.read_text(encoding="ascii")))

    def install_archive(self, archive: Path, name: str) -> tuple[Path, Path]:
        root = self.root / "installed" / name
        root.mkdir(mode=0o700, parents=True)
        with tarfile.open(archive, "r:gz") as package:
            members = [item for item in package.getmembers() if item.isfile()]
            for item in members:
                self.assertTrue(item.name.startswith("package/"))
                relative = Path(item.name.removeprefix("package/"))
                target = root / relative
                target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                stream = package.extractfile(item)
                self.assertIsNotNone(stream)
                target.write_bytes(stream.read())
                target.chmod(item.mode & 0o777)
        inventory = viewer_installed.installed_product_inventory(
            root,
            archive,
            digest(archive.read_bytes()),
            name,
        )
        manifest = self.json_file(name + "-manifest.json", inventory)
        return root, manifest

    def inputs(self):
        sdk_archive = viewer_installed.SDK_ARCHIVE
        viewer_archive = viewer_installed.VIEWER_ARCHIVE
        sdk_root, sdk_manifest = self.install_archive(sdk_archive, "sdk")
        viewer_root, viewer_manifest = self.install_archive(viewer_archive, "viewer")
        browser = self.write("browser/chromium", b"fixed-browser", 0o500)
        environment = self.json_file(
            "environment.json",
            {
                "TESLATLAS_VIEWER_PACKAGE_ROOT": str(viewer_root),
                "TESLATLAS_VIEWER_PACKAGE_MANIFEST": str(viewer_manifest),
                "TESLATLAS_VIEWER_SDK_ROOT": str(sdk_root),
                "TESLATLAS_VIEWER_SDK_MANIFEST": str(sdk_manifest),
                "TESLATLAS_VIEWER_PAGE_ORIGIN": "https://127.0.0.1:18481",
                "TESLATLAS_VIEWER_BROWSER_EXECUTABLE": str(browser),
                "TESLATLAS_VIEWER_BROWSER_SHA256": digest(browser.read_bytes()),
                "TESLATLAS_VIEWER_BROWSER_ENGINE": "chromium",
                "TESLATLAS_VIEWER_BROWSER_VERSION": "151.0.7922.34",
            },
        )
        scenario = self.json_file(
            "scenario.json",
            {"name": "two-vehicles-five-drives", "provenance": "synthetic-only"},
        )
        session_config = self.json_file(
            "installed-session.json",
            {"run_id": "viewer-run", "scenario": self.binding(scenario)},
        )
        certificate, certificate_der_sha256 = self.certificate()
        hub = self.write("teslatlas-hub", b"fixed-hub", 0o500)
        job = {
            "adapter": "viewer", "cell_id": "viewer__macos_arm64",
            "timeout_seconds": 60, "environment_file": str(environment),
            "installed_session": {"config": self.binding(session_config)},
            "source_identities": [
                {"role": "hub_source", "repo": str(viewer_installed.WORKSPACE / "hub")},
                {"role": "protocol_source", "repo": str(viewer_installed.WORKSPACE / "teslatlas-protocol")},
                {"role": "typescript_sdk_source", "repo": str(viewer_installed.SDK_ROOT)},
                {"role": "viewer_source", "repo": str(viewer_installed.VIEWER_ROOT)},
            ],
            "artifacts": [
                {"role": "hub_executable", "name": "teslatlas-hub", "path": str(hub),
                 "embedded_version": "2026.36.2", "sha256": digest(hub.read_bytes())},
                {"role": "typescript_sdk_tarball", "name": "teslatlas-sdk-2026.36.2.tgz",
                 "path": str(sdk_archive), "embedded_version": "2026.36.2",
                 "sha256": viewer_installed.SDK_ARCHIVE_SHA256},
                {"role": "viewer_package_tarball", "name": "teslatlas-viewer-2026.36.2.tgz",
                 "path": str(viewer_archive), "embedded_version": "2026.36.2",
                 "sha256": viewer_installed.VIEWER_ARCHIVE_SHA256},
            ],
            "runtime": {"hub": {}, "client": {}, "browser_engines": [], "client_transports": []},
        }
        cell = {"id": job["cell_id"], "client_id": "viewer", "hub_target": "macos_arm64"}
        profile_root = self.root / "profile"
        shutil.copytree(PROFILE, profile_root)
        config = {
            "product_version": "2026.36.2",
            "profile": {"id": "hub-http-v1", "revision": "1.0.0",
                        "path": str(profile_root),
                        "sha256": digest((profile_root / "SHA256SUMS").read_bytes())},
        }
        descriptor = {
            "schema_version": 1, "kind": "installed-host",
            "broker_socket": str(self.broker_root / "controller.sock"),
            "session_id": SESSION_ID, "registration_sha256": "a" * 64,
        }
        running = {"descriptor": {
            "certificate_path": str(certificate),
            "endpoint": "https://127.0.0.1:18480",
        }}
        return job, cell, config, descriptor, running, certificate_der_sha256

    def test_reviewed_contract_imports_products_and_topology_are_fixed(self):
        loaded = load_reviewed_contract(
            "viewer", {"viewer": viewer_installed.CONTRACT}, private=False,
        )
        self.assertEqual("viewer", loaded.manifest["adapter_id"])
        self.assertEqual(
            ("viewer_ui", "viewer_sdk_contract"),
            tuple(item["id"] for item in loaded.manifest["actors"]),
        )
        self.assertEqual(
            (6, 81),
            (len(viewer_installed.REVIEWED_VIEWER_SOURCES), viewer_installed.SDK_MEMBER_COUNT),
        )
        self.assertEqual(
            (("macos_arm64", "local"), ("debian13_amd64", "local"),
             ("debian13_arm64", "local")),
            viewer_installed.execution_by_target(),
        )
        self.assertEqual(
            (("macos_arm64", "unix"), ("debian13_amd64", "unix"),
             ("debian13_arm64", "unix")),
            viewer_installed.broker_kind_by_target(),
        )
        self.assertEqual(
            "03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c",
            viewer_installed.SDK_ARCHIVE_SHA256,
        )

    def test_session_stages_two_exact_products_and_fixed_actor_authority(self):
        job, cell, config, descriptor, running, certificate_der_sha256 = self.inputs()
        contract = load_reviewed_contract("viewer", {"viewer": viewer_installed.CONTRACT}, private=False)
        path, session = viewer_installed.build_session_input(
            job, cell, config, {}, contract, descriptor, running,
        )
        self.assertEqual(session, json.loads(path.read_text(encoding="utf-8")))
        self.assertEqual({"kind": "unix", "socket_path": descriptor["broker_socket"]}, session["broker"])
        self.assertEqual(certificate_der_sha256, session["inputs"]["certificate_der_sha256"])
        self.assertEqual(
            ["viewer_package_tarball", "typescript_sdk_tarball"],
            [item["artifact_role"] for item in session["inputs"]["product_inputs"]],
        )
        self.assertEqual(
            ["viewer_ui", "viewer_sdk_contract"],
            [item["id"] for item in session["actors"]],
        )
        self.assertEqual(
            ["viewer_installed_playwright", "viewer_installed_playwright_sdk_dependency"],
            [item["entrypoint_ref"] for item in session["actors"]],
        )
        self.assertTrue(all(Path(value).parent == Path(session["outputs"]["normalized"]).parent
                            for value in session["outputs"].values() if not value.endswith("coordination")))
        self.assertEqual(18, len(session["inputs"]["profile_members"]))
        self.assertEqual(
            {
                "origin": "https://127.0.0.1:18481",
                "server_authority": "runner-owned-installed-viewer",
                "artifact_role": "viewer_package_tarball",
            },
            session["viewer"]["page"],
        )
        self.assertEqual(
            {
                "public_origin": "https://127.0.0.1:18480",
                "cors_allowed_origin": "https://127.0.0.1:18481",
                "cross_origin": True,
            },
            session["viewer"]["hub"],
        )
        self.assertEqual(
            session["inputs"]["certificate"],
            session["viewer"]["trusted_ca"]["certificate"],
        )
        self.assertEqual("chromium", session["viewer"]["browser"]["engine"])
        self.assertEqual(
            {"raw_evidence_dir", "browser_log", "close_record", "supplement"},
            set(session["viewer"]["reservations"]),
        )
        self.assertTrue(all(
            str(Path(value).parent).startswith(str(self.broker_root))
            for value in session["viewer"]["reservations"].values()
        ))

    def test_viewer_wire_rejects_ambient_or_unbound_runtime_authority(self):
        job, cell, config, descriptor, running, _ = self.inputs()
        contract = load_reviewed_contract("viewer", {"viewer": viewer_installed.CONTRACT}, private=False)
        environment_path = Path(job["environment_file"])
        environment = json.loads(environment_path.read_text(encoding="utf-8"))
        variants = (
            ("TESLATLAS_VIEWER_PAGE_ORIGIN", "http://127.0.0.1:18481"),
            ("TESLATLAS_VIEWER_BROWSER_ENGINE", "webkit"),
            ("TESLATLAS_VIEWER_BROWSER_SHA256", "f" * 64),
        )
        for index, (name, value) in enumerate(variants, start=2):
            with self.subTest(name=name):
                descriptor["session_id"] = f"11111111-1111-4111-8111-{index:012d}"
                changed = dict(environment, **{name: value})
                environment_path.write_text(json.dumps(changed) + "\n", encoding="utf-8")
                environment_path.chmod(0o600)
                with self.assertRaises(viewer_installed.ViewerInstalledPending):
                    viewer_installed.build_session_input(
                        job, cell, config, {}, contract, descriptor, running,
                    )
                environment_path.write_text(json.dumps(environment) + "\n", encoding="utf-8")
                environment_path.chmod(0o600)

    def test_ambient_viewer_environment_has_no_session_authority(self):
        job, cell, config, descriptor, running, _ = self.inputs()
        contract = load_reviewed_contract("viewer", {"viewer": viewer_installed.CONTRACT}, private=False)
        with mock.patch.dict(os.environ, {
            "TESLATLAS_VIEWER_PAGE_ORIGIN": "https://localhost:65535",
            "TESLATLAS_VIEWER_BROWSER_EXECUTABLE": "/tmp/foreign-browser",
            "TESLATLAS_VIEWER_BROWSER_ENGINE": "webkit",
            "TESLATLAS_VIEWER_BROWSER_VERSION": "0.0",
            "NODE_EXTRA_CA_CERTS": "/tmp/foreign-ca.pem",
        }, clear=False):
            _path, session = viewer_installed.build_session_input(
                job, cell, config, {}, contract, descriptor, running,
            )
        self.assertEqual("https://127.0.0.1:18481", session["viewer"]["page"]["origin"])
        self.assertEqual("chromium", session["viewer"]["browser"]["engine"])
        self.assertEqual(
            session["inputs"]["certificate"],
            session["viewer"]["trusted_ca"]["certificate"],
        )

    def test_job_cannot_select_executable_validator_outputs_or_broker(self):
        job, cell, config, descriptor, running, _ = self.inputs()
        contract = load_reviewed_contract("viewer", {"viewer": viewer_installed.CONTRACT}, private=False)
        for field in ("executable", "validator_path", "output_path", "broker_socket"):
            hostile = dict(job, **{field: "/tmp/foreign"})
            with self.subTest(field=field), self.assertRaisesRegex(
                viewer_installed.ViewerInstalledPending, "select Viewer adapter authority",
            ):
                viewer_installed.build_session_input(
                    hostile, cell, config, {}, contract, descriptor, running,
                )

    def test_historical_job_argv_and_output_paths_have_no_authority(self):
        job, cell, config, descriptor, running, _ = self.inputs()
        job.update({
            "argv": ["vite", "--host", "0.0.0.0"],
            "cwd": "/tmp/foreign-source",
            "evidence_path": "/tmp/foreign-evidence.json",
        })
        contract = load_reviewed_contract("viewer", {"viewer": viewer_installed.CONTRACT}, private=False)
        _path, session = viewer_installed.build_session_input(
            job, cell, config, {}, contract, descriptor, running,
        )
        self.assertNotIn("/tmp/foreign-evidence.json", session["outputs"].values())
        self.assertTrue(all(str(Path(value).parent).startswith(str(self.broker_root))
                            for value in session["outputs"].values()))

    def test_product_pins_reject_old_sdk_and_changed_installed_member(self):
        job, _cell, _config, _descriptor, _running, _ = self.inputs()
        old = [dict(item) for item in job["artifacts"]]
        next(item for item in old if item["role"] == "typescript_sdk_tarball")["sha256"] = "4" * 64
        with self.assertRaisesRegex(viewer_installed.ViewerInstalledPending, "reviewed product inventory"):
            viewer_installed.validate_job_inventory(dict(job, artifacts=old))

        viewer_artifact = next(item for item in job["artifacts"] if item["role"] == "viewer_package_tarball")
        environment = json.loads(Path(job["environment_file"]).read_text(encoding="utf-8"))
        root = Path(environment["TESLATLAS_VIEWER_PACKAGE_ROOT"])
        manifest = json.loads(Path(environment["TESLATLAS_VIEWER_PACKAGE_MANIFEST"]).read_text(encoding="utf-8"))
        (root / "dist/index.html").write_bytes(b"substitute")
        with self.assertRaisesRegex(viewer_installed.ViewerInstalledPending, "installed product"):
            viewer_installed.validate_installed_product(root, viewer_artifact, manifest)

    def test_launch_stays_closed_until_coordinator_consumes_viewer_wire(self):
        session_path = self.write("session.json", b"{}\n")
        session = {"outputs": {
            "normalized": str(self.root / "normalized.json"),
            "actor_evidence": str(self.root / "actor-evidence.json"),
            "coordination_dir": str(self.root / "coordination"),
            "framework_log": str(self.root / "framework.json"),
        }}
        with mock.patch.object(viewer_installed.subprocess, "Popen") as popen:
            with self.assertRaisesRegex(
                viewer_installed.ViewerInstalledPending,
                "consume the staged Viewer wire",
            ):
                viewer_installed.launch_adapter({}, {}, {}, None, session_path, session)
        popen.assert_not_called()

    def test_admission_rejects_current_semantic_placeholders(self):
        contract = load_reviewed_contract("viewer", {"viewer": viewer_installed.CONTRACT}, private=False)
        normalized = {
            "cases": [
                {"id": case_id, "status": "pending", "expected": {"evidence": "not-run"},
                 "actual": {"evidence": "not-run"}, "evidence_kind": "http",
                 "request_transcript": []}
                for case_id in contract.manifest["required_cases"]
            ],
        }
        actors = {"actors": [], "invocations": []}
        with self.assertRaisesRegex(
            viewer_installed.ViewerInstalledPending,
            "semantic evidence is pending",
        ):
            viewer_installed.admit(
                {}, {"id": "viewer__macos_arm64", "client_id": "viewer"}, {}, {},
                contract, normalized, actors,
            )


if __name__ == "__main__":
    unittest.main()
