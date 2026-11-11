#!/usr/bin/env python3
"""Generate the client certificate profile accepted by the pinned receiver."""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


RECEIVER_CLIENT_ISSUER = "Tesla Motors Products CA"
VIN = re.compile(r"^[A-HJ-NPR-Z0-9]{17}$")


class IdentityGenerationError(ValueError):
    """The requested synthetic client identity cannot be generated safely."""


@dataclass(frozen=True)
class SyntheticVehicleClientIdentity:
    client_ca_certificate: Path
    client_certificate: Path
    client_key: Path


def _run_openssl(stage: Path, *arguments: str) -> None:
    try:
        subprocess.run(
            ["openssl", *arguments],
            cwd=stage,
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
    except FileNotFoundError as error:
        raise IdentityGenerationError("openssl is required") from error
    except subprocess.CalledProcessError as error:
        detail = error.stderr.strip().splitlines()
        reason = detail[-1] if detail else "command failed"
        raise IdentityGenerationError(f"openssl identity generation failed: {reason}") from error


def _validate_output_directory(output: Path) -> None:
    if not output.is_absolute() or output.resolve(strict=False) != output:
        raise IdentityGenerationError("identity output path must be canonical and absolute")
    if output.exists() or output.is_symlink():
        raise IdentityGenerationError("identity output directory already exists")
    if not output.parent.is_dir() or output.parent.is_symlink():
        raise IdentityGenerationError("identity output parent must be a real directory")


def generate_synthetic_vehicle_client_identity(
    output_directory: Path,
    vin: str,
) -> SyntheticVehicleClientIdentity:
    """Create a fresh CA and receiver-authorized client identity for ``vin``.

    The CA is deliberately named after the pinned receiver's known Tesla issuer;
    the receiver derives the device identity from the leaf common name.  The CA
    private key stays in a private staging directory and is never returned.
    """

    if not isinstance(vin, str) or VIN.fullmatch(vin) is None:
        raise IdentityGenerationError("VIN must be a 17-character VIN")

    output = Path(output_directory)
    _validate_output_directory(output)

    stage = Path(tempfile.mkdtemp(prefix=".synthetic-vehicle-", dir=output.parent))
    os.chmod(stage, 0o700)
    output_created = False
    old_umask = os.umask(0o077)
    try:
        ca_key = stage / "vehicle-client-ca.key"
        ca_certificate = stage / "vehicle-client-ca.crt"
        client_key = stage / "vehicle-client.key"
        client_request = stage / "vehicle-client.csr"
        client_certificate = stage / "vehicle-client.crt"
        extensions = stage / "vehicle-client.ext"

        _run_openssl(stage, "genrsa", "-out", str(ca_key), "2048")
        _run_openssl(
            stage,
            "req",
            "-x509",
            "-new",
            "-sha256",
            "-days",
            "1825",
            "-key",
            str(ca_key),
            "-subj",
            f"/CN={RECEIVER_CLIENT_ISSUER}",
            "-addext",
            "basicConstraints=critical,CA:TRUE,pathlen:1",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
            "-out",
            str(ca_certificate),
        )

        _run_openssl(stage, "genrsa", "-out", str(client_key), "2048")
        _run_openssl(
            stage,
            "req",
            "-new",
            "-sha256",
            "-key",
            str(client_key),
            "-subj",
            f"/CN={vin}",
            "-out",
            str(client_request),
        )
        extensions.write_text(
            "basicConstraints=critical,CA:FALSE\n"
            "keyUsage=critical,digitalSignature,keyEncipherment\n"
            "extendedKeyUsage=clientAuth\n",
            encoding="ascii",
        )
        os.chmod(extensions, 0o600)
        _run_openssl(
            stage,
            "x509",
            "-req",
            "-sha256",
            "-days",
            "1825",
            "-in",
            str(client_request),
            "-CA",
            str(ca_certificate),
            "-CAkey",
            str(ca_key),
            "-CAcreateserial",
            "-out",
            str(client_certificate),
            "-extfile",
            str(extensions),
        )

        _run_openssl(
            stage,
            "verify",
            "-CAfile",
            str(ca_certificate),
            "-purpose",
            "sslclient",
            str(client_certificate),
        )

        os.chmod(ca_certificate, 0o644)
        os.chmod(client_certificate, 0o600)
        os.chmod(client_key, 0o600)
        ca_key.unlink()

        output.mkdir(mode=0o700)
        output_created = True
        for source, destination, mode in (
            (ca_certificate, output / "vehicle-client-ca.crt", 0o644),
            (client_certificate, output / "vehicle-client.crt", 0o600),
            (client_key, output / "vehicle-client.key", 0o600),
        ):
            source.replace(destination)
            os.chmod(destination, mode)
        os.chmod(output, 0o700)
        return SyntheticVehicleClientIdentity(
            client_ca_certificate=output / "vehicle-client-ca.crt",
            client_certificate=output / "vehicle-client.crt",
            client_key=output / "vehicle-client.key",
        )
    except (OSError, IdentityGenerationError):
        if output_created:
            shutil.rmtree(output, ignore_errors=True)
        raise
    finally:
        os.umask(old_umask)
        shutil.rmtree(stage, ignore_errors=True)

