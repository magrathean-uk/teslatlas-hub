# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for the fixed TypeScript installed archive binding."""

from pathlib import Path
import tempfile
import unittest

from . import typescript_installed


NEW_SHA256 = "42348d3688c5a723bd154e3c1e8172bc07b20d1bf28944818ccfdbf3d97891f7"
OLD_SHA256 = "4602b950690baf7b727b839b7c876cb461f8870b7750eb268383a40672fb1233"


class TypeScriptInstalledArchiveTests(unittest.TestCase):
    @staticmethod
    def artifact(path, sha256):
        return {
            "role": "typescript_sdk_tarball",
            "path": str(path),
            "embedded_version": "2026.36.2",
            "sha256": sha256,
        }

    def test_current_archive_requires_exact_staged_bytes_and_rejects_old_identity(self):
        self.assertEqual(83, typescript_installed.SDK_MEMBER_COUNT)
        with tempfile.TemporaryDirectory() as temporary:
            candidate = Path(temporary).resolve() / "teslatlas-sdk-2026.36.2.tgz"
            candidate.write_bytes(b"not-the-reviewed-current-package")
            candidate.chmod(0o600)
            with self.assertRaisesRegex(
                typescript_installed.TypeScriptInstalledPending,
                "artifact digest changed",
            ):
                typescript_installed._artifact(
                    {"artifacts": [self.artifact(candidate, NEW_SHA256)]},
                    "typescript_sdk_tarball",
                )
            with self.assertRaisesRegex(
                typescript_installed.TypeScriptInstalledPending,
                "outside the reviewed launch inventory",
            ):
                typescript_installed._artifact(
                    {"artifacts": [self.artifact(candidate, OLD_SHA256)]},
                    "typescript_sdk_tarball",
                )

    def test_lane_binding_matches_viewer_free_installed_contract_bytes(self):
        path = typescript_installed.LANE_ROOT / "installed_contract.mjs"
        self.assertEqual(
            typescript_installed.LANE_FILES["installed_contract.mjs"],
            typescript_installed._digest(path.read_bytes()),
        )


if __name__ == "__main__":
    unittest.main()
