#!/usr/bin/env python3
"""Tests for the Hub consumer trust-bundle handoff boundary."""

from __future__ import annotations

import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import unittest


HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "prepare_hub_consumer_inputs.sh"
VALID_EDGE_BEARER = (
    "tte1.123e4567-e89b-12d3-a456-426614174000."
    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
)


def run(*arguments: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        arguments,
        check=check,
        capture_output=True,
        text=True,
    )


def make_ca(root: Path, name: str) -> tuple[Path, Path]:
    key = root / f"{name}.key"
    certificate = root / f"{name}.crt"
    run(
        "openssl",
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-sha256",
        "-days",
        "2",
        "-subj",
        f"/CN={name}",
        "-addext",
        "basicConstraints=critical,CA:true",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
        "-keyout",
        str(key),
        "-out",
        str(certificate),
    )
    return certificate, key


def make_leaf(
    root: Path,
    name: str,
    ca_certificate: Path,
    ca_key: Path,
    extensions: str,
) -> tuple[Path, Path]:
    key = root / f"{name}.key"
    csr = root / f"{name}.csr"
    certificate = root / f"{name}.crt"
    ext = root / f"{name}.ext"
    ext.write_text(extensions, encoding="utf-8")
    run(
        "openssl",
        "req",
        "-new",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-sha256",
        "-subj",
        f"/CN={name}",
        "-keyout",
        str(key),
        "-out",
        str(csr),
    )
    run(
        "openssl",
        "x509",
        "-req",
        "-sha256",
        "-days",
        "2",
        "-in",
        str(csr),
        "-CA",
        str(ca_certificate),
        "-CAkey",
        str(ca_key),
        "-CAcreateserial",
        "-out",
        str(certificate),
        "-extfile",
        str(ext),
    )
    return certificate, key


class HubConsumerInputTests(unittest.TestCase):
    def test_bearer_rejects_tokens_that_are_not_edge_credentials(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server_ca, server_ca_key = make_ca(root, "handoff-server-bearer-ca")
            client_ca, client_ca_key = make_ca(root, "handoff-client-bearer-ca")
            server_leaf, _ = make_leaf(
                root,
                "edge-delivery-bearer",
                server_ca,
                server_ca_key,
                "basicConstraints=critical,CA:false\n"
                "keyUsage=critical,digitalSignature,keyEncipherment\n"
                "subjectAltName=IP:192.168.64.10,DNS:edge-delivery.local\n"
                "extendedKeyUsage=serverAuth\n",
            )
            client_leaf, client_key = make_leaf(
                root,
                "hub-client-bearer",
                client_ca,
                client_ca_key,
                "basicConstraints=critical,CA:false\n"
                "keyUsage=critical,digitalSignature,keyEncipherment\n"
                "extendedKeyUsage=clientAuth\n",
            )
            bearer = root / "bearer"
            bearer.write_text("owner-only-test-bearer", encoding="utf-8")
            output = root / "hub-inputs"

            result = run(
                str(SCRIPT),
                "--server-ca-source",
                str(server_ca),
                "--server-certificate",
                str(server_leaf),
                "--server-ip",
                "192.168.64.10",
                "--client-ca",
                str(client_ca),
                "--client-certificate",
                str(client_leaf),
                "--client-key",
                str(client_key),
                "--bearer",
                str(bearer),
                "--output-dir",
                str(output),
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())

    def test_server_ip_rejects_wildcard_before_output_creation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server_ca, server_ca_key = make_ca(root, "handoff-server-address-ca")
            client_ca, client_ca_key = make_ca(root, "handoff-client-address-ca")
            server_leaf, _ = make_leaf(
                root,
                "edge-delivery-address",
                server_ca,
                server_ca_key,
                "basicConstraints=critical,CA:false\n"
                "keyUsage=critical,digitalSignature,keyEncipherment\n"
                "subjectAltName=IP:192.168.64.10,DNS:edge-delivery.local\n"
                "extendedKeyUsage=serverAuth\n",
            )
            client_leaf, client_key = make_leaf(
                root,
                "hub-client-address",
                client_ca,
                client_ca_key,
                "basicConstraints=critical,CA:false\n"
                "keyUsage=critical,digitalSignature,keyEncipherment\n"
                "extendedKeyUsage=clientAuth\n",
            )
            bearer = root / "bearer"
            bearer.write_text("owner-only-address-bearer", encoding="utf-8")
            output = root / "hub-inputs"

            result = run(
                str(SCRIPT),
                "--server-ca-source",
                str(server_ca),
                "--server-certificate",
                str(server_leaf),
                "--server-ip",
                "0.0.0.0",
                "--client-ca",
                str(client_ca),
                "--client-certificate",
                str(client_leaf),
                "--client-key",
                str(client_key),
                "--bearer",
                str(bearer),
                "--output-dir",
                str(output),
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())

    def test_server_certificate_rejects_wildcard_loopback_and_other_ip_sans(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server_ca, server_ca_key = make_ca(root, "handoff-server-reject-ca")
            client_ca, client_ca_key = make_ca(root, "handoff-client-reject-ca")
            client_leaf, client_key = make_leaf(
                root,
                "hub-client-reject",
                client_ca,
                client_ca_key,
                "basicConstraints=critical,CA:false\n"
                "keyUsage=critical,digitalSignature,keyEncipherment\n"
                "extendedKeyUsage=clientAuth\n",
            )
            bearer = root / "bearer"
            bearer.write_text("owner-only-reject-bearer", encoding="utf-8")

            for label, server_san in (
                ("wildcard", "192.168.64.10,IP:0.0.0.0"),
                ("loopback", "192.168.64.10,IP:127.0.0.1"),
                ("other", "192.168.64.10,IP:192.168.64.11"),
            ):
                with self.subTest(server_san=server_san):
                    server_leaf, _ = make_leaf(
                        root,
                        f"edge-delivery-{label}",
                        server_ca,
                        server_ca_key,
                        "basicConstraints=critical,CA:false\n"
                        "keyUsage=critical,digitalSignature,keyEncipherment\n"
                        f"subjectAltName=IP:{server_san},DNS:edge-delivery.local\n"
                        "extendedKeyUsage=serverAuth\n",
                    )
                    output = root / f"hub-inputs-{label}"
                    result = run(
                        str(SCRIPT),
                        "--server-ca-source",
                        str(server_ca),
                        "--server-certificate",
                        str(server_leaf),
                        "--server-ip",
                        "192.168.64.10",
                        "--client-ca",
                        str(client_ca),
                        "--client-certificate",
                        str(client_leaf),
                        "--client-key",
                        str(client_key),
                        "--bearer",
                        str(bearer),
                        "--output-dir",
                        str(output),
                        check=False,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse((output / "server-ca.pem").exists())

    def test_generated_bundle_accepts_explicit_non_loopback_server_ip(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server_ca, server_ca_key = make_ca(root, "handoff-server-private-ca")
            client_ca, client_ca_key = make_ca(root, "handoff-client-private-ca")
            server_leaf, _ = make_leaf(
                root,
                "edge-delivery-private",
                server_ca,
                server_ca_key,
                "basicConstraints=critical,CA:false\n"
                "keyUsage=critical,digitalSignature,keyEncipherment\n"
                "subjectAltName=IP:192.168.64.10,DNS:edge-delivery.local\n"
                "extendedKeyUsage=serverAuth\n",
            )
            client_leaf, client_key = make_leaf(
                root,
                "hub-client-private",
                client_ca,
                client_ca_key,
                "basicConstraints=critical,CA:false\n"
                "keyUsage=critical,digitalSignature,keyEncipherment\n"
                "extendedKeyUsage=clientAuth\n",
            )
            bearer = root / "bearer"
            bearer.write_text(VALID_EDGE_BEARER, encoding="utf-8")
            output = root / "hub-inputs"

            result = run(
                str(SCRIPT),
                "--server-ca-source",
                str(server_ca),
                "--server-certificate",
                str(server_leaf),
                "--server-ip",
                "192.168.64.10",
                "--client-ca",
                str(client_ca),
                "--client-certificate",
                str(client_leaf),
                "--client-key",
                str(client_key),
                "--bearer",
                str(bearer),
                "--output-dir",
                str(output),
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((output / "server-ca.pem").is_file())

    def test_generated_bundle_uses_issuing_server_ca_and_separate_client_auth(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            server_ca, server_ca_key = make_ca(root, "handoff-server-issuing-ca")
            client_ca, client_ca_key = make_ca(root, "handoff-client-issuing-ca")
            server_leaf, _ = make_leaf(
                root,
                "edge-delivery",
                server_ca,
                server_ca_key,
                "basicConstraints=critical,CA:false\n"
                "keyUsage=critical,digitalSignature,keyEncipherment\n"
                "subjectAltName=IP:127.0.0.1,DNS:localhost\n"
                "extendedKeyUsage=serverAuth\n",
            )
            client_leaf, client_key = make_leaf(
                root,
                "hub-client",
                client_ca,
                client_ca_key,
                "basicConstraints=critical,CA:false\n"
                "keyUsage=critical,digitalSignature,keyEncipherment\n"
                "extendedKeyUsage=clientAuth\n",
            )
            bearer = root / "bearer"
            bearer.write_text(VALID_EDGE_BEARER, encoding="utf-8")
            output = root / "hub-inputs"

            result = run(
                str(SCRIPT),
                "--server-ca-source",
                str(server_ca),
                "--server-certificate",
                str(server_leaf),
                "--server-ip",
                "127.0.0.1",
                "--client-ca",
                str(client_ca),
                "--client-certificate",
                str(client_leaf),
                "--client-key",
                str(client_key),
                "--bearer",
                str(bearer),
                "--output-dir",
                str(output),
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o700)

            handed_off_ca = output / "server-ca.pem"
            ca_text = run(
                "openssl",
                "x509",
                "-in",
                str(handed_off_ca),
                "-noout",
                "-subject",
                "-text",
            ).stdout
            self.assertIn("handoff-server-issuing-ca", ca_text)
            self.assertRegex(ca_text, re.compile(r"Basic Constraints: critical.*?CA:TRUE", re.S))
            self.assertIn(
                "OK",
                run(
                    "openssl",
                    "verify",
                    "-CAfile",
                    str(handed_off_ca),
                    "-purpose",
                    "sslserver",
                    str(server_leaf),
                ).stdout,
            )
            server_text = run(
                "openssl",
                "x509",
                "-in",
                str(server_leaf),
                "-noout",
                "-text",
            ).stdout
            self.assertRegex(
                server_text,
                re.compile(r"Subject Alternative Name:.*?IP Address:127\.0\.0\.1", re.S),
            )
            self.assertIn(
                "OK",
                run(
                    "openssl",
                    "verify",
                    "-CAfile",
                    str(client_ca),
                    "-purpose",
                    "sslclient",
                    str(output / "hub-client.crt"),
                ).stdout,
            )
            client_cert_key = run(
                "openssl",
                "x509",
                "-in",
                str(output / "hub-client.crt"),
                "-pubkey",
                "-noout",
            ).stdout
            client_key_public = run(
                "openssl",
                "pkey",
                "-in",
                str(output / "hub-client.key"),
                "-pubout",
            ).stdout
            self.assertEqual(client_cert_key, client_key_public)

            for name in ("server-ca.pem", "hub-client.crt", "hub-client.key", "delivery-bearer"):
                self.assertEqual(stat.S_IMODE((output / name).stat().st_mode), 0o600)
            self.assertEqual((output / "delivery-bearer").read_text(encoding="utf-8"), VALID_EDGE_BEARER)

            bad_output = root / "bad-inputs"
            rejected = run(
                str(SCRIPT),
                "--server-ca-source",
                str(server_leaf),
                "--server-certificate",
                str(server_leaf),
                "--server-ip",
                "127.0.0.1",
                "--client-ca",
                str(client_ca),
                "--client-certificate",
                str(client_leaf),
                "--client-key",
                str(client_key),
                "--bearer",
                str(bearer),
                "--output-dir",
                str(bad_output),
                check=False,
            )
            self.assertNotEqual(rejected.returncode, 0)
            self.assertFalse((bad_output / "server-ca.pem").exists())


if __name__ == "__main__":
    unittest.main()
