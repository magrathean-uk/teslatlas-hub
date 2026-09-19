# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for the fixed TypeScript installed archive binding."""

from pathlib import Path
import unittest

from . import typescript_installed


M1_ARCHIVE = Path(
    "/Users/bolyki/.codex/artifacts/teslatlas-interop/2026-09-08-working-product/"
    "sdk-m1-node-26.7.0-pack/teslatlas-sdk-2026.36.2.tgz"
)
NEW_SHA256 = "03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c"
OLD_SHA256 = "4602b950690baf7b727b839b7c876cb461f8870b7750eb268383a40672fb1233"


class TypeScriptInstalledArchiveTests(unittest.TestCase):
    @staticmethod
    def artifact(sha256):
        return {
            "role": "typescript_sdk_tarball",
            "path": str(M1_ARCHIVE),
            "embedded_version": "2026.36.2",
            "sha256": sha256,
        }

    def test_m1_archive_pin_accepts_new_digest_and_rejects_previous(self):
        accepted = typescript_installed._artifact(
            {"artifacts": [self.artifact(NEW_SHA256)]},
            "typescript_sdk_tarball",
        )
        self.assertEqual(NEW_SHA256, accepted["sha256"])

        with self.assertRaises(typescript_installed.TypeScriptInstalledPending):
            typescript_installed._artifact(
                {"artifacts": [self.artifact(OLD_SHA256)]},
                "typescript_sdk_tarball",
            )


if __name__ == "__main__":
    unittest.main()
