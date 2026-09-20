"""Static and adversarial checks for the bounded platform-gate harness."""

from __future__ import annotations

import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import platform_gate


ROOT = Path(__file__).resolve().parents[1]


class PlatformGateTests(unittest.TestCase):
    def test_repository_contract_and_accepted_source_identity(self):
        result = platform_gate.verify_repository(ROOT)
        self.assertTrue(result["verified"])
        self.assertFalse(result["runtime_executed"])
        self.assertEqual(platform_gate.SOURCE_IDENTITY, result["source_package_identity_sha256"])
        self.assertEqual(124, result["source_input_files"])
        self.assertEqual(769736, result["source_input_bytes"])

    def test_contract_rejects_duplicate_security_key(self):
        payload = platform_gate.CONTRACT_PATH.read_text(encoding="utf-8")
        duplicate = payload.replace(
            '"schema_version": 1,',
            '"schema_version": 1,\n  "schema_version": 1,',
            1,
        )
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "contract.json"
            path.write_text(duplicate, encoding="utf-8")
            with self.assertRaisesRegex(platform_gate.PlatformGateError, "duplicate key"):
                platform_gate.load_contract(path)

    def test_contract_rejects_changed_source_identity(self):
        mutations = (
            ("source_handoff", "identity_sha256", "0" * 64),
            ("linux_arm64", "official_catalog_url", "https://example.invalid/swift"),
            ("linux_arm64", "dockerfile_source_git_commit", "0" * 40),
        )
        for section, key, value in mutations:
            with self.subTest(key=key), tempfile.TemporaryDirectory() as temporary:
                contract = json.loads(
                    platform_gate.CONTRACT_PATH.read_text(encoding="utf-8")
                )
                contract[section][key] = value
                path = Path(temporary) / "contract.json"
                path.write_text(json.dumps(contract), encoding="utf-8")
                with self.assertRaisesRegex(
                    platform_gate.PlatformGateError, "accepted inputs"
                ):
                    platform_gate.load_contract(path)

    def test_commands_are_bounded_and_do_not_use_matrix_or_emulation(self):
        ios = platform_gate.command_for("ios17", ROOT)
        macos = platform_gate.command_for("macos14", ROOT)
        linux = platform_gate.command_for("linux-arm64", ROOT)
        self.assertIn("OS=17.0,name=iPhone 15", " ".join(ios))
        self.assertIn("CurrentHubRuntimeHost", ios)
        self.assertIn("external-four-library-consumer", " ".join(macos))
        self.assertIn("linux/arm64/v8", linux)
        self.assertIn("<verified-source-handoff>", linux)
        self.assertNotIn(str(ROOT), linux[-1])
        joined = " ".join(ios + macos + linux).lower()
        for forbidden in ("matrix_wire.py", "linux/amd64", "x86", "qemu"):
            self.assertNotIn(forbidden, joined)

    def test_linux_container_assigns_the_unprivileged_user_home(self):
        dockerfile = (ROOT / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn("ENV HOME=/home/swiftuser", dockerfile)
        self.assertIn(
            "COPY --chown=swiftuser:swiftuser teslatlas-sdk-swift/", dockerfile
        )
        self.assertIn(
            "COPY --chown=swiftuser:swiftuser external-four-library-consumer/",
            dockerfile,
        )
        self.assertIn(
            "RUN chmod -R a-w /workspace/teslatlas-sdk-swift "
            "/workspace/external-four-library-consumer",
            dockerfile,
        )
        self.assertIn(
            'CMD ["swift", "run", "--package-path", '
            '"/workspace/external-four-library-consumer"',
            dockerfile,
        )
        self.assertLess(
            dockerfile.index("ENV HOME=/home/swiftuser"),
            dockerfile.index("USER swiftuser"),
        )

    def test_linux_consumer_output_requires_all_four_products_in_order(self):
        platform_gate._require_linux_consumer_output(
            "build output\n" + platform_gate.LINUX_CONSUMER_OUTPUT + "\n"
        )
        with self.assertRaisesRegex(platform_gate.PlatformGateError, "all four"):
            platform_gate._require_linux_consumer_output(
                "TeslatlasHubSDK,TeslatlasCommands\n"
            )

    @mock.patch("platform_gate.platform.mac_ver", return_value=("27.0", ("", "", ""), ""))
    @mock.patch("platform_gate.platform.machine", return_value="arm64")
    @mock.patch("platform_gate.platform.system", return_value="Darwin")
    def test_macos_floor_rejects_newer_host(self, _system, _machine, _version):
        with self.assertRaisesRegex(platform_gate.PlatformGateError, "requires macOS 14"):
            platform_gate._require_native_darwin(14)

    @mock.patch("platform_gate.subprocess.run")
    def test_ios_inventory_requires_exact_ios17_device(self, run):
        run.return_value.returncode = 0
        run.return_value.stdout = json.dumps(
            {
                "devices": {
                    "com.apple.CoreSimulator.SimRuntime.iOS-18-0": [
                        {"name": "iPhone 15", "isAvailable": True}
                    ]
                }
            }
        ).encode("utf-8")
        with self.assertRaisesRegex(platform_gate.PlatformGateError, "iOS 17.0"):
            platform_gate._require_ios_simulator("17.0", "iPhone 15")

    @mock.patch("platform_gate.platform.machine", return_value="x86_64")
    @mock.patch("platform_gate.platform.system", return_value="Linux")
    def test_linux_run_rejects_non_arm64_before_docker(self, _system, _machine):
        with mock.patch("platform_gate.verify_repository", return_value={}), mock.patch(
            "platform_gate.subprocess.run"
        ) as run:
            with self.assertRaisesRegex(platform_gate.PlatformGateError, "native Linux ARM64"):
                platform_gate.run_gate("linux-arm64", ROOT)
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
