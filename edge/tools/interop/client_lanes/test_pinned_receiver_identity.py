#!/usr/bin/env python3
"""Run the generated identity through an exact pinned receiver verifier copy."""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from synthetic_vehicle_identity import generate_synthetic_vehicle_client_identity


DEFAULT_VIN = "5YJ3E1EA7KF000004"


GO_TEST = r'''package messages

import (
	"crypto/x509"
	"encoding/pem"
	"errors"
	"os"
	"testing"
)

func readCertificate(t *testing.T) *x509.Certificate {
	t.Helper()
	raw, err := os.ReadFile(os.Getenv("SYNTHETIC_CLIENT_CERT"))
	if err != nil {
		t.Fatal(err)
	}
	block, _ := pem.Decode(raw)
	if block == nil {
		t.Fatal("client certificate is not PEM")
	}
	certificate, err := x509.ParseCertificate(block.Bytes)
	if err != nil {
		t.Fatal(err)
	}
	return certificate
}

func TestGeneratedSyntheticIdentityIsAccepted(t *testing.T) {
	clientType, deviceID, err := CreateIdentityFromCert(readCertificate(t))
	if err != nil {
		t.Fatalf("generated identity rejected: %v", err)
	}
	if clientType != "vehicle_device" || deviceID != os.Getenv("SYNTHETIC_VIN") {
		t.Fatalf("unexpected identity: %s.%s", clientType, deviceID)
	}
}

func TestWrongIssuerIsRejected(t *testing.T) {
	certificate := readCertificate(t)
	certificate.Issuer.CommonName = "Teslatlas Synthetic Vehicle CA"
	_, _, err := CreateIdentityFromCert(certificate)
	if !errors.Is(err, ErrUnauthorizedCert) {
		t.Fatalf("wrong issuer returned %v, want ErrUnauthorizedCert", err)
	}
}

func TestKnownOIDIssuerWithoutIdentityOIDIsRejected(t *testing.T) {
	certificate := readCertificate(t)
	certificate.Issuer.CommonName = "Tesla Motors Product Issuing CA"
	certificate.UnknownExtKeyUsage = nil
	_, _, err := CreateIdentityFromCert(certificate)
	if !errors.Is(err, ErrUnauthorizedCert) {
		t.Fatalf("missing identity OID returned %v, want ErrUnauthorizedCert", err)
	}
}
'''


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--pinned-source",
        required=True,
        type=Path,
        help="root of the exact pinned Fleet Telemetry source checkout",
    )
    parser.add_argument("--vin", default=DEFAULT_VIN, help="synthetic VIN to generate")
    parser.add_argument(
        "--certificate",
        type=Path,
        help="use an existing generated client certificate instead of generating one",
    )
    parser.add_argument("--go", default="go", help="Go executable to use")
    return parser.parse_args()


def main() -> int:
    arguments = _parse_args()
    verifier = arguments.pinned_source / "messages" / "identity.go"
    if not verifier.is_file() or verifier.is_symlink():
        raise SystemExit(f"pinned verifier is missing: {verifier}")

    verifier_sha256 = hashlib.sha256(verifier.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="pinned-receiver-identity-") as directory:
        root = Path(directory)
        if arguments.certificate is None:
            identity = generate_synthetic_vehicle_client_identity(
                (root / "identity").resolve(), arguments.vin
            )
            certificate_path = identity.client_certificate
        else:
            certificate_path = arguments.certificate
            if (
                not certificate_path.is_absolute()
                or certificate_path.resolve(strict=False) != certificate_path
                or certificate_path.is_symlink()
                or not certificate_path.is_file()
            ):
                raise SystemExit("--certificate must be a canonical regular file")
        messages = root / "messages"
        messages.mkdir()
        shutil.copyfile(verifier, messages / "identity.go")
        (root / "go.mod").write_text(
            "module teslatlas-pinned-receiver-identity-regression\n\ngo 1.23.0\n",
            encoding="ascii",
        )
        (messages / "identity_test.go").write_text(GO_TEST, encoding="ascii")

        environment = os.environ.copy()
        environment.update(
            {
                "GOTOOLCHAIN": "local",
                "GOPROXY": "off",
                "GOSUMDB": "off",
                "SYNTHETIC_CLIENT_CERT": str(certificate_path),
                "SYNTHETIC_VIN": arguments.vin,
            }
        )
        result = subprocess.run(
            [arguments.go, "test", "./messages"],
            cwd=root,
            env=environment,
            check=False,
            text=True,
            capture_output=True,
        )
        if result.returncode != 0:
            raise SystemExit(result.stdout + result.stderr)

    print(f"pinned receiver identity regression: 3 passed; verifier_sha256={verifier_sha256}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
