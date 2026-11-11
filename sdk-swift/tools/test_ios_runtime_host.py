"""Source-only wiring smoke checks for the SDK-owned iOS XCTest host.

Import markers, project declarations and target listings do not demonstrate
execution or behaviour of a public SDK product.
"""

from __future__ import annotations

import shutil
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
HOST = ROOT / "iOSRuntimeHost"
SPEC = HOST / "project.yml"
PROJECT = HOST / "CurrentHubRuntimeHost.xcodeproj"
SCHEME = PROJECT / "xcshareddata/xcschemes/CurrentHubRuntimeHost.xcscheme"
PRODUCTS = (
    "TeslatlasHubSDK",
    "TeslatlasCommands",
    "TeslatlasHubV1Compatibility",
    "TeslatlasCurrentHub",
)


class IOSRuntimeHostTests(unittest.TestCase):
    def test_included_support_hash_dependency_is_in_sdk_source(self):
        """The small host must not depend on an excluded SwiftPM test file."""
        support = "CurrentHubTestSupport.swift"
        for declaration in (SPEC, PROJECT / "project.pbxproj"):
            contents = declaration.read_text(encoding="utf-8")
            self.assertIn(support, contents)
            self.assertNotIn("CurrentHubMatrixWorkerTests.swift", contents)
        contents = (ROOT / "Tests/TeslatlasCurrentHubTests" / support).read_text()
        self.assertNotIn("MatrixSHA256", contents)
        self.assertIn("CurrentHubSHA256.hexDigest(of:", contents)
        sdk = (ROOT / "Sources/TeslatlasCurrentHub/CurrentHubSHA256.swift").read_text()
        self.assertIn("enum CurrentHubSHA256", sdk)
        self.assertIn("static func hexDigest(of data: Data)", sdk)

    def test_host_spec_declares_app_test_target_and_local_package(self):
        self.assertTrue(SPEC.is_file(), f"missing iOS host spec: {SPEC}")
        contents = SPEC.read_text(encoding="utf-8")
        for marker in (
            "CurrentHubRuntimeHost:",
            "CurrentHubRuntimeTests:",
            "type: application",
            "type: bundle.unit-test",
            "path: ..",
            'iOS: "17.0"',
            "schemes:",
        ):
            self.assertIn(marker, contents)
        for product in PRODUCTS:
            self.assertEqual(2, contents.count(f"product: {product}"))

    def test_shared_scheme_builds_host_and_runs_tests(self):
        self.assertTrue(SCHEME.is_file(), f"missing shared iOS host scheme: {SCHEME}")
        contents = SCHEME.read_text(encoding="utf-8")
        self.assertIn("CurrentHubRuntimeHost.app", contents)
        self.assertIn("CurrentHubRuntimeTests.xctest", contents)
        self.assertIn("TestAction", contents)

    def test_host_probe_contains_public_product_import_markers(self):
        contents = (HOST / "Sources/PlatformSurfaceProbe.swift").read_text(
            encoding="utf-8"
        )
        for product in PRODUCTS:
            self.assertIn(f"import {product}", contents)

    @unittest.skipUnless(shutil.which("xcodebuild"), "xcodebuild is required on the Apple host")
    def test_generated_host_lists_app_and_test_targets(self):
        self.assertTrue(PROJECT.is_dir(), f"missing generated iOS host: {PROJECT}")
        result = subprocess.run(
            ["xcodebuild", "-project", str(PROJECT), "-list"],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("CurrentHubRuntimeHost", result.stdout)
        self.assertIn("CurrentHubRuntimeTests", result.stdout)


if __name__ == "__main__":
    unittest.main()
