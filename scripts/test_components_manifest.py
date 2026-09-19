#!/usr/bin/env python3
"""Fail closed on the D1 aggregate component selection manifest."""

from __future__ import annotations

import json
import hashlib
from pathlib import Path
import stat
import tarfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "packaging" / "components.json"


class ComponentsManifestTests(unittest.TestCase):
    def test_records_all_d1_component_slots_without_promoting_unbound_payloads(self) -> None:
        document = json.loads(MANIFEST.read_text(encoding="utf-8"))

        self.assertEqual(document["schema_version"], 1)
        self.assertEqual(document["product_version"], "2026.36.2")
        self.assertEqual(
            [component["id"] for component in document["components"]],
            [
                "hub-core",
                "fleet-helpers",
                "protocol",
                "sdk-typescript",
                "sdk-swift",
                "viewer",
                "home-assistant",
                "edge",
            ],
        )
        self.assertTrue(document["selection_receipt"]["runtime_receipt_required"])

        home_assistant = next(
            component
            for component in document["components"]
            if component["id"] == "home-assistant"
        )
        self.assertEqual(
            home_assistant["payload"]["handoff_manifest_sha256"],
            "73d702a85e0d79116c6a82f936080a2e393ac0b92e3ab0afa86d38f405a90940",
        )
        self.assertEqual(
            home_assistant["source"]["head"],
            "f650331a1af0cf33cec271bc7eefc0f1201ebd4e",
        )
        self.assertEqual(
            home_assistant["selectors"][0]["id"], "debian13-arm64-container"
        )
        self.assertEqual(
            home_assistant["status"],
            "accepted_primary_runtime_lane_only",
        )
        for component in document["components"]:
            if component["id"] == "home-assistant":
                continue
            if component["id"] == "viewer":
                continue
            if component["id"] == "sdk-typescript":
                continue
            if component["id"] == "sdk-swift":
                continue
            if component["id"] == "edge":
                continue
            if component["id"] == "protocol":
                continue
            self.assertTrue(component["status"].startswith("blocked_"))
            self.assertTrue(component["blocked_reason"])

        typescript = next(
            component
            for component in document["components"]
            if component["id"] == "sdk-typescript"
        )
        self.assertEqual(
            typescript["status"], "prepared_optional_developer_resource_review"
        )
        self.assertEqual(
            typescript["source"]["head"], "b1cd548fef7ddd26be2637c14bb435480166cef7"
        )
        self.assertEqual(
            typescript["package_archive"]["member_count"], 81
        )

        swift = next(
            component
            for component in document["components"]
            if component["id"] == "sdk-swift"
        )
        self.assertEqual(
            swift["status"], "prepared_optional_swiftpm_source_review"
        )
        self.assertEqual(
            swift["source"]["head"], "51494612db6aa3d0b0dafd08001ce432befa7fbf"
        )
        self.assertEqual(swift["swiftpm"]["product_count"], 5)

        edge = next(
            component
            for component in document["components"]
            if component["id"] == "edge"
        )
        self.assertEqual(
            edge["status"], "prepared_source_candidate_review_runtime_limited"
        )
        self.assertEqual(edge["source"]["handoff_file_count"], 87)
        self.assertEqual(edge["selectors"][0]["id"], "debian13-arm64-native")

        protocol = next(
            component
            for component in document["components"]
            if component["id"] == "protocol"
        )
        self.assertEqual(
            protocol["status"], "prepared_optional_protocol_source_review"
        )
        self.assertEqual(protocol["source"]["handoff_file_count"], 177)
        self.assertEqual(protocol["developer_resource"]["version"], "2026.36.2")

        viewer = next(
            component
            for component in document["components"]
            if component["id"] == "viewer"
        )
        self.assertEqual(viewer["status"], "prepared_for_aggregate_review")
        self.assertEqual(viewer["source"]["handoff_file_count"], 106)
        self.assertEqual(
            viewer["source"]["handoff_manifest_sha256"],
            "b57d3e5b7c015bf397b55ab5823afe33d20c0b2296d0892f39c6bd683d6bae7a",
        )
        self.assertEqual(
            viewer["verified_supporting_identities"]["dockerfile_sha256"],
            "d1f9509a4d4d2efc15afa32f8fcd82950bd67dced732f2429ade35cae9f94a51",
        )
        self.assertEqual(
            viewer["verified_supporting_identities"]["compose_sha256"],
            "2298aed0aa44cadcf775381df8d0679ecf2e143f7ae5557dff0d03f70b8be4bf",
        )
        self.assertTrue(viewer["package_archive_present"])

    def test_keeps_ha_arm64_selection_explicitly_historical_and_narrow(self) -> None:
        document = json.loads(MANIFEST.read_text(encoding="utf-8"))
        home_assistant = next(
            component
            for component in document["components"]
            if component["id"] == "home-assistant"
        )
        self.assertEqual(
            home_assistant["selection_receipt"]["selector_id"],
            "debian13-arm64-container",
        )
        self.assertIn("not a standalone package", home_assistant["selection_receipt"]["scope"])
        self.assertFalse(home_assistant["payload"]["standalone_package"])
        self.assertEqual(
            home_assistant["selectors"][0]["selection_receipt"],
            home_assistant["selection_receipt"]["path"],
        )

    def test_verifies_the_external_frozen_typescript_snapshot_and_retained_package(self) -> None:
        document = json.loads(MANIFEST.read_text(encoding="utf-8"))
        typescript = next(
            component
            for component in document["components"]
            if component["id"] == "sdk-typescript"
        )
        import sys

        sys.path.insert(0, str(ROOT / "tools"))
        from companions.core import KNOWN_REPOSITORIES, source_manifest_for

        source = Path(typescript["source"]["frozen_snapshot"]["path"])
        record_path = Path(typescript["source"]["frozen_snapshot"]["manifest_path"])
        record = json.loads(record_path.read_text(encoding="utf-8"))["components"]["sdk-typescript"]
        observed = source_manifest_for(
            "sdk-typescript", source, KNOWN_REPOSITORIES["sdk-typescript"], record["commit"]
        )
        # The companion record names its source checkout. A frozen copy is valid
        # only after that path is explicitly rebound to the owner-only snapshot.
        expected = {**record, "path": str(source)}
        self.assertEqual(expected, observed)
        self.assertEqual(len(observed["files"]), 254)
        self.assertEqual(
            typescript["source"]["handoff_manifest_sha256"],
            hashlib.sha256(record_path.read_bytes()).hexdigest(),
        )
        self.assertEqual(
            typescript["source"]["handoff_manifest_sha256"],
            "4264d380a1d4f8e9ae8bb5d1b76c99bcbf4da89eb89906f79f40c10929d80067",
        )
        self.assertEqual(
            typescript["source"]["handoff_source_sha256"], observed["source_sha256"]
        )
        self.assertEqual(
            typescript["source"]["profile_id"], "hub-http-v1@1.0.0"
        )
        self.assertEqual(
            typescript["source"]["profile_sha256"],
            "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926",
        )
        for path in [source, *source.rglob("*")]:
            self.assertFalse(stat.S_IMODE(path.stat().st_mode) & 0o222, path)

        archive = Path(typescript["package_archive"]["path"])
        self.assertEqual(
            typescript["package_archive"]["sha256"],
            hashlib.sha256(archive.read_bytes()).hexdigest(),
        )
        with tarfile.open(archive) as package:
            self.assertEqual(len(package.getmembers()), 81)
            self.assertEqual(
                hashlib.sha256(package.extractfile("package/dist/node.js").read()).hexdigest(),
                typescript["package_archive"]["node_entry_sha256"],
            )
            self.assertEqual(
                hashlib.sha256(package.extractfile("package/dist/browser.js").read()).hexdigest(),
                typescript["package_archive"]["browser_entry_sha256"],
            )
        self.assertEqual(
            typescript["status"], "prepared_optional_developer_resource_review"
        )
        self.assertTrue(
            all(selector["mode"] == "developer_resource" for selector in typescript["selectors"][:-1])
        )
        self.assertEqual(typescript["selectors"][-1]["status"], "blocked_not_a_service")

    def test_verifies_the_external_frozen_swift_snapshot_twice_before_binding_source_only(self) -> None:
        document = json.loads(MANIFEST.read_text(encoding="utf-8"))
        swift = next(
            component
            for component in document["components"]
            if component["id"] == "sdk-swift"
        )
        import sys

        sys.path.insert(0, str(ROOT / "tools"))
        from companions.core import KNOWN_REPOSITORIES, source_manifest_for

        source = Path(swift["source"]["frozen_snapshot"]["path"])
        record_path = Path(swift["source"]["frozen_snapshot"]["manifest_path"])
        record = json.loads(record_path.read_text(encoding="utf-8"))["components"]["sdk-swift"]
        for _ in range(2):
            observed = source_manifest_for(
                "sdk-swift", source, KNOWN_REPOSITORIES["sdk-swift"], record["commit"]
            )
            self.assertEqual(record, observed)
        self.assertEqual(len(record["files"]), 158)
        self.assertEqual(
            swift["source"]["handoff_manifest_sha256"],
            hashlib.sha256(record_path.read_bytes()).hexdigest(),
        )
        self.assertEqual(
            swift["source"]["handoff_source_sha256"], record["source_sha256"]
        )
        for path in [source, *source.rglob("*")]:
            self.assertFalse(stat.S_IMODE(path.stat().st_mode) & 0o222, path)
        package = source / "Package.swift"
        version = source / "VERSION"
        self.assertEqual(
            swift["swiftpm"]["package_swift_sha256"],
            hashlib.sha256(package.read_bytes()).hexdigest(),
        )
        self.assertEqual(swift["swiftpm"]["version"], version.read_text(encoding="utf-8").strip())
        self.assertEqual(swift["swiftpm"]["product_count"], 5)
        self.assertEqual(swift["selectors"][0]["id"], "swiftpm-source")
        self.assertEqual(swift["selectors"][0]["status"], "supported_source_candidate_not_installed")
        self.assertTrue(
            all(selector["status"].startswith("blocked_") for selector in swift["selectors"][1:4])
        )
        self.assertEqual(swift["selectors"][-1]["status"], "unsupported_by_swift_sdk_contract")

    def test_verifies_the_external_frozen_edge_snapshot_twice_without_promoting_runtime_history(self) -> None:
        document = json.loads(MANIFEST.read_text(encoding="utf-8"))
        edge = next(
            component
            for component in document["components"]
            if component["id"] == "edge"
        )
        import sys

        sys.path.insert(0, str(ROOT / "tools"))
        from companions.core import KNOWN_REPOSITORIES, source_manifest_for

        source = Path(edge["source"]["frozen_snapshot"]["path"])
        record_path = Path(edge["source"]["frozen_snapshot"]["manifest_path"])
        record = json.loads(record_path.read_text(encoding="utf-8"))["components"]["edge"]
        for _ in range(2):
            observed = source_manifest_for(
                "edge", source, KNOWN_REPOSITORIES["edge"], record["commit"]
            )
            self.assertEqual(record, observed)
        self.assertEqual(len(record["files"]), 87)
        self.assertEqual(
            edge["source"]["handoff_manifest_sha256"],
            hashlib.sha256(record_path.read_bytes()).hexdigest(),
        )
        self.assertEqual(edge["source"]["handoff_source_sha256"], record["source_sha256"])
        for path in [source, *source.rglob("*")]:
            self.assertFalse(stat.S_IMODE(path.stat().st_mode) & 0o222, path)
        self.assertEqual(
            edge["source"]["profile_id"], "edge-delivery-v2@2.0.0"
        )
        self.assertEqual(
            edge["supporting_source_identities"]["Dockerfile"],
            "d0b3b47f04afdf049ad2bd0b06e134401d73031e6224c9759282615069a4632a",
        )
        self.assertEqual(
            edge["selectors"][0]["status"], "runtime_subset_passed_not_final_acceptance"
        )
        self.assertEqual(
            edge["selectors"][1]["status"], "runtime_shape_passed_no_durable_hub_ack"
        )
        self.assertEqual(edge["selectors"][3]["status"], "blocked_no_fresh_authorized_target")
        self.assertEqual(edge["selectors"][4]["status"], "source_candidate_runtime_pending")
        self.assertTrue(edge["selection_receipt"]["runtime_receipt_required"])
        self.assertIn("never", edge["selection_limit"].lower())

    def test_verifies_the_external_frozen_protocol_snapshot_twice_before_binding_source_only(self) -> None:
        document = json.loads(MANIFEST.read_text(encoding="utf-8"))
        protocol = next(
            component
            for component in document["components"]
            if component["id"] == "protocol"
        )
        import sys

        sys.path.insert(0, str(ROOT / "tools"))
        from companions.core import KNOWN_REPOSITORIES, source_manifest_for

        self.assertIn("source", protocol)
        source = Path(protocol["source"]["frozen_snapshot"]["path"])
        record_path = Path(protocol["source"]["frozen_snapshot"]["manifest_path"])
        record = json.loads(record_path.read_text(encoding="utf-8"))["components"]["protocol"]
        for _ in range(2):
            observed = source_manifest_for(
                "protocol", source, KNOWN_REPOSITORIES["protocol"], record["commit"]
            )
            self.assertEqual(record, observed)
        self.assertEqual(len(record["files"]), 177)
        self.assertEqual(
            protocol["source"]["handoff_manifest_sha256"],
            hashlib.sha256(record_path.read_bytes()).hexdigest(),
        )
        self.assertEqual(protocol["source"]["handoff_source_sha256"], record["source_sha256"])
        for path in [source, *source.rglob("*")]:
            self.assertFalse(stat.S_IMODE(path.stat().st_mode) & 0o222, path)
        self.assertEqual(protocol["source"]["profile_id"], "hub-http-v1@1.0.0")
        self.assertEqual(
            protocol["developer_resource"]["compatibility_hub_sha256"],
            "db8ac0037f8b4929897c1b06718b70e0d5c0384d70d0f24ce8c57f2009f8ed21",
        )
        self.assertEqual(
            protocol["developer_resource"]["checker_sha256"],
            "22cff4d2e7f58b7e394869c8498a73ab7b925b9f821b2d7ea4a6ea5d444266af",
        )
        self.assertEqual(protocol["selectors"][0]["status"], "supported_source_candidate_not_installed")
        self.assertEqual(protocol["selectors"][1]["status"], "source_candidate_test_environment_not_runtime")
        self.assertEqual(protocol["selectors"][-1]["status"], "unsupported_by_protocol_contract")


if __name__ == "__main__":
    unittest.main()
