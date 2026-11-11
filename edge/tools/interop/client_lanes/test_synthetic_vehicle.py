#!/usr/bin/env python3
"""Tests for the identity-bound synthetic receiver invocation contract."""

from __future__ import annotations

import json
from pathlib import Path
import tempfile
import unittest

from synthetic_vehicle import InvocationError, build_invocation


VIN = "5YJ3E1EA7KF000002"


class SyntheticVehicleInvocationTests(unittest.TestCase):
    def test_invocation_derives_vin_from_sealed_producer_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            manifest = root / "handoff.json"
            manifest.write_text(
                json.dumps({"producer": {"vin": VIN}}), encoding="utf-8"
            )
            helper = root / "synthetic-vehicle"
            helper.write_bytes(b"teslatlas-synthetic-vehicle-identity-v2")
            helper.chmod(0o700)
            cert = root / "vehicle-client.crt"
            key = root / "vehicle-client.key"
            server_ca = root / "server-ca.pem"
            for path in (cert, key, server_ca):
                path.write_bytes(b"input")
                path.chmod(0o600)

            argv = build_invocation(
                manifest_path=manifest,
                helper_path=helper,
                endpoint="wss://127.0.0.1:20444/",
                certificate_path=cert,
                key_path=key,
                server_ca_path=server_ca,
                transaction_id="edge-r1-test-001",
                created_at_ms=1_800_000_000_000,
            )

            self.assertEqual(argv[0], str(helper))
            self.assertIn(("-vin", VIN), list(zip(argv[1::2], argv[2::2])))
            self.assertNotIn("5YJ3E1EA7KF000001", argv)
            self.assertEqual(argv[argv.index("-cert") + 1], str(cert))
            self.assertEqual(argv[argv.index("-key") + 1], str(key))
            self.assertEqual(argv[argv.index("-server-ca") + 1], str(server_ca))

    def test_invocation_rejects_helper_without_explicit_identity_contract(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            manifest = root / "handoff.json"
            manifest.write_text(json.dumps({"producer": {"vin": VIN}}), encoding="utf-8")
            helper = root / "synthetic-vehicle"
            helper.write_bytes(b"legacy helper with 5YJ3E1EA7KF000001")
            helper.chmod(0o700)
            inputs = []
            for name in ("vehicle-client.crt", "vehicle-client.key", "server-ca.pem"):
                path = root / name
                path.write_bytes(b"input")
                path.chmod(0o600)
                inputs.append(path)

            with self.assertRaises(InvocationError):
                build_invocation(
                    manifest_path=manifest,
                    helper_path=helper,
                    endpoint="wss://127.0.0.1:20444/",
                    certificate_path=inputs[0],
                    key_path=inputs[1],
                    server_ca_path=inputs[2],
                    transaction_id="edge-r1-test-002",
                    created_at_ms=1_800_000_000_000,
                )


if __name__ == "__main__":
    unittest.main()
