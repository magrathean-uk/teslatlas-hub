#!/usr/bin/env python3
"""Tests for the isolated synthetic vehicle client certificate profile."""

from __future__ import annotations

from pathlib import Path
import subprocess
import tempfile
import unittest

from synthetic_vehicle_identity import (
    RECEIVER_CLIENT_ISSUER,
    generate_synthetic_vehicle_client_identity,
)


VIN = "5YJ3E1EA7KF000004"


def run(*args: str) -> str:
    result = subprocess.run(
        args,
        check=True,
        text=True,
        capture_output=True,
    )
    return result.stdout


class SyntheticVehicleIdentityTests(unittest.TestCase):
    def test_generator_emits_receiver_authorized_client_profile(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory).resolve() / "vehicle-client"
            identity = generate_synthetic_vehicle_client_identity(output, VIN)

            self.assertEqual(identity.client_ca_certificate, output / "vehicle-client-ca.crt")
            self.assertEqual(identity.client_certificate, output / "vehicle-client.crt")
            self.assertEqual(identity.client_key, output / "vehicle-client.key")
            self.assertEqual(output.stat().st_mode & 0o777, 0o700)
            self.assertEqual(identity.client_key.stat().st_mode & 0o777, 0o600)
            self.assertEqual(identity.client_certificate.stat().st_mode & 0o777, 0o600)
            self.assertEqual(identity.client_ca_certificate.stat().st_mode & 0o777, 0o644)

            certificate = run(
                "openssl",
                "x509",
                "-in",
                str(identity.client_certificate),
                "-noout",
                "-subject",
                "-issuer",
                "-text",
            )
            self.assertIn(f"CN={VIN}", certificate)
            self.assertIn(f"CN={RECEIVER_CLIENT_ISSUER}", certificate)
            self.assertIn("CA:FALSE", certificate)
            self.assertIn("Digital Signature", certificate)
            self.assertIn("TLS Web Client Authentication", certificate)

            verification = run(
                "openssl",
                "verify",
                "-CAfile",
                str(identity.client_ca_certificate),
                "-purpose",
                "sslclient",
                str(identity.client_certificate),
            )
            self.assertIn(": OK", verification)

            self.assertEqual(
                {path.name for path in output.iterdir()},
                {"vehicle-client-ca.crt", "vehicle-client.crt", "vehicle-client.key"},
            )

    def test_generator_rejects_malformed_vin(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory).resolve() / "vehicle-client"

            with self.assertRaises(ValueError):
                generate_synthetic_vehicle_client_identity(output, "not-a-vin")


if __name__ == "__main__":
    unittest.main()
