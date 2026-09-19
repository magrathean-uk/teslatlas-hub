"""Contract checks for the SDK-owned iOS XCTest application host."""

from __future__ import annotations

import shutil
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
HOST = ROOT / "iOSRuntimeHost"
SPEC = HOST / "project.yml"
PROJECT = HOST / "CurrentHubRuntimeHost.xcodeproj"


class IOSRuntimeHostTests(unittest.TestCase):
    def test_host_spec_declares_app_test_target_and_local_package(self):
        self.assertTrue(SPEC.is_file(), f"missing iOS host spec: {SPEC}")
        contents = SPEC.read_text(encoding="utf-8")
        for marker in (
            "CurrentHubRuntimeHost:",
            "CurrentHubRuntimeTests:",
            "type: application",
            "type: bundle.unit-test",
            "product: TeslatlasCurrentHub",
            "path: ..",
        ):
            self.assertIn(marker, contents)

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
