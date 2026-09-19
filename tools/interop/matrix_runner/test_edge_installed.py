# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for the source-fixed Edge installed adapter seam."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import ssl
import subprocess
import tempfile
import unittest
from unittest import mock

from . import edge_installed
from .adapter_wire import load_reviewed_contract

SESSION_ID = "11111111-1111-4111-8111-111111111111"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class EdgeInstalledTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)

    def write_json(self, name: str, value: object) -> dict[str, str]:
        path = self.root / name
        path.write_text(
            json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )
        path.chmod(0o600)
        return {"path": str(path), "sha256": digest(path)}

    def fixture(self) -> dict[str, object]:
        placeholder = self.write_json("bound.json", {"fixed": True})
        members = [
            {"path": str(edge_installed.EDGE_PROFILE_ROOT / relative),
             "sha256": digest(edge_installed.EDGE_PROFILE_ROOT / relative)}
            for relative in edge_installed.EDGE_PROFILE_MEMBERS
        ]
        credentials = self.write_json(
            "credentials.json",
            {
                "schema_version": 1,
                "session_id": SESSION_ID,
                "cell_id": "edge_v2__debian13_amd64",
                "instance_nonce": "a" * 64,
                "files": [],
            },
        )
        hub_roots = {
            "primary": self.root / "hubs" / "edge_v2__debian13_amd64",
            "reference": self.root / "hubs" / "edge_v2__debian13_amd64-edge-reference",
            "fault": self.root / "hubs" / "edge_v2__debian13_amd64-edge-fault",
            "negative": self.root / "hubs" / "edge_v2__debian13_amd64-edge-negative",
        }
        return {
            "schema_version": 1,
            "kind": "edge-installed-fixture",
            "run_id": "edge-run",
            "cell_id": "edge_v2__debian13_amd64",
            "session_id": SESSION_ID,
            "instance_nonce": "a" * 64,
            "host_registration": placeholder,
            "recipe": placeholder,
            "vectors": {
                "path": str(edge_installed.VECTORS_PATH),
                "sha256": edge_installed.REVIEWED_SOURCES[
                    "tools/interop/client_lanes/edge_contract/edge-installed-vectors.json"
                ],
            },
            "edge_profile": {
                "manifest": {
                    "path": str(edge_installed.EDGE_PROFILE_ROOT / "SHA256SUMS"),
                    "sha256": edge_installed.EDGE_PROFILE_SHA256,
                },
                "members": members,
            },
            "producer_registration": placeholder,
            "launch_inventory": placeholder,
            "lease": placeholder,
            "private_root": str(self.root / "private"),
            "lanes": [
                {
                    "id": lane_id,
                    "hub_role": hub_role,
                    "hub_root": str(hub_roots[lane_id]),
                    "producer_root": str(self.root / ("producer-" + lane_id)),
                    "hub_port": hub_port,
                    "receiver_port": receiver_port,
                    "delivery_port": delivery_port,
                }
                for lane_id, hub_role, hub_port, receiver_port, delivery_port in edge_installed.LANES
            ],
            "credentials": credentials,
        }

    def certificate(self) -> Path:
        config = self.root / "openssl.cnf"
        config.write_text(
            "[req]\nprompt=no\ndistinguished_name=dn\n[v3]\n"
            "basicConstraints=critical,CA:TRUE\n"
            "keyUsage=critical,digitalSignature,keyCertSign\n"
            "subjectAltName=IP:127.0.0.1\n[dn]\nCN=127.0.0.1\n",
            encoding="ascii",
        )
        config.chmod(0o600)
        key = self.root / "key.pem"
        certificate = self.root / "certificate.pem"
        result = subprocess.run(
            [
                "openssl", "req", "-x509", "-nodes", "-newkey", "rsa:2048",
                "-keyout", str(key), "-out", str(certificate), "-days", "1",
                "-config", str(config),
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        key.chmod(0o600)
        certificate.chmod(0o600)
        return certificate

    def build_inputs(self):
        fixture_binding = self.write_json("edge-fixture.json", self.fixture())
        scenario = self.write_json(
            "scenario.json", {"name": "two-vehicles-five-drives", "provenance": "synthetic-only"}
        )
        session_config = self.write_json(
            "installed-session.json",
            {"schema_version": 2, "run_id": "edge-run", "edge_fixture": fixture_binding,
             "scenario": scenario},
        )
        hub = self.root / "teslatlas-hub"
        edge = self.root / "teslatlas-edge"
        hub.write_bytes(b"fixed-hub")
        edge.write_bytes(b"fixed-edge")
        hub.chmod(0o500)
        edge.chmod(0o500)
        sources = []
        for role, repo in (
            ("hub_source", edge_installed.WORKSPACE / "hub"),
            ("protocol_source", edge_installed.PROTOCOL_ROOT),
            ("edge_source", edge_installed.EDGE_ROOT),
        ):
            sources.append(
                {"role": role, "repo": str(repo), "head": "a" * 40,
                 "dirty_patch_sha256": "b" * 64,
                 "untracked_source_manifest_sha256": "c" * 64}
            )
        job = {
            "adapter": "edge_v2", "cell_id": "edge_v2__debian13_amd64",
            "timeout_seconds": 60,
            "installed_session": {"config": session_config},
            "source_identities": sources,
            "artifacts": [
                {"role": "hub_executable", "name": "teslatlas-hub",
                 "path": str(hub), "embedded_version": "2026.36.2",
                 "sha256": digest(hub)},
                {"role": "edge_executable", "name": "teslatlas-edge",
                 "path": str(edge), "embedded_version": "2026.36.2",
                 "sha256": digest(edge)},
            ],
            "runtime": {
                "hub": {"os": "Debian 13", "architecture": "amd64",
                        "native_or_emulated": "emulated",
                        "service_mode": "installed-deb-systemd",
                        "tool_versions": {"hub": "2026.36.2"}},
                "client": {"os": "macOS", "architecture": "arm64",
                           "native_or_emulated": "native",
                           "service_mode": "edge-root-coordinator",
                           "tool_versions": {"python": "3.14"}},
                "browser_engines": [], "client_transports": ["unix", "raw-tls"],
            },
            # Required historical runner fields are deliberately hostile. They
            # have no authority over this source-owned coordinator.
            "argv": ["/bin/sh", "-c", "exit 99"],
            "cwd": "/tmp/foreign",
            "evidence_path": "/tmp/foreign.json",
        }
        cell = {"id": job["cell_id"], "client_id": "edge_v2",
                "hub_target": "debian13_amd64"}
        profile = edge_installed.PROTOCOL_ROOT / "profiles" / "hub-http-v1" / "1.0.0"
        config = {
            "product_version": "2026.36.2",
            "profile": {"id": "hub-http-v1", "revision": "1.0.0",
                        "path": str(profile),
                        "sha256": digest(profile / "SHA256SUMS")},
        }
        certificate = self.certificate()
        broker = self.root / "broker"
        broker.mkdir(mode=0o700)
        descriptor = {
            "schema_version": 1, "kind": "installed-host",
            "broker_socket": str(broker / "controller.sock"),
            "session_id": SESSION_ID, "registration_sha256": "d" * 64,
        }
        running = {"descriptor": {"certificate_path": str(certificate)}}
        return job, cell, config, descriptor, running

    def test_reviewed_contract_imports_and_source_hashes_are_exact(self):
        contract = load_reviewed_contract(
            "edge_v2", {"edge_v2": edge_installed.CONTRACT}, private=False
        )
        self.assertEqual(contract.manifest["adapter_id"], "edge_v2")
        self.assertEqual(len(contract.manifest["phases"]), 31)
        self.assertEqual(tuple(contract.module.ACTOR_IDS), edge_installed.ACTOR_IDS)
        self.assertEqual(
            tuple(
                (row["actor_id"], row["phase_id"], row["ordinal"], row["recipe_id"])
                for row in contract.manifest["phases"]
            ),
            edge_installed.PHASES,
        )
        for relative, expected in edge_installed.REVIEWED_SOURCES.items():
            self.assertEqual(digest(edge_installed.EDGE_ROOT / relative), expected)
        for relative, expected in edge_installed.REVIEW_HANDOFFS.items():
            self.assertEqual(digest(edge_installed.EDGE_ROOT / relative), expected)

    def test_target_actor_and_broker_topology_are_source_fixed(self):
        self.assertEqual(
            edge_installed.execution_by_target(),
            (
                ("macos_arm64", "local"),
                ("debian13_amd64", "local"),
                ("debian13_arm64", "local"),
            ),
        )
        self.assertEqual(
            edge_installed.broker_kind_by_target(),
            (
                ("macos_arm64", "unix"),
                ("debian13_amd64", "unix"),
                ("debian13_arm64", "unix"),
            ),
        )
        self.assertEqual(
            edge_installed.ACTOR_IDS,
            (
                "installed_controller",
                "edge_normal",
                "edge_producer",
                "edge_reference",
                "edge_fault",
                "edge_transport",
            ),
        )
        self.assertEqual(len(edge_installed.PHASES), 31)
        self.assertEqual([item[2] for item in edge_installed.PHASES], list(range(1, 32)))

    def test_fixture_input_is_closed_and_fixed_to_four_lane_topology(self):
        fixture = self.fixture()
        edge_installed.validate_fixture_input(
            fixture,
            cell_id="edge_v2__debian13_amd64",
            session_id=SESSION_ID,
        )
        fixture["argv"] = ["/bin/sh"]
        with self.assertRaisesRegex(edge_installed.EdgeInstalledPending, "shape"):
            edge_installed.validate_fixture_input(
                fixture,
                cell_id="edge_v2__debian13_amd64",
                session_id=SESSION_ID,
            )

    def test_fixture_rejects_changed_lane_and_incomplete_edge_profile(self):
        fixture = self.fixture()
        fixture["lanes"][0]["delivery_port"] = 19999
        with self.assertRaisesRegex(edge_installed.EdgeInstalledPending, "lane"):
            edge_installed.validate_fixture_input(
                fixture,
                cell_id="edge_v2__debian13_amd64",
                session_id=SESSION_ID,
            )
        fixture = self.fixture()
        fixture["edge_profile"]["members"].pop()
        with self.assertRaisesRegex(edge_installed.EdgeInstalledPending, "profile"):
            edge_installed.validate_fixture_input(
                fixture,
                cell_id="edge_v2__debian13_amd64",
                session_id=SESSION_ID,
            )
        fixture = self.fixture()
        fixture["lanes"][1]["hub_root"] = str(self.root / "foreign-reference")
        with self.assertRaisesRegex(edge_installed.EdgeInstalledPending, "lane"):
            edge_installed.validate_fixture_input(
                fixture, cell_id="edge_v2__debian13_amd64", session_id=SESSION_ID
            )

    def test_job_authority_and_product_source_inventory_are_closed(self):
        job = {
            "adapter": "edge_v2",
            "cell_id": "edge_v2__debian13_amd64",
            "source_identities": [
                {"role": role, "repo": str(root), "head": "a" * 40,
                 "dirty_patch_sha256": "b" * 64,
                 "untracked_source_manifest_sha256": "c" * 64}
                for role, root in (
                    ("hub_source", edge_installed.WORKSPACE / "hub"),
                    ("protocol_source", edge_installed.PROTOCOL_ROOT),
                    ("edge_source", edge_installed.EDGE_ROOT),
                )
            ],
            "artifacts": [
                {"role": "hub_executable", "name": "teslatlas-hub",
                 "path": str(self.root / "teslatlas-hub"), "embedded_version": "2026.36.2",
                 "sha256": "d" * 64},
                {"role": "edge_executable", "name": "teslatlas-edge",
                 "path": str(self.root / "teslatlas-edge"), "embedded_version": "2026.36.2",
                 "sha256": "e" * 64},
            ],
        }
        edge_installed.validate_job_inventory(job, "2026.36.2")
        job["broker_socket"] = "/tmp/foreign.sock"
        with self.assertRaisesRegex(edge_installed.EdgeInstalledPending, "authority"):
            edge_installed.validate_job_inventory(job, "2026.36.2")
        del job["broker_socket"]
        job["source_identities"][0]["branch"] = "main"
        with self.assertRaisesRegex(edge_installed.EdgeInstalledPending, "source"):
            edge_installed.validate_job_inventory(job, "2026.36.2")

    def test_session_input_stages_fixed_contract_product_and_actor_order(self):
        job, cell, config, descriptor, running = self.build_inputs()
        contract = load_reviewed_contract(
            "edge_v2", {"edge_v2": edge_installed.CONTRACT}, private=False
        )
        path, session = edge_installed.build_session_input(
            job, cell, config, {}, contract, descriptor, running
        )
        self.assertEqual(json.loads(path.read_text()), session)
        self.assertEqual(session["broker"], {
            "kind": "unix", "socket_path": descriptor["broker_socket"]
        })
        self.assertEqual(tuple(actor["id"] for actor in session["actors"]),
                         edge_installed.ACTOR_IDS)
        self.assertEqual(
            [actor["phase_contract"] is not None for actor in session["actors"]],
            [False, True, True, True, True, True],
        )
        product = session["inputs"]["product_inputs"]
        self.assertEqual([item["artifact_role"] for item in product], ["edge_executable"])
        self.assertEqual(product[0]["staged"]["local"]["sha256"],
                         job["artifacts"][1]["sha256"])
        self.assertNotIn("argv", session)
        self.assertNotIn("edge_fixture", session["inputs"])
        source_manifest = json.loads(
            Path(session["actors"][1]["input_manifest"]["local"]["path"]).read_text()
        )
        paths = [item["path"] for item in source_manifest["files"]]
        self.assertEqual(paths, sorted(set(paths)))
        self.assertIn("edge_profile/SHA256SUMS", paths)
        self.assertEqual(
            len([item for item in paths if item.startswith("edge_profile/")]), 20
        )
        self.assertIn("edge_contract/edge-installed-vectors.json", paths)

    def test_launch_uses_only_staged_reviewed_coordinator(self):
        job, cell, config, descriptor, running = self.build_inputs()
        contract = load_reviewed_contract(
            "edge_v2", {"edge_v2": edge_installed.CONTRACT}, private=False
        )
        path, session = edge_installed.build_session_input(
            job, cell, config, {}, contract, descriptor, running
        )
        sentinel = object()
        with mock.patch.object(edge_installed.subprocess, "Popen", return_value=sentinel) as popen:
            result = edge_installed.launch_adapter(
                job, cell, config, contract, path, session
            )
        self.assertIs(result, sentinel)
        argv = popen.call_args.args[0]
        self.assertEqual(Path(argv[0]).resolve(), Path(edge_installed.sys.executable).resolve())
        self.assertEqual(argv[1], str(
            path.parent / "source" / "tools" / "interop" / "client_lanes" / "edge.py"
        ))
        self.assertEqual(argv[2], str(path))
        self.assertNotIn("/bin/sh", argv)
        self.assertTrue(popen.call_args.kwargs["start_new_session"])

    def test_runtime_admission_and_receipt_hooks_fail_closed_without_root_evidence(self):
        calls = (
            (edge_installed.runtime_inventory, ({}, {}, {}, None, {})),
            (edge_installed.admit, ({}, {}, {}, {}, None, {}, {})),
            (edge_installed.build_supplement, ({}, {}, {}, {}, None, None)),
            (edge_installed.execution_logs, ({}, {}, {}, None, None)),
        )
        for callback, arguments in calls:
            with self.subTest(callback=callback.__name__):
                with self.assertRaisesRegex(
                    edge_installed.EdgeInstalledPending, "root-owned Edge runtime evidence"
                ):
                    callback(*arguments)


if __name__ == "__main__":
    unittest.main()
