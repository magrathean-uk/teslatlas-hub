#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Create local, candidate-bound release evidence without building anything."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from datetime import datetime, timezone
import xml.etree.ElementTree as ET


SCHEMA = "teslatlas.release-evidence/v1"
TAG_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
PACKAGE_VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$")
FINGERPRINT_RE = re.compile(r"^(?:[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64})$")
UUID_RE = re.compile(
    r"^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-"
    r"[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$"
)
GO_EVIDENCE_NAMES = (
    "GO_THIRD_PARTY_NOTICES.generated.md",
    "go-build-receipt.json",
    "go-component-manifest.json",
    "go-dependency-inventory.json",
    "go-sbom.spdx.json",
    "tesla-http-proxy.unsigned",
    "tesla-http-proxy-go-sources.tar.gz",
)
RUST_SOURCE_EVIDENCE_NAMES = (
    "rust-source-evidence-manifest.json",
    "rust-source-inventory.json",
    "rust-vendored-sources.tar.gz",
)
FLEET_TELEMETRY_EVIDENCE_NAMES = (
    "FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md",
    "fleet-telemetry-bridge-lock.json",
    "fleet-telemetry-component-manifest.json",
    "fleet-telemetry-dependency-inventory.json",
    "fleet-telemetry-legal-lock.json",
    "fleet-telemetry-go-module-sources.tar.gz",
    "fleet-telemetry-license-material.tar.gz",
    "fleet-telemetry-sbom.spdx.json",
    "fleet-telemetry-upstream-source.tar.gz",
    "fleet-telemetry.unsigned",
)
LEGAL_BUNDLE_BASE_NAMES = (
    "RUST_THIRD_PARTY_NOTICES.generated.md",
    "legal-bundle-manifest.json",
    "rust-dependency-inventory.json",
    "rust-sbom.spdx.json",
)
LEGAL_BUNDLE_SIDECAR_NAMES = (
    "FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md",
    "GO_THIRD_PARTY_NOTICES.generated.md",
    "fleet-telemetry-bridge-lock.json",
    "fleet-telemetry-dependency-inventory.json",
    "fleet-telemetry-legal-lock.json",
    "fleet-telemetry-license-material.tar.gz",
    "fleet-telemetry-sbom.spdx.json",
    "go-dependency-inventory.json",
    "go-sbom.spdx.json",
)
DEBIAN_DEPENDENCY_LEGAL_PREFIX = "usr/share/doc/teslatlas-hub/dependency-legal/"
MACOS_DEPENDENCY_LEGAL_DIRECTORY = "DependencyLegal"
DEBIAN_ATTESTATION_RECEIPT_NAME = "debian-native-attestation.json"
DEBIAN_ATTESTATION_SIGNATURE_NAME = "debian-native-attestation.sig"
DEBIAN_ATTESTATION_PUBLIC_KEY_NAME = "TeslatlasHubDebianAttestationPublicKey.pem"
MAX_DEBIAN_ARTIFACT_BYTES = 256 * 1024 * 1024
MAX_DEBIAN_CONTROL_BYTES = 1024 * 1024
MAX_DEBIAN_TAR_BYTES = 512 * 1024 * 1024
MAX_LEGAL_FILE_BYTES = 16 * 1024 * 1024
MAX_HUB_BINARY_BYTES = 128 * 1024 * 1024
MAX_ATTESTATION_RECEIPT_BYTES = 1024 * 1024
MAX_ATTESTATION_PUBLIC_KEY_BYTES = 64 * 1024
MAX_CARGO_MANIFEST_BYTES = 1024 * 1024
MAX_CARGO_LOCK_BYTES = 32 * 1024 * 1024
DEBIAN_LEGAL_FILES = {
    "usr/share/doc/teslatlas-hub/copyright": "LICENSE",
    "usr/share/doc/teslatlas-hub/NOTICE": "NOTICE",
    "usr/share/doc/teslatlas-hub/THIRD_PARTY_NOTICES.md":
        "docs/legal/third-party-notices.md",
    "usr/share/doc/teslatlas-hub/PROVENANCE.md": "docs/legal/provenance.md",
    "usr/share/doc/teslatlas-hub/ADDITIONAL_TERMS.md": "docs/legal/additional-terms.md",
    "usr/share/doc/teslatlas-hub/SOURCE_AVAILABILITY.md":
        "docs/legal/source-availability.md",
    "usr/share/doc/teslatlas-hub/RELEASE_VERIFICATION.md":
        "docs/releases/verification.md",
}
MACOS_LEGAL_FILES = {
    "LICENSE": "LICENSE",
    "NOTICE": "NOTICE",
    "THIRD_PARTY_NOTICES.md": "docs/legal/third-party-notices.md",
    "PROVENANCE.md": "docs/legal/provenance.md",
    "ADDITIONAL_TERMS.md": "docs/legal/additional-terms.md",
    "SOURCE_AVAILABILITY.md": "docs/legal/source-availability.md",
    "RELEASE_VERIFICATION.md": "docs/releases/verification.md",
}


class GateError(RuntimeError):
    pass


@dataclass(frozen=True)
class ArtifactWitness:
    path: Path
    relative_path: str
    size: int
    digest: str
    device: int
    inode: int
    mtime_ns: int


@dataclass(frozen=True)
class DebianAttestation:
    architecture: str
    package: ArtifactWitness
    receipt: ArtifactWitness
    signature: ArtifactWitness


def run(args: list[str], cwd: Path, *, env: dict[str, str] | None = None,
        capture: bool = True) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            args, cwd=cwd, env=env, text=True, capture_output=capture, check=True
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        detail = ""
        if isinstance(exc, subprocess.CalledProcessError):
            detail = (exc.stderr or exc.stdout or "").strip()
        raise GateError(f"command failed: {' '.join(args)}{': ' + detail if detail else ''}") from exc


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def capture_artifact(repo: Path, path: Path) -> ArtifactWitness:
    try:
        before = os.lstat(path)
    except OSError as exc:
        raise GateError(f"artifact is unavailable: {path}") from exc
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
        raise GateError(f"artifact must be a regular, non-symlink file: {path}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise GateError(f"artifact cannot be safely opened: {path}") from exc
    try:
        opened = os.fstat(descriptor)
        if (
            (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)
            or opened.st_nlink != 1
        ):
            raise GateError(f"artifact changed while opening: {path}")
        digest = hashlib.sha256()
        while True:
            block = os.read(descriptor, 1024 * 1024)
            if not block:
                break
            digest.update(block)
        after = os.fstat(descriptor)
        if (
            after.st_size != opened.st_size
            or after.st_nlink != 1
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
        ):
            raise GateError(f"artifact changed while reading: {path}")
    finally:
        os.close(descriptor)
    try:
        current = os.lstat(path)
    except OSError as exc:
        raise GateError(f"artifact changed after reading: {path}") from exc
    if (
        (current.st_dev, current.st_ino) != (opened.st_dev, opened.st_ino)
        or current.st_nlink != 1
        or current.st_size != opened.st_size
        or current.st_mtime_ns != opened.st_mtime_ns
    ):
        raise GateError(f"artifact changed after reading: {path}")
    return ArtifactWitness(
        path,
        relative(repo, path),
        opened.st_size,
        digest.hexdigest(),
        opened.st_dev,
        opened.st_ino,
        opened.st_mtime_ns,
    )


def verify_artifact_unchanged(repo: Path, expected: ArtifactWitness) -> None:
    try:
        current = capture_artifact(repo, expected.path)
    except GateError as exc:
        raise GateError(
            f"artifact changed during evidence generation: {expected.path}"
        ) from exc
    if current != expected:
        raise GateError(f"artifact changed during evidence generation: {expected.path}")


def verify_go_evidence_unchanged(repo: Path, expected: ArtifactWitness) -> None:
    try:
        current = capture_artifact(repo, expected.path)
    except GateError as exc:
        raise GateError(
            f"Go proxy evidence changed during evidence generation: {expected.path}"
        ) from exc
    if current != expected:
        raise GateError(
            f"Go proxy evidence changed during evidence generation: {expected.path}"
        )


def copy_witness_to(repo: Path, expected: ArtifactWitness, destination: Path) -> None:
    """Copy exactly the descriptor-pinned bytes represented by a witness."""
    source_flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    destination_flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_NOFOLLOW", 0)
    )
    try:
        source = os.open(expected.path, source_flags)
    except OSError as exc:
        raise GateError(
            f"artifact cannot be safely reopened: {expected.path}"
        ) from exc
    try:
        opened = os.fstat(source)
        if (
            opened.st_dev != expected.device
            or opened.st_ino != expected.inode
            or opened.st_size != expected.size
            or opened.st_mtime_ns != expected.mtime_ns
            or opened.st_nlink != 1
        ):
            raise GateError(
                f"artifact changed before staging: {expected.path}"
            )
        try:
            target = os.open(destination, destination_flags, 0o644)
        except OSError as exc:
            raise GateError(
                f"cannot create staged artifact: {destination}"
            ) from exc
        digest = hashlib.sha256()
        copied = 0
        try:
            while True:
                block = os.read(source, 1024 * 1024)
                if not block:
                    break
                digest.update(block)
                copied += len(block)
                remaining = memoryview(block)
                while remaining:
                    written = os.write(target, remaining)
                    if written <= 0:
                        raise GateError(
                            f"short write while staging artifact: {destination}"
                        )
                    remaining = remaining[written:]
            os.fchmod(target, 0o644)
            os.fsync(target)
        finally:
            os.close(target)
        closed_over = os.fstat(source)
        if (
            closed_over.st_size != opened.st_size
            or closed_over.st_mtime_ns != opened.st_mtime_ns
            or closed_over.st_ctime_ns != opened.st_ctime_ns
        ):
            raise GateError(
                f"artifact changed while staging: {expected.path}"
            )
        if copied != expected.size or digest.hexdigest() != expected.digest:
            raise GateError(
                f"artifact no longer matches its witness: {expected.path}"
            )
    finally:
        os.close(source)

    staged = capture_artifact(destination.parent, destination)
    if staged.size != expected.size or staged.digest != expected.digest:
        raise GateError(
            f"staged artifact does not match its witness: {destination}"
        )


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n")


def relative(repo: Path, path: Path) -> str:
    return Path(os.path.relpath(path, repo)).as_posix()


def regular_file(path: Path, label: str) -> None:
    if not path.is_file() or path.is_symlink():
        raise GateError(f"{label} must be a regular, non-symlink file: {path}")


def strict_utf8_document(data: bytes, label: str) -> str:
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise GateError(f"{label} is not UTF-8") from exc
    if "\0" in text or "\r" in text or not text.endswith("\n"):
        raise GateError(f"{label} is not canonical UTF-8 text")
    if "'''" in text or '\"\"\"' in text:
        raise GateError(f"{label} uses unsupported multiline TOML strings")
    return text


def cargo_package_version(repo: Path) -> str:
    manifest = repo / "Cargo.toml"
    witness = capture_artifact(repo, manifest)
    data = witness_bytes(witness, "Cargo manifest", MAX_CARGO_MANIFEST_BYTES)
    text = strict_utf8_document(data, "Cargo manifest")
    in_package = False
    seen_package = False
    version: str | None = None
    for raw_line in text.splitlines():
        stripped = raw_line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        array_table = re.fullmatch(r"\[\[([^\[\]\r\n]+)\]\]\s*(?:#.*)?", stripped)
        table = re.fullmatch(r"\[([^\[\]\r\n]+)\]\s*(?:#.*)?", stripped)
        if array_table is not None or table is not None:
            table_name = (array_table or table).group(1).strip()
            if table_name == "package":
                if seen_package:
                    raise GateError("Cargo manifest has duplicate package tables")
                seen_package = True
                in_package = True
            else:
                in_package = False
            continue
        if not in_package:
            continue
        match = re.fullmatch(
            r'version\s*=\s*"([^"\\\x00-\x1f]+)"\s*(?:#.*)?',
            stripped,
        )
        if match is not None:
            if version is not None:
                raise GateError("Cargo manifest has duplicate package versions")
            version = match.group(1)
        elif re.match(r"version(?:\s|=)", stripped):
            raise GateError("Cargo manifest package version is not a literal basic string")
    if version is None or not PACKAGE_VERSION_RE.fullmatch(version):
        raise GateError("Cargo package version is not a supported release version")
    return version


def macos_versions(version: str) -> tuple[str, str]:
    base, separator, prerelease = version.partition("-")
    if not separator:
        return base, base
    match = re.fullmatch(r"(alpha|beta|rc)\.([0-9]+)", prerelease)
    if match is None:
        raise GateError(f"unsupported macOS prerelease version: {version}")
    suffix = {"alpha": "a", "beta": "b", "rc": "fc"}[match.group(1)]
    return base, f"{base}{suffix}{match.group(2)}"


def debian_version(version: str) -> str:
    if "-" not in version:
        return f"{version}-1"
    base, prerelease = version.split("-", 1)
    return f"{base}~{prerelease}-1"


def require_hub_binary_version(
    repo: Path,
    binary: Path,
    version: str,
    label: str,
    executor: list[str] | None = None,
) -> None:
    result = run([*(executor or []), str(binary), "--version"], repo)
    if result.stdout != f"teslatlas-hub {version}\n" or result.stderr:
        raise GateError(f"{label} version does not match Cargo package version")


def ar_members(data: bytes) -> dict[str, bytes]:
    if not data.startswith(b"!<arch>\n"):
        raise GateError("Debian artifact is not an ar archive")
    offset = 8
    members: dict[str, bytes] = {}
    while offset < len(data):
        if len(data) - offset < 60:
            raise GateError("Debian ar archive has a truncated member header")
        header = data[offset:offset + 60]
        if header[58:60] != b"`\n":
            raise GateError("Debian ar archive has an invalid member header")
        try:
            raw_name = header[:16].decode("ascii").strip()
            raw_size = header[48:58].decode("ascii").strip()
            size = int(raw_size, 10)
        except (UnicodeDecodeError, ValueError) as exc:
            raise GateError("Debian ar archive metadata is invalid") from exc
        if not raw_name.endswith("/") or raw_name.startswith(("/", "#1/")):
            raise GateError("Debian ar archive uses an unsupported member name")
        name = raw_name[:-1]
        if not name or name in members or size < 0:
            raise GateError("Debian ar archive contains invalid duplicate members")
        start = offset + 60
        end = start + size
        if end > len(data):
            raise GateError("Debian ar archive has a truncated member")
        members[name] = data[start:end]
        offset = end + (size % 2)
    if offset != len(data):
        raise GateError("Debian ar archive has invalid trailing data")
    return members


def decoded_tar_bytes(data: bytes, archive_name: str, label: str) -> bytes:
    if not archive_name.endswith(".zst"):
        return data
    try:
        from compression import zstd  # type: ignore[attr-defined]
    except ImportError:
        zstd_binary = shutil.which("zstd")
        if zstd_binary is None:
            raise GateError(f"{label} uses zstd but no bounded decoder is available")
        with tempfile.TemporaryDirectory(prefix="teslatlas-deb-zstd-") as raw_directory:
            source = Path(raw_directory) / "archive.tar.zst"
            source.write_bytes(data)
            try:
                process = subprocess.Popen(
                    [zstd_binary, "-q", "-d", "--stdout", str(source)],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                )
            except OSError as exc:
                raise GateError(f"{label} zstd decoder could not start") from exc
            assert process.stdout is not None
            output = bytearray()
            try:
                while len(output) <= MAX_DEBIAN_TAR_BYTES:
                    block = process.stdout.read(
                        min(1024 * 1024, MAX_DEBIAN_TAR_BYTES + 1 - len(output))
                    )
                    if not block:
                        break
                    output.extend(block)
                if len(output) > MAX_DEBIAN_TAR_BYTES:
                    process.kill()
                    process.wait()
                    raise GateError(f"{label} expands beyond the safety limit")
                if process.wait() != 0:
                    raise GateError(f"{label} zstd decompression failed")
            finally:
                process.stdout.close()
            return bytes(output)
    try:
        with zstd.open(io.BytesIO(data), "rb") as source:
            output = source.read(MAX_DEBIAN_TAR_BYTES + 1)
    except (OSError, EOFError) as exc:
        raise GateError(f"{label} zstd decompression failed") from exc
    if len(output) > MAX_DEBIAN_TAR_BYTES:
        raise GateError(f"{label} expands beyond the safety limit")
    return output


def tar_regular_members(data: bytes, label: str, archive_name: str) -> dict[str, bytes]:
    data = decoded_tar_bytes(data, archive_name, label)
    try:
        archive = tarfile.open(fileobj=io.BytesIO(data), mode="r:*")
    except (tarfile.TarError, OSError) as exc:
        raise GateError(f"{label} is not a readable tar archive") from exc
    values: dict[str, bytes] = {}
    seen: set[str] = set()
    try:
        for member in archive:
            name = member.name.removeprefix("./")
            if not name or name.endswith("/"):
                continue
            if name in seen:
                raise GateError(f"{label} contains duplicate paths")
            seen.add(name)
            if not member.isfile():
                continue
            if name == "control":
                limit = MAX_DEBIAN_CONTROL_BYTES
            elif name == "usr/bin/teslatlas-hub":
                limit = MAX_HUB_BINARY_BYTES
            else:
                limit = MAX_LEGAL_FILE_BYTES
            if member.size < 0 or member.size > limit:
                if name == "control" or name in DEBIAN_LEGAL_FILES or name == "usr/bin/teslatlas-hub":
                    raise GateError(f"{label} contains an oversized required file: {name}")
                continue
            source = archive.extractfile(member)
            if source is None:
                raise GateError(f"{label} cannot read required member: {name}")
            content = source.read(limit + 1)
            if len(content) != member.size:
                raise GateError(f"{label} contains a truncated file: {name}")
            values[name] = content
    except (tarfile.TarError, OSError) as exc:
        raise GateError(f"{label} cannot be read safely") from exc
    finally:
        archive.close()
    return values


def debian_control_fields(data: bytes) -> dict[str, str]:
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise GateError("Debian control file is not UTF-8") from exc
    fields: dict[str, str] = {}
    current: str | None = None
    for line in text.splitlines():
        if line.startswith((" ", "\t")):
            if current is None:
                raise GateError("Debian control file has an orphan continuation")
            fields[current] += "\n" + line[1:]
            continue
        if not line:
            current = None
            continue
        if ":" not in line:
            raise GateError("Debian control file is malformed")
        name, value = line.split(":", 1)
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]*", name) or name in fields:
            raise GateError("Debian control file has an invalid or duplicate field")
        fields[name] = value.strip()
        current = name
    return fields


def validate_elf_architecture(data: bytes, architecture: str) -> None:
    if len(data) < 20 or data[:4] != b"\x7fELF" or data[4:6] != b"\x02\x01":
        raise GateError("Debian Hub payload is not a little-endian ELF64 binary")
    machine = struct.unpack_from("<H", data, 18)[0]
    expected = {"amd64": 62, "arm64": 183}.get(architecture)
    if expected is None or machine != expected:
        raise GateError("Debian package architecture does not match its Hub binary")


def linux_sidecar_lock_rows(data: bytes) -> dict[str, tuple[str, str]]:
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise GateError("reviewed Linux sidecar lock is not UTF-8") from exc
    if "\r" in text or not text.endswith("\n"):
        raise GateError("reviewed Linux sidecar lock is not canonical text")
    rows: dict[str, tuple[str, str]] = {}
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        match = re.fullmatch(r"(amd64|arm64) ([0-9a-f]{64}) ([0-9a-f]{64})", stripped)
        if match is None or match.group(1) in rows:
            raise GateError("reviewed Linux sidecar lock is invalid")
        rows[match.group(1)] = (match.group(2), match.group(3))
    if set(rows) != {"amd64", "arm64"}:
        raise GateError("reviewed Linux sidecar lock does not cover both architectures")
    return rows


def validate_debian_artifacts(
    repo: Path,
    artifacts: list[ArtifactWitness],
    package_version: str,
    legal_bundle: dict[str, bytes],
    bundle_has_sidecars: bool,
) -> tuple[dict[str, ArtifactWitness], dict[str, dict[str, dict[str, object]]]]:
    debian_artifacts = [item for item in artifacts if item.path.suffix.lower() == ".deb"]
    seen_architectures: set[str] = set()
    packages_by_architecture: dict[str, ArtifactWitness] = {}
    sidecars_by_architecture: dict[str, dict[str, dict[str, object]]] = {}
    for artifact in debian_artifacts:
        if artifact.size <= 0 or artifact.size > MAX_DEBIAN_ARTIFACT_BYTES:
            raise GateError("Debian artifact size is invalid")
        try:
            data = artifact.path.read_bytes()
        except OSError as exc:
            raise GateError("Debian artifact cannot be read") from exc
        if len(data) != artifact.size or hashlib.sha256(data).hexdigest() != artifact.digest:
            raise GateError("Debian artifact changed before validation")
        members = ar_members(data)
        if members.get("debian-binary") != b"2.0\n":
            raise GateError("Debian artifact has an unsupported format version")
        control_names = [name for name in members if name.startswith("control.tar.")]
        data_names = [name for name in members if name.startswith("data.tar.")]
        if len(control_names) != 1 or len(data_names) != 1:
            raise GateError("Debian artifact must contain one control and one data archive")
        control_files = tar_regular_members(
            members[control_names[0]], "Debian control archive", control_names[0]
        )
        if "control" not in control_files:
            raise GateError("Debian package control file is missing")
        fields = debian_control_fields(control_files["control"])
        if fields.get("Package") != "teslatlas-hub":
            raise GateError("Debian package name is not teslatlas-hub")
        if fields.get("Version") != debian_version(package_version):
            raise GateError("Debian package version does not match Cargo package version")
        architecture = fields.get("Architecture", "")
        if architecture not in {"amd64", "arm64"} or architecture in seen_architectures:
            raise GateError("Debian package architecture is invalid or duplicated")
        seen_architectures.add(architecture)
        packages_by_architecture[architecture] = artifact
        payload = tar_regular_members(
            members[data_names[0]], "Debian data archive", data_names[0]
        )
        hub = payload.get("usr/bin/teslatlas-hub")
        if hub is None:
            raise GateError("Debian package has no Hub binary")
        validate_elf_architecture(hub, architecture)
        proxy_present = "usr/lib/teslatlas-hub/tesla-http-proxy" in payload
        fleet_present = "usr/lib/teslatlas-hub/fleet-telemetry" in payload
        if proxy_present != fleet_present or proxy_present != bundle_has_sidecars:
            raise GateError("Debian sidecar binaries and dependency legal profile differ")
        packaged_sidecar_lock = payload.get(
            "usr/share/doc/teslatlas-hub/SIDECAR_BUILD_LOCK"
        )
        packaged_sidecar_sums = payload.get(
            "usr/share/doc/teslatlas-hub/SIDECAR_SHA256SUMS"
        )
        if proxy_present:
            sidecar_lock_path = repo / "packaging" / "linux" / "sidecar-sha256.lock"
            regular_file(sidecar_lock_path, "reviewed Linux sidecar lock")
            reviewed_lock = sidecar_lock_path.read_bytes()
            if packaged_sidecar_lock != reviewed_lock:
                raise GateError("Debian packaged sidecar build lock differs from tagged source")
            expected_proxy, expected_fleet = linux_sidecar_lock_rows(reviewed_lock)[architecture]
            proxy = payload["usr/lib/teslatlas-hub/tesla-http-proxy"]
            fleet = payload["usr/lib/teslatlas-hub/fleet-telemetry"]
            if hashlib.sha256(proxy).hexdigest() != expected_proxy:
                raise GateError("Debian command proxy does not match its architecture lock row")
            if hashlib.sha256(fleet).hexdigest() != expected_fleet:
                raise GateError("Debian Fleet receiver does not match its architecture lock row")
            expected_sums = (
                f"{expected_proxy}  tesla-http-proxy\n"
                f"{expected_fleet}  fleet-telemetry\n"
            ).encode()
            if packaged_sidecar_sums != expected_sums:
                raise GateError("Debian packaged sidecar checksums do not bind its payloads")
            sidecars_by_architecture[architecture] = {
                "go_proxy": {
                    "name": "tesla-http-proxy",
                    "sha256": expected_proxy,
                    "size": len(proxy),
                },
                "fleet_telemetry": {
                    "name": "fleet-telemetry",
                    "sha256": expected_fleet,
                    "size": len(fleet),
                },
            }
        elif packaged_sidecar_lock is not None or packaged_sidecar_sums is not None:
            raise GateError("Debian package has sidecar receipts without sidecar binaries")
        for packaged_path, source_name in DEBIAN_LEGAL_FILES.items():
            packaged = payload.get(packaged_path)
            source_path = repo / source_name
            regular_file(source_path, f"release legal source {source_name}")
            if packaged is None or packaged != source_path.read_bytes():
                raise GateError(f"Debian package legal payload mismatch: {source_name}")
        packaged_legal = {
            path.removeprefix(DEBIAN_DEPENDENCY_LEGAL_PREFIX): data
            for path, data in payload.items()
            if path.startswith(DEBIAN_DEPENDENCY_LEGAL_PREFIX)
        }
        if packaged_legal != legal_bundle:
            raise GateError("Debian dependency legal bundle mismatch")
    return packages_by_architecture, sidecars_by_architecture


def witness_bytes(expected: ArtifactWitness, label: str, maximum: int) -> bytes:
    if expected.size <= 0 or expected.size > maximum:
        raise GateError(f"{label} size is invalid")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(expected.path, flags)
    except OSError as exc:
        raise GateError(f"{label} cannot be safely opened") from exc
    try:
        opened = os.fstat(descriptor)
        if (
            opened.st_dev != expected.device
            or opened.st_ino != expected.inode
            or opened.st_size != expected.size
            or opened.st_mtime_ns != expected.mtime_ns
        ):
            raise GateError(f"{label} changed before reading")
        data = bytearray()
        digest = hashlib.sha256()
        while len(data) <= maximum:
            block = os.read(descriptor, min(1024 * 1024, maximum + 1 - len(data)))
            if not block:
                break
            data.extend(block)
            digest.update(block)
        after = os.fstat(descriptor)
        if (
            len(data) != expected.size
            or len(data) > maximum
            or digest.hexdigest() != expected.digest
            or after.st_size != opened.st_size
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
            or after.st_nlink != 1
        ):
            raise GateError(f"{label} changed while reading")
    finally:
        os.close(descriptor)
    return bytes(data)


def reject_duplicate_json_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    value: dict[str, object] = {}
    for key, item in pairs:
        if key in value:
            raise GateError(f"Debian attestation receipt has duplicate JSON key: {key}")
        value[key] = item
    return value


def capture_debian_attestations(
    repo: Path,
    directories: list[Path],
    packages_by_architecture: dict[str, ArtifactWitness],
    sidecars_by_architecture: dict[str, dict[str, dict[str, object]]],
) -> list[DebianAttestation]:
    expected_names = {
        DEBIAN_ATTESTATION_RECEIPT_NAME,
        DEBIAN_ATTESTATION_SIGNATURE_NAME,
    }
    if len(directories) != len(packages_by_architecture):
        raise GateError("each Debian package requires exactly one native attestation directory")
    attestations: list[DebianAttestation] = []
    seen_directories: set[Path] = set()
    seen_architectures: set[str] = set()
    for raw_directory in directories:
        directory = Path(os.path.abspath(raw_directory))
        try:
            metadata = os.lstat(directory)
        except OSError as exc:
            raise GateError(f"Debian attestation directory is unavailable: {directory}") from exc
        if not stat.S_ISDIR(metadata.st_mode):
            raise GateError("Debian attestation must be a real, non-symlink directory")
        directory = directory.resolve()
        if directory in seen_directories:
            raise GateError("duplicate Debian attestation directory")
        seen_directories.add(directory)
        try:
            actual_names = {path.name for path in directory.iterdir()}
        except OSError as exc:
            raise GateError("Debian attestation directory cannot be read") from exc
        if actual_names != expected_names:
            raise GateError(
                "Debian attestation directory must contain only receipt and signature"
            )
        receipt = capture_artifact(repo, directory / DEBIAN_ATTESTATION_RECEIPT_NAME)
        signature = capture_artifact(repo, directory / DEBIAN_ATTESTATION_SIGNATURE_NAME)
        if signature.size != 64:
            raise GateError("Debian attestation signature must be 64 bytes")
        receipt_data = witness_bytes(
            receipt, "Debian attestation receipt", MAX_ATTESTATION_RECEIPT_BYTES
        )
        try:
            value = json.loads(
                receipt_data.decode("utf-8"),
                object_pairs_hook=reject_duplicate_json_keys,
            )
            subject = value["subject"]
            architecture = subject["architecture"]
            package_digest = subject["package_sha256"]
            sidecars = subject["sidecars"]
        except (UnicodeDecodeError, json.JSONDecodeError, KeyError, TypeError) as exc:
            raise GateError("Debian attestation receipt is unreadable") from exc
        if value.get("schema") != "teslatlas.debian-native-release-attestation/v1":
            raise GateError("Debian attestation receipt schema is unsupported")
        if architecture not in packages_by_architecture or architecture in seen_architectures:
            raise GateError("Debian attestation architecture is missing, duplicated, or unexpected")
        package = packages_by_architecture[architecture]
        if package_digest != package.digest:
            raise GateError("Debian attestation package digest does not match its captured artifact")
        validate_debian_attestation_sidecars(
            architecture,
            sidecars,
            sidecars_by_architecture.get(architecture),
        )
        seen_architectures.add(architecture)
        attestations.append(DebianAttestation(architecture, package, receipt, signature))
    if seen_architectures != set(packages_by_architecture):
        raise GateError("Debian attestation architecture set is incomplete")
    return sorted(attestations, key=lambda item: item.architecture)


def validate_debian_attestation_sidecars(
    architecture: str,
    value: object,
    packaged: dict[str, dict[str, object]] | None,
) -> None:
    expected_keys = {"go_proxy", "fleet_telemetry"}
    if not isinstance(value, dict) or set(value) != expected_keys:
        raise GateError("Debian attestation sidecar receipt shape is invalid")
    expected_paths = {
        "go_proxy": "usr/lib/teslatlas-hub/tesla-http-proxy",
        "fleet_telemetry": "usr/lib/teslatlas-hub/fleet-telemetry",
    }
    expected_help_prefixes = {
        "go_proxy": "Usage: tesla-http-proxy [OPTION...]\n",
        "fleet_telemetry": "maxprocs: <runtime>\nUsage of fleet-telemetry:\n",
    }
    for key in sorted(expected_keys):
        actual = value[key]
        expected = None if packaged is None else packaged[key]
        if expected is None:
            if actual is not None:
                raise GateError(
                    f"Debian {architecture} attestation claims an absent {key} sidecar"
                )
            continue
        if not isinstance(actual, dict) or set(actual) != {
            "name", "path", "sha256", "size", "help"
        }:
            raise GateError(f"Debian {architecture} attestation {key} receipt is invalid")
        if (
            actual["name"] != expected["name"]
            or actual["path"] != expected_paths[key]
            or actual["sha256"] != expected["sha256"]
            or actual["size"] != expected["size"]
        ):
            raise GateError(
                f"Debian {architecture} attestation {key} does not match its package"
            )
        help_receipt = actual["help"]
        if not isinstance(help_receipt, dict) or set(help_receipt) != {
            "arguments", "exit_code", "stdout", "stderr"
        }:
            raise GateError(
                f"Debian {architecture} attestation {key} help receipt is invalid"
            )
        stderr = help_receipt["stderr"]
        if (
            help_receipt["arguments"] != ["--help"]
            or isinstance(help_receipt["exit_code"], bool)
            or help_receipt["exit_code"] != 0
            or help_receipt["stdout"] != ""
            or not isinstance(stderr, str)
            or not stderr.startswith(expected_help_prefixes[key])
            or not stderr.endswith("\n")
            or len(stderr.encode("utf-8")) > 64 * 1024
            or "\0" in stderr
            or "\r" in stderr
        ):
            raise GateError(
                f"Debian {architecture} attestation {key} help behavior is invalid"
            )


def verify_debian_attestations(
    repo: Path,
    tag: str,
    tag_signer_fingerprint: str,
    attestations: list[DebianAttestation],
    public_key: ArtifactWitness,
    public_key_sha256: str,
) -> None:
    helper = repo / "scripts" / "debian-release-attestation.py"
    regular_file(helper, "Debian native attestation verifier")
    for attestation in attestations:
        run(
            [
                sys.executable,
                str(helper),
                "verify",
                "--repo",
                str(repo),
                "--tag",
                tag,
                "--tag-signer-fingerprint",
                tag_signer_fingerprint,
                "--package",
                str(attestation.package.path),
                "--architecture",
                attestation.architecture,
                "--receipt",
                str(attestation.receipt.path),
                "--signature",
                str(attestation.signature.path),
                "--public-key",
                str(public_key.path),
                "--public-key-sha256",
                public_key_sha256,
            ],
            repo,
        )
        verify_artifact_unchanged(repo, attestation.package)
        verify_artifact_unchanged(repo, attestation.receipt)
        verify_artifact_unchanged(repo, attestation.signature)
        verify_artifact_unchanged(repo, public_key)


def verify_debian_attestation_structure(attestation: DebianAttestation) -> None:
    expected_names = {
        DEBIAN_ATTESTATION_RECEIPT_NAME,
        DEBIAN_ATTESTATION_SIGNATURE_NAME,
    }
    directory = attestation.receipt.path.parent
    try:
        metadata = os.lstat(directory)
        actual_names = {path.name for path in directory.iterdir()}
    except OSError as exc:
        raise GateError("Debian attestation directory changed during evidence generation") from exc
    if not stat.S_ISDIR(metadata.st_mode) or actual_names != expected_names:
        raise GateError("Debian attestation directory changed during evidence generation")


def private_signing_key(path: Path) -> None:
    regular_file(path, "signing key")
    metadata = path.stat()
    if metadata.st_uid != os.geteuid() or metadata.st_mode & 0o077:
        raise GateError("signing key must be owned by the current user and mode 0600 or stricter")


def requires_external_proxy_notice(path: Path) -> bool:
    name = path.name.lower()
    return path.suffix.lower() in {".pkg", ".zip"} or "macos" in name


def architecture_evidence_directories(
    values: list[str], label: str
) -> dict[str, Path]:
    result: dict[str, Path] = {}
    for value in values:
        architecture, separator, raw_path = value.partition("=")
        if separator != "=" or architecture not in {"amd64", "arm64"} or not raw_path:
            raise GateError(f"{label} must use ARCH=DIR with amd64 or arm64")
        if architecture in result:
            raise GateError(f"{label} duplicates architecture {architecture}")
        result[architecture] = Path(os.path.abspath(raw_path))
    return result


def component_manifest(directory: Path, name: str, label: str) -> dict[str, object]:
    try:
        data = (directory / name).read_bytes()
        value = json.loads(data, object_pairs_hook=reject_duplicate_json_keys)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GateError(f"{label} manifest is unreadable") from exc
    if not isinstance(value, dict):
        raise GateError(f"{label} manifest must be a JSON object")
    return value


def captured_component_manifest(
    witnesses: list[ArtifactWitness], name: str, label: str
) -> dict[str, object]:
    matches = [witness for witness in witnesses if witness.path.name == name]
    if len(matches) != 1:
        raise GateError(f"{label} manifest witness is missing or duplicated")
    data = witness_bytes(matches[0], f"{label} manifest", MAX_LEGAL_FILE_BYTES)
    try:
        value = json.loads(data.decode("utf-8"), object_pairs_hook=reject_duplicate_json_keys)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GateError(f"{label} manifest is unreadable") from exc
    if not isinstance(value, dict):
        raise GateError(f"{label} manifest must be a JSON object")
    return value


def validate_linux_sidecar_evidence_subjects(
    architecture: str,
    go_manifest: dict[str, object],
    fleet_manifest: dict[str, object],
    packaged: dict[str, dict[str, object]],
) -> None:
    expected_target = f"linux-{architecture}"
    go_subject = go_manifest.get("subject")
    fleet_subject = fleet_manifest.get("subject")
    if go_manifest.get("target") != expected_target or go_subject != packaged["go_proxy"]:
        raise GateError(
            f"Linux Go proxy evidence does not match the {architecture} Debian payload"
        )
    if not isinstance(fleet_subject, dict):
        raise GateError("Linux Fleet Telemetry evidence subject is invalid")
    fleet_without_target = {
        key: value for key, value in fleet_subject.items() if key != "target"
    }
    if (
        fleet_subject.get("target") != expected_target
        or fleet_without_target != packaged["fleet_telemetry"]
    ):
        raise GateError(
            f"Linux Fleet Telemetry evidence does not match the {architecture} Debian payload"
        )


def require_linux_evidence_coverage(
    required: set[str], go_directories: dict[str, Path], fleet_directories: dict[str, Path]
) -> None:
    if set(go_directories) != required:
        raise GateError(
            "Linux Go proxy evidence must exactly cover sidecar-bearing Debian architectures"
        )
    if set(fleet_directories) != required:
        raise GateError(
            "Linux Fleet Telemetry evidence must exactly cover sidecar-bearing Debian architectures"
        )


def capture_go_evidence(repo: Path, directory: Path) -> list[ArtifactWitness]:
    if not directory.is_dir() or directory.is_symlink():
        raise GateError("Go proxy evidence must be a real directory")
    helper = repo / "scripts" / "go-proxy-evidence.py"
    regular_file(helper, "Go proxy evidence verifier")
    run(
        [sys.executable, str(helper), "--repo", str(repo), "--verify-dir", str(directory)],
        repo,
    )
    actual = {path.name for path in directory.iterdir()}
    if actual != set(GO_EVIDENCE_NAMES):
        raise GateError("Go proxy evidence file set changed after validation")
    witnesses = [capture_artifact(repo, directory / name) for name in GO_EVIDENCE_NAMES]
    run(
        [sys.executable, str(helper), "--repo", str(repo), "--verify-dir", str(directory)],
        repo,
    )
    for witness in witnesses:
        verify_go_evidence_unchanged(repo, witness)
    return witnesses


def capture_rust_source_evidence(repo: Path, directory: Path) -> list[ArtifactWitness]:
    if not directory.is_dir() or directory.is_symlink():
        raise GateError("Rust source evidence must be a real directory")
    actual = {path.name for path in directory.iterdir()}
    if actual != set(RUST_SOURCE_EVIDENCE_NAMES):
        raise GateError("Rust source evidence file set is incomplete or unexpected")
    witnesses = [
        capture_artifact(repo, directory / name) for name in RUST_SOURCE_EVIDENCE_NAMES
    ]
    helper = repo / "scripts" / "rust-source-evidence.py"
    regular_file(helper, "Rust source evidence verifier")
    run(
        [sys.executable, str(helper), "--repo", str(repo), "--verify-dir", str(directory)],
        repo,
    )
    for witness in witnesses:
        verify_artifact_unchanged(repo, witness)
    return witnesses


def capture_fleet_telemetry_evidence(repo: Path, directory: Path) -> list[ArtifactWitness]:
    if not directory.is_dir() or directory.is_symlink():
        raise GateError("Fleet Telemetry evidence must be a real directory")
    helper = repo / "scripts" / "fleet-telemetry-evidence.py"
    regular_file(helper, "Fleet Telemetry evidence verifier")
    run([sys.executable, str(helper), "--repo", str(repo), "--verify-dir", str(directory)], repo)
    if {path.name for path in directory.iterdir()} != set(FLEET_TELEMETRY_EVIDENCE_NAMES):
        raise GateError("Fleet Telemetry evidence file set changed after validation")
    witnesses = [capture_artifact(repo, directory / name) for name in FLEET_TELEMETRY_EVIDENCE_NAMES]
    run([sys.executable, str(helper), "--repo", str(repo), "--verify-dir", str(directory)], repo)
    for witness in witnesses:
        verify_artifact_unchanged(repo, witness)
    return witnesses


def capture_legal_bundle(
    repo: Path,
    directory: Path,
    go_evidence: Path | None,
    fleet_telemetry_evidence: Path | None,
) -> list[ArtifactWitness]:
    if not directory.is_dir() or directory.is_symlink():
        raise GateError("dependency legal bundle must be a real directory")
    if (go_evidence is None) != (fleet_telemetry_evidence is None):
        raise GateError("dependency legal bundle requires paired sidecar evidence")
    helper = repo / "scripts" / "legal-bundle.py"
    regular_file(helper, "dependency legal bundle verifier")
    command = [sys.executable, str(helper), "--repo", str(repo), "--verify-dir", str(directory)]
    names = list(LEGAL_BUNDLE_BASE_NAMES)
    if go_evidence is not None and fleet_telemetry_evidence is not None:
        command.extend([
            "--go-proxy-evidence", str(go_evidence),
            "--fleet-telemetry-evidence", str(fleet_telemetry_evidence),
        ])
        names.extend(LEGAL_BUNDLE_SIDECAR_NAMES)
    run(command, repo)
    if {path.name for path in directory.iterdir()} != set(names):
        raise GateError("dependency legal bundle file set changed after validation")
    witnesses = [capture_artifact(repo, directory / name) for name in names]
    run(command, repo)
    for witness in witnesses:
        verify_artifact_unchanged(repo, witness)
    return witnesses


def capture_macos_release_bundle(
    repo: Path,
    artifacts: list[ArtifactWitness],
    go_evidence: Path,
    go_witnesses: list[ArtifactWitness],
    fleet_telemetry_evidence: Path,
    fleet_telemetry_witnesses: list[ArtifactWitness],
    legal_bundle: Path,
    legal_bundle_witnesses: list[ArtifactWitness],
) -> tuple[Path, list[ArtifactWitness]] | None:
    try:
        go_evidence.relative_to(repo)
        fleet_telemetry_evidence.relative_to(repo)
        legal_bundle.relative_to(repo)
    except ValueError:
        return None
    zip_artifacts = [item for item in artifacts if item.path.suffix.lower() == ".zip"]
    package_artifacts = [item for item in artifacts if item.path.suffix.lower() == ".pkg"]
    if len(zip_artifacts) != 1 or len(package_artifacts) != 1:
        return None
    bundle = zip_artifacts[0].path.parent
    if package_artifacts[0].path.parent != bundle or go_evidence.parent != bundle \
            or fleet_telemetry_evidence.parent != bundle or legal_bundle.parent != bundle:
        return None
    logs = bundle / "notary-logs"
    checksums = bundle / "SHA256SUMS"
    expected_top_level = {
        zip_artifacts[0].path.name,
        package_artifacts[0].path.name,
        go_evidence.name,
        fleet_telemetry_evidence.name,
        legal_bundle.name,
        logs.name,
        checksums.name,
    }
    if {path.name for path in bundle.iterdir()} != expected_top_level:
        raise GateError("macOS release bundle contains unexpected or missing sidecars")
    for directory, label in (
        (bundle, "macOS release bundle"),
        (go_evidence, "Go proxy evidence"),
        (fleet_telemetry_evidence, "Fleet Telemetry evidence"),
        (legal_bundle, "dependency legal bundle"),
        (logs, "macOS notary logs"),
    ):
        if not directory.is_dir() or directory.is_symlink():
            raise GateError(f"{label} must be a real directory")
    regular_file(checksums, "macOS release checksums")
    expected_log_names = {
        "app-log.json",
        "app-submit.json",
        "service-package-log.json",
        "service-package-submit.json",
    }
    if {path.name for path in logs.iterdir()} != expected_log_names:
        raise GateError("macOS notary log set is incomplete")
    checksum_witness = capture_artifact(repo, checksums)
    log_witnesses = [capture_artifact(repo, logs / name) for name in sorted(expected_log_names)]
    if checksum_witness.size > 64 * 1024 or any(
        witness.size > 8 * 1024 * 1024 for witness in log_witnesses
    ):
        raise GateError("macOS release receipt is unexpectedly large")
    expected_checksum_paths = {
        zip_artifacts[0].path.name,
        package_artifacts[0].path.name,
        *(f"{go_evidence.name}/{name}" for name in GO_EVIDENCE_NAMES),
        *(f"{fleet_telemetry_evidence.name}/{name}" for name in FLEET_TELEMETRY_EVIDENCE_NAMES),
        *(f"{legal_bundle.name}/{name}" for name in (*LEGAL_BUNDLE_BASE_NAMES, *LEGAL_BUNDLE_SIDECAR_NAMES)),
    }
    expected_digests = {
        zip_artifacts[0].path.name: zip_artifacts[0].digest,
        package_artifacts[0].path.name: package_artifacts[0].digest,
        **{
            f"{go_evidence.name}/{witness.path.name}": witness.digest
            for witness in go_witnesses
        },
        **{
            f"{fleet_telemetry_evidence.name}/{witness.path.name}": witness.digest
            for witness in fleet_telemetry_witnesses
        },
        **{
            f"{legal_bundle.name}/{witness.path.name}": witness.digest
            for witness in legal_bundle_witnesses
        },
    }
    checksum_records: dict[str, str] = {}
    receipts = [checksum_witness, *log_witnesses]
    with tempfile.TemporaryDirectory(prefix="teslatlas-release-receipts-") as raw_stage:
        receipt_stage = Path(raw_stage)
        for witness in receipts:
            destination = receipt_stage / witness.path.name
            copy_witness_to(repo, witness, destination)
            if witness in log_witnesses:
                try:
                    json.loads(destination.read_text())
                except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
                    raise GateError("macOS notary log is invalid JSON") from exc
        validate_notary_receipts(
            receipt_stage,
            f"{zip_artifacts[0].path.stem}-submission.zip",
            package_artifacts[0].path.name,
        )
        try:
            checksum_lines = (receipt_stage / checksums.name).read_text().splitlines()
        except (OSError, UnicodeDecodeError) as exc:
            raise GateError("macOS release checksums are unreadable") from exc
        for line in checksum_lines:
            match = re.fullmatch(r"([0-9a-f]{64})  ([^\r\n]+)", line)
            if match is None or match.group(2) in checksum_records:
                raise GateError("macOS release checksums are invalid")
            checksum_records[match.group(2)] = match.group(1)
    if set(checksum_records) != expected_checksum_paths:
        raise GateError("macOS release checksums do not cover the exact bundle")
    if checksum_records != expected_digests:
        raise GateError("macOS release checksum mismatch")
    for witness in receipts:
        verify_artifact_unchanged(repo, witness)
    return bundle, receipts


def validate_notary_receipts(
    directory: Path, app_archive_name: str, package_archive_name: str
) -> None:
    for label, archive_name in (
        ("app", app_archive_name),
        ("service-package", package_archive_name),
    ):
        try:
            submit = json.loads((directory / f"{label}-submit.json").read_text())
            detail = json.loads((directory / f"{label}-log.json").read_text())
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            raise GateError(f"macOS {label} notary receipts are unreadable") from exc
        if not isinstance(submit, dict) or not isinstance(detail, dict):
            raise GateError(f"macOS {label} notary receipts have invalid schemas")
        submission_id = submit.get("id")
        if (
            not isinstance(submission_id, str)
            or UUID_RE.fullmatch(submission_id) is None
            or submit.get("status") != "Accepted"
            or not isinstance(submit.get("message"), str)
            or not submit["message"]
        ):
            raise GateError(f"macOS {label} notary submission receipt is invalid")
        ticket_contents = detail.get("ticketContents")
        if (
            detail.get("logFormatVersion") != 1
            or detail.get("jobId") != submission_id
            or detail.get("status") != "Accepted"
            or detail.get("statusCode") != 0
            or detail.get("archiveFilename") != archive_name
            or not isinstance(detail.get("statusSummary"), str)
            or not detail["statusSummary"]
            or not isinstance(detail.get("sha256"), str)
            or re.fullmatch(r"[0-9A-Fa-f]{64}", detail["sha256"]) is None
            or detail.get("issues") not in (None, [])
            or not isinstance(ticket_contents, list)
            or not ticket_contents
        ):
            raise GateError(f"macOS {label} notary detail receipt is invalid")
        for ticket in ticket_contents:
            if (
                not isinstance(ticket, dict)
                or not isinstance(ticket.get("path"), str)
                or not ticket["path"]
                or len(ticket["path"]) > 4096
                or ticket.get("digestAlgorithm") != "SHA-256"
                or not isinstance(ticket.get("cdhash"), str)
                or re.fullmatch(r"[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64}", ticket["cdhash"])
                is None
            ):
                raise GateError(f"macOS {label} notary ticket content is invalid")


def verify_macos_release_bundle_structure(
    bundle: Path,
    artifacts: list[ArtifactWitness],
    go_evidence: Path,
    fleet_telemetry_evidence: Path,
    legal_bundle: Path,
    receipts: list[ArtifactWitness],
) -> None:
    zip_artifact = next(item for item in artifacts if item.path.suffix.lower() == ".zip")
    package_artifact = next(
        item for item in artifacts if item.path.suffix.lower() == ".pkg"
    )
    logs = bundle / "notary-logs"
    expected_top_level = {
        zip_artifact.path.name,
        package_artifact.path.name,
        go_evidence.name,
        fleet_telemetry_evidence.name,
        legal_bundle.name,
        logs.name,
        "SHA256SUMS",
    }
    for directory, label in (
        (bundle, "macOS release bundle"),
        (go_evidence, "Go proxy evidence"),
        (fleet_telemetry_evidence, "Fleet Telemetry evidence"),
        (legal_bundle, "dependency legal bundle"),
        (logs, "macOS notary logs"),
    ):
        if not directory.is_dir() or directory.is_symlink():
            raise GateError(f"{label} changed during evidence generation")
    if {path.name for path in bundle.iterdir()} != expected_top_level:
        raise GateError("macOS release bundle changed during evidence generation")
    if {path.name for path in go_evidence.iterdir()} != set(GO_EVIDENCE_NAMES):
        raise GateError("Go proxy evidence file set changed during evidence generation")
    if {path.name for path in fleet_telemetry_evidence.iterdir()} != set(FLEET_TELEMETRY_EVIDENCE_NAMES):
        raise GateError("Fleet Telemetry evidence file set changed during evidence generation")
    if {path.name for path in legal_bundle.iterdir()} != set(
        (*LEGAL_BUNDLE_BASE_NAMES, *LEGAL_BUNDLE_SIDECAR_NAMES)
    ):
        raise GateError("dependency legal bundle file set changed during evidence generation")
    expected_logs = {
        witness.path.name
        for witness in receipts
        if witness.path.parent.name == "notary-logs"
    }
    if {path.name for path in logs.iterdir()} != expected_logs:
        raise GateError("macOS notary log set changed during evidence generation")


def extract_checked_macos_app(archive_path: Path, destination: Path) -> Path:
    member_limit = 20_000
    expanded_limit = 2 * 1024 * 1024 * 1024
    seen: set[str] = set()
    payload_members: list[PurePosixPath] = []
    expanded = 0
    try:
        archive = zipfile.ZipFile(archive_path)
    except (OSError, zipfile.BadZipFile) as exc:
        raise GateError("macOS release artifact is not a valid ZIP archive") from exc
    with archive:
        members = archive.infolist()
        if not members or len(members) > member_limit:
            raise GateError("macOS release ZIP member count is invalid")
        for member in members:
            name = member.filename
            if not name or "\\" in name or member.flag_bits & 0x1:
                raise GateError("macOS release ZIP contains an unsafe member")
            relative_path = PurePosixPath(name)
            if relative_path.is_absolute() or any(
                part in {"", ".", ".."} for part in relative_path.parts
            ):
                raise GateError("macOS release ZIP contains an unsafe path")
            normalized = relative_path.as_posix().rstrip("/")
            if normalized in seen:
                raise GateError("macOS release ZIP contains duplicate members")
            seen.add(normalized)
            expanded += member.file_size
            if expanded > expanded_limit:
                raise GateError("macOS release ZIP expands beyond the safety limit")
            if relative_path.parts[0] == "__MACOSX":
                continue
            payload_members.append(relative_path)
            mode = (member.external_attr >> 16) & 0xFFFF
            file_kind = stat.S_IFMT(mode)
            is_directory = member.is_dir()
            if is_directory:
                if file_kind not in {0, stat.S_IFDIR}:
                    raise GateError("macOS release ZIP contains an unsafe directory")
            elif file_kind not in {0, stat.S_IFREG}:
                raise GateError("macOS release ZIP contains a non-regular member")
            target = destination.joinpath(*relative_path.parts)
            target.resolve().relative_to(destination.resolve())
            if is_directory:
                target.mkdir(parents=True, exist_ok=True)
                target.chmod((mode & 0o777) or 0o755)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            try:
                source = archive.open(member)
                with source, target.open("xb") as output:
                    shutil.copyfileobj(source, output, 1024 * 1024)
            except (OSError, RuntimeError, zipfile.BadZipFile) as exc:
                raise GateError("macOS release ZIP member cannot be extracted safely") from exc
            if target.stat().st_size != member.file_size:
                raise GateError("macOS release ZIP member has a short read")
            target.chmod((mode & 0o777) or 0o644)

    app_roots = sorted(
        {
            path.parents[1]
            for path in destination.glob("*.app/Contents/Info.plist")
            if path.is_file() and not path.is_symlink()
        }
    )
    if len(app_roots) != 1:
        raise GateError("macOS release ZIP must contain exactly one app")
    app_name = app_roots[0].name
    if any(member.parts[0] != app_name for member in payload_members):
        raise GateError("macOS release ZIP contains content outside the signed app")
    return app_roots[0]


def signed_team(
    details: str,
    authority: str,
    label: str,
) -> str:
    if f"Authority={authority}:" not in details:
        raise GateError(f"{label} is not signed by {authority}")
    match = re.search(r"^TeamIdentifier=([A-Z0-9]{10})$", details, re.MULTILINE)
    if match is None:
        raise GateError(f"{label} has no valid Team ID")
    return match.group(1)


def checked_package_payload_file(root: Path, relative_path: PurePosixPath) -> Path:
    current = root
    for part in relative_path.parts[:-1]:
        current /= part
        try:
            metadata = os.lstat(current)
        except OSError as exc:
            raise GateError("macOS package payload path is missing") from exc
        if not stat.S_ISDIR(metadata.st_mode):
            raise GateError("macOS package payload path is unsafe")
    target = current / relative_path.parts[-1]
    regular_file(target, "macOS package Tesla proxy")
    return target


def canonical_macho_digest(path: Path) -> str:
    try:
        data = bytearray(path.read_bytes())
    except OSError as exc:
        raise GateError("signed Tesla proxy cannot be read") from exc
    if len(data) < 32 or len(data) > 128 * 1024 * 1024:
        raise GateError("signed Tesla proxy size is invalid")
    try:
        header = struct.unpack_from("<8I", data, 0)
    except struct.error as exc:
        raise GateError("signed Tesla proxy is not a thin 64-bit Mach-O") from exc
    if header[0] != 0xFEEDFACF:
        raise GateError("signed Tesla proxy is not a thin 64-bit Mach-O")
    command_count = header[4]
    command_bytes = header[5]
    if command_count > 4_096 or command_bytes > len(data) - 32:
        raise GateError("signed Tesla proxy Mach-O commands are invalid")
    offset = 32
    command_end = offset + command_bytes
    linkedit_count = 0
    for _ in range(command_count):
        if offset + 8 > command_end:
            raise GateError("signed Tesla proxy Mach-O commands are truncated")
        command, command_size = struct.unpack_from("<II", data, offset)
        if command_size < 8 or offset + command_size > command_end:
            raise GateError("signed Tesla proxy Mach-O command size is invalid")
        if command == 0x19:
            if command_size < 72:
                raise GateError("signed Tesla proxy segment command is invalid")
            segment = bytes(data[offset + 8 : offset + 24]).split(b"\0", 1)[0]
            if segment == b"__LINKEDIT":
                linkedit_count += 1
                # codesign changes only this rounded virtual size after its
                # embedded signature is removed. Normalize that one field;
                # every executable byte and remaining load-command byte stays
                # bound to the reviewed linker-signed proxy.
                struct.pack_into("<Q", data, offset + 32, 0)
        offset += command_size
    if offset != command_end or linkedit_count != 1:
        raise GateError("signed Tesla proxy Mach-O layout is invalid")
    return hashlib.sha256(data).hexdigest()


def validate_dependency_legal_directory(
    directory: Path, expected: dict[str, bytes], label: str
) -> None:
    try:
        metadata = os.lstat(directory)
    except OSError as exc:
        raise GateError(f"{label} is missing") from exc
    if not stat.S_ISDIR(metadata.st_mode):
        raise GateError(f"{label} is unsafe")
    try:
        actual_names = {entry.name for entry in directory.iterdir()}
    except OSError as exc:
        raise GateError(f"{label} cannot be listed") from exc
    if actual_names != set(expected):
        missing = sorted(set(expected) - actual_names)
        unexpected = sorted(actual_names - set(expected))
        raise GateError(
            f"{label} file set mismatch; missing={missing}; unexpected={unexpected}"
        )
    for name, expected_data in sorted(expected.items()):
        path = directory / name
        regular_file(path, f"{label} component {name}")
        if path.stat().st_size > MAX_LEGAL_FILE_BYTES or path.read_bytes() != expected_data:
            raise GateError(f"{label} component mismatch: {name}")


def unsigned_code_digest(repo: Path, source: Path, destination: Path) -> str:
    witness = capture_artifact(source.parent, source)
    copy_witness_to(repo, witness, destination)
    run(["codesign", "--remove-signature", str(destination)], repo)
    regular_file(destination, "signature-stripped Tesla proxy")
    return canonical_macho_digest(destination)


def validate_macos_artifacts(
    repo: Path,
    artifacts: list[ArtifactWitness],
    package_version: str,
    go_manifest: dict,
    go_manifest_digest: str,
    fleet_telemetry_manifest: dict,
    fleet_telemetry_manifest_digest: str,
    legal_bundle: dict[str, bytes],
    legal_bundle_manifest_digest: str,
    stage: Path,
) -> None:
    marketing_version, bundle_version = macos_versions(package_version)
    macos = [item for item in artifacts if requires_external_proxy_notice(item.path)]
    zip_artifacts = [item for item in macos if item.path.suffix.lower() == ".zip"]
    package_artifacts = [item for item in macos if item.path.suffix.lower() == ".pkg"]
    if len(zip_artifacts) != 1 or len(package_artifacts) > 1:
        raise GateError("macOS evidence requires one app ZIP and at most one matching package")
    if len(zip_artifacts) + len(package_artifacts) != len(macos):
        raise GateError("unsupported macOS release artifact type")

    check_root = stage / ".macos-artifact-check"
    check_root.mkdir(mode=0o700)
    try:
        checked_zip = check_root / "release.zip"
        copy_witness_to(repo, zip_artifacts[0], checked_zip)
        extracted = check_root / "extracted"
        extracted.mkdir(mode=0o700)
        app = extract_checked_macos_app(checked_zip, extracted)
        run(["codesign", "--verify", "--deep", "--strict", str(app)], repo)
        run(["spctl", "--assess", "--type", "execute", "--verbose=4", str(app)], repo)
        run(["xcrun", "stapler", "validate", str(app)], repo)
        description = run(["codesign", "-d", "--verbose=4", str(app)], repo)
        signing_details = f"{description.stdout}\n{description.stderr}"
        team = signed_team(
            signing_details,
            "Developer ID Application",
            "macOS release app",
        )

        info_path = app / "Contents" / "Info.plist"
        hub_path = app / "Contents" / "Resources" / "teslatlas-hub"
        proxy_path = app / "Contents" / "Resources" / "tesla-http-proxy"
        fleet_telemetry_path = app / "Contents" / "Resources" / "fleet-telemetry"
        package_path = app / "Contents" / "Resources" / "TeslatlasHubService.pkg"
        for path, label in (
            (info_path, "Info.plist"),
            (hub_path, "Hub binary"),
            (proxy_path, "Tesla proxy"),
            (fleet_telemetry_path, "Fleet Telemetry receiver"),
            (package_path, "service package"),
        ):
            regular_file(path, f"macOS release {label}")
        try:
            info = plistlib.loads(info_path.read_bytes())
        except (OSError, plistlib.InvalidFileException) as exc:
            raise GateError("macOS release Info.plist is invalid") from exc
        if info.get("TeslatlasHubVersion") != package_version:
            raise GateError("macOS release Hub version does not match Cargo package version")
        if info.get("CFBundleShortVersionString") != marketing_version:
            raise GateError("macOS release marketing version does not match Cargo package version")
        if info.get("CFBundleVersion") != bundle_version:
            raise GateError("macOS release bundle version does not match Cargo package version")
        require_hub_binary_version(repo, hub_path, package_version, "macOS app Hub binary")
        resources = app / "Contents" / "Resources"
        validate_dependency_legal_directory(
            resources / MACOS_DEPENDENCY_LEGAL_DIRECTORY,
            legal_bundle,
            "macOS app dependency legal bundle",
        )
        for packaged_name, source_name in MACOS_LEGAL_FILES.items():
            packaged = resources / packaged_name
            source = repo / source_name
            regular_file(packaged, f"macOS app legal payload {packaged_name}")
            regular_file(source, f"release legal source {source_name}")
            if packaged.read_bytes() != source.read_bytes():
                raise GateError(f"macOS app legal payload mismatch: {source_name}")
        subject = go_manifest.get("subject")
        if not isinstance(subject, dict) or not isinstance(subject.get("sha256"), str):
            raise GateError("Go component manifest subject is invalid")
        if go_manifest.get("target") != "darwin-arm64":
            raise GateError("Go component manifest is not for the macOS release target")
        if info.get("TeslatlasOfficialRelease") is not True:
            raise GateError("macOS artifact is not marked as an official release")
        if info.get("TeslatlasReleaseTeamIdentifier") != team:
            raise GateError("macOS release Team ID metadata does not match its signature")
        if info.get("TeslatlasUnsignedProxySHA256") != subject["sha256"]:
            raise GateError("macOS artifact does not bind the locked unsigned proxy")
        if info.get("TeslatlasGoEvidenceManifestSHA256") != go_manifest_digest:
            raise GateError("macOS artifact does not bind the supplied Go evidence")
        fleet_subject = fleet_telemetry_manifest.get("subject")
        if not isinstance(fleet_subject, dict) or not isinstance(fleet_subject.get("sha256"), str):
            raise GateError("Fleet Telemetry component manifest subject is invalid")
        if fleet_subject.get("target") != "darwin-arm64":
            raise GateError(
                "Fleet Telemetry component manifest is not for the macOS release target"
            )
        if info.get("TeslatlasUnsignedFleetTelemetrySHA256") != fleet_subject["sha256"]:
            raise GateError("macOS artifact does not bind the locked Fleet Telemetry receiver")
        if info.get("TeslatlasFleetTelemetryEvidenceManifestSHA256") != fleet_telemetry_manifest_digest:
            raise GateError("macOS artifact does not bind the supplied Fleet Telemetry evidence")
        if info.get("TeslatlasLegalBundleManifestSHA256") != legal_bundle_manifest_digest:
            raise GateError("macOS artifact does not bind the supplied dependency legal bundle")
        embedded_package_digest = sha256(package_path)
        if info.get("TeslatlasServicePackageSHA256") != embedded_package_digest:
            raise GateError("macOS app does not bind its embedded service package")
        if package_artifacts and package_artifacts[0].digest != embedded_package_digest:
            raise GateError("external service package does not match the signed app")

        run(["codesign", "--verify", "--strict", str(proxy_path)], repo)
        proxy_description = run(["codesign", "-d", "--verbose=4", str(proxy_path)], repo)
        proxy_team = signed_team(
            f"{proxy_description.stdout}\n{proxy_description.stderr}",
            "Developer ID Application",
            "macOS app Tesla proxy",
        )
        if proxy_team != team:
            raise GateError("macOS app Tesla proxy Team ID does not match the app")

        unsigned_proxy = stage / "go-proxy-evidence" / "tesla-http-proxy.unsigned"
        regular_file(unsigned_proxy, "Go evidence unsigned Tesla proxy")
        reviewed_code_digest = unsigned_code_digest(
            repo,
            unsigned_proxy,
            check_root / "reviewed-proxy.stripped",
        )
        app_code_digest = unsigned_code_digest(
            repo,
            proxy_path,
            check_root / "app-proxy.stripped",
        )
        if app_code_digest != reviewed_code_digest:
            raise GateError("macOS app Tesla proxy does not match the reviewed unsigned proxy")

        run(["codesign", "--verify", "--strict", str(fleet_telemetry_path)], repo)
        fleet_description = run(["codesign", "-d", "--verbose=4", str(fleet_telemetry_path)], repo)
        fleet_team = signed_team(
            f"{fleet_description.stdout}\n{fleet_description.stderr}",
            "Developer ID Application",
            "macOS app Fleet Telemetry receiver",
        )
        if fleet_team != team:
            raise GateError("macOS app Fleet Telemetry receiver Team ID does not match the app")
        unsigned_receiver = stage / "fleet-telemetry-evidence" / "fleet-telemetry.unsigned"
        regular_file(unsigned_receiver, "Fleet Telemetry evidence unsigned receiver")
        reviewed_receiver_digest = unsigned_code_digest(
            repo, unsigned_receiver, check_root / "reviewed-fleet-telemetry.stripped"
        )
        app_receiver_digest = unsigned_code_digest(
            repo, fleet_telemetry_path, check_root / "app-fleet-telemetry.stripped"
        )
        if app_receiver_digest != reviewed_receiver_digest:
            raise GateError("macOS app Fleet Telemetry receiver does not match the reviewed unsigned receiver")

        package_signature = run(["pkgutil", "--check-signature", str(package_path)], repo)
        package_details = f"{package_signature.stdout}\n{package_signature.stderr}"
        package_team_match = re.search(
            r"Developer ID Installer:.*\(([A-Z0-9]{10})\)",
            package_details,
        )
        if package_team_match is None or package_team_match.group(1) != team:
            raise GateError("macOS service package signature does not match the app Team ID")
        run(["spctl", "--assess", "--type", "install", "--verbose=4", str(package_path)], repo)
        run(["xcrun", "stapler", "validate", str(package_path)], repo)
        expanded_package = check_root / "service-package"
        run(
            ["pkgutil", "--expand-full", str(package_path), str(expanded_package)],
            repo,
        )
        applications_payload = expanded_package / "Payload" / "Applications"
        if os.path.lexists(applications_payload):
            raise GateError("macOS service package unexpectedly contains an application")
        package_info = expanded_package / "PackageInfo"
        regular_file(package_info, "macOS service package metadata")
        try:
            package_info_root = ET.fromstring(package_info.read_bytes())
        except (OSError, ET.ParseError) as exc:
            raise GateError("macOS service package metadata is invalid") from exc
        if (
            package_info_root.tag != "pkg-info"
            or package_info_root.attrib.get("version") != bundle_version
        ):
            raise GateError("macOS service package version does not match Cargo package version")
        if package_info_root.attrib.get("identifier") != "com.teslatlas.hub.service":
            raise GateError("macOS service package identifier is invalid")
        if package_info_root.attrib.get("install-location") != "/":
            raise GateError("macOS service package install location is invalid")
        package_hub = checked_package_payload_file(
            expanded_package,
            PurePosixPath("Payload/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"),
        )
        require_hub_binary_version(
            repo, package_hub, package_version, "macOS package Hub binary"
        )
        package_share = PurePosixPath(
            "Payload/Library/Application Support/Teslatlas Hub/share"
        )
        validate_dependency_legal_directory(
            expanded_package
            / "Payload"
            / "Library"
            / "Application Support"
            / "Teslatlas Hub"
            / "share"
            / "dependency-legal",
            legal_bundle,
            "macOS package dependency legal bundle",
        )
        for packaged_name, source_name in MACOS_LEGAL_FILES.items():
            packaged = checked_package_payload_file(
                expanded_package, package_share / packaged_name
            )
            source = repo / source_name
            regular_file(source, f"release legal source {source_name}")
            if packaged.read_bytes() != source.read_bytes():
                raise GateError(f"macOS package legal payload mismatch: {source_name}")
        package_proxy = checked_package_payload_file(
            expanded_package,
            PurePosixPath(
                "Payload/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy"
            ),
        )
        run(["codesign", "--verify", "--strict", str(package_proxy)], repo)
        package_proxy_description = run(
            ["codesign", "-d", "--verbose=4", str(package_proxy)],
            repo,
        )
        package_proxy_team = signed_team(
            f"{package_proxy_description.stdout}\n{package_proxy_description.stderr}",
            "Developer ID Application",
            "macOS package Tesla proxy",
        )
        if package_proxy_team != team:
            raise GateError("macOS package Tesla proxy Team ID does not match the app")
        package_code_digest = unsigned_code_digest(
            repo,
            package_proxy,
            check_root / "package-proxy.stripped",
        )
        if package_code_digest != reviewed_code_digest:
            raise GateError("macOS package Tesla proxy does not match the reviewed unsigned proxy")
        package_receiver = checked_package_payload_file(
            expanded_package,
            PurePosixPath("Payload/Library/Application Support/Teslatlas Hub/bin/fleet-telemetry"),
        )
        run(["codesign", "--verify", "--strict", str(package_receiver)], repo)
        receiver_description = run(["codesign", "-d", "--verbose=4", str(package_receiver)], repo)
        receiver_team = signed_team(
            f"{receiver_description.stdout}\n{receiver_description.stderr}",
            "Developer ID Application",
            "macOS package Fleet Telemetry receiver",
        )
        if receiver_team != team:
            raise GateError("macOS package Fleet Telemetry receiver Team ID does not match the app")
        package_receiver_digest = unsigned_code_digest(
            repo, package_receiver, check_root / "package-fleet-telemetry.stripped"
        )
        if package_receiver_digest != reviewed_receiver_digest:
            raise GateError("macOS package Fleet Telemetry receiver does not match the reviewed unsigned receiver")
    finally:
        shutil.rmtree(check_root, ignore_errors=True)


def clean_and_tag(
    repo: Path,
    tag: str,
    artifacts: list[Path],
    ignored_paths: list[Path],
    expected_signer: str,
) -> tuple[str, str, str]:
    exclusions = []
    for artifact in artifacts:
        artifact_path = relative(repo, artifact)
        exclusions.append(f":(top,exclude,literal){artifact_path}")
        tracked = subprocess.run(
            ["git", "ls-files", "--error-unmatch", "--", f":(top,literal){artifact_path}"],
            cwd=repo, text=True, capture_output=True,
        )
        if tracked.returncode == 0:
            raise GateError(f"artifact must not be a tracked source file: {artifact}")
    for ignored in ignored_paths:
        try:
            ignored_relative = ignored.relative_to(repo).as_posix()
        except ValueError as exc:
            raise GateError(f"ignored release path must be inside repo: {ignored}") from exc
        if not ignored_relative:
            raise GateError("repository root cannot be an ignored release path")
        tracked = run(
            ["git", "ls-files", "--", f":(top,literal){ignored_relative}"], repo
        ).stdout
        if tracked:
            raise GateError(f"ignored release path contains tracked source files: {ignored}")
        exclusions.append(f":(top,exclude,literal){ignored_relative}")
    status_args = ["git", "status", "--porcelain=v1", "--untracked-files=all", "--", "."] + exclusions
    status = run(status_args, repo).stdout
    if status:
        raise GateError("candidate checkout is not clean")
    commit = run(["git", "rev-parse", f"{tag}^{{commit}}"], repo).stdout.strip()
    head = run(["git", "rev-parse", "HEAD"], repo).stdout.strip()
    if head != commit:
        raise GateError("candidate HEAD does not match the signed tag commit")
    exact = run(["git", "describe", "--exact-match", "--tags", commit], repo).stdout.strip()
    if exact != tag:
        raise GateError(f"tag is not an exact commit tag: {tag}")
    try:
        verification = run(["git", "verify-tag", "--raw", tag], repo)
    except GateError as exc:
        raise GateError(f"tag is not cryptographically verified: {tag}") from exc
    status = f"{verification.stdout}\n{verification.stderr}"
    signers = {
        match.group(1).upper()
        for match in re.finditer(r"^\[GNUPG:\] VALIDSIG ([0-9A-F]+)\b", status, re.MULTILINE)
    }
    if signers != {expected_signer.upper()}:
        raise GateError("tag signer does not match the pinned maintainer fingerprint")
    timestamp = run(["git", "show", "-s", "--format=%cI", commit], repo).stdout.strip()
    try:
        created = datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
    except ValueError as exc:
        raise GateError(f"invalid commit timestamp: {timestamp}") from exc
    return (
        commit,
        created.astimezone(timezone.utc).isoformat().replace("+00:00", "Z"),
        expected_signer.upper(),
    )


def validate_release_signing_key(
    repo: Path, key: ArtifactWitness, expected_signer: str
) -> None:
    result = run(
        ["gpg", "--batch", "--with-colons", "--show-keys", str(key.path)],
        repo,
    )
    primary_fingerprints: list[str] = []
    awaiting_primary_fingerprint = False
    for line in result.stdout.splitlines():
        fields = line.split(":")
        record = fields[0] if fields else ""
        if record in {"sec", "ssb"}:
            raise GateError("release signing key asset must not contain secret key material")
        if record == "pub":
            if awaiting_primary_fingerprint:
                raise GateError("release signing key asset has an incomplete public key")
            awaiting_primary_fingerprint = True
        elif record == "fpr" and awaiting_primary_fingerprint:
            if len(fields) <= 9 or FINGERPRINT_RE.fullmatch(fields[9]) is None:
                raise GateError("release signing key asset has an invalid primary fingerprint")
            primary_fingerprints.append(fields[9].upper())
            awaiting_primary_fingerprint = False
    if awaiting_primary_fingerprint or primary_fingerprints != [expected_signer.upper()]:
        raise GateError("RELEASE_SIGNING_KEY.asc does not match the signed-tag fingerprint")


def assert_candidate_unchanged(
    repo: Path,
    tag: str,
    commit: str,
    artifacts: list[Path],
    ignored_paths: list[Path],
    stage: Path,
) -> None:
    exclusions = [f":(top,exclude,literal){relative(repo, path)}" for path in artifacts]
    exclusions.extend(
        f":(top,exclude,literal){path.relative_to(repo).as_posix()}"
        for path in ignored_paths
    )
    try:
        stage.relative_to(repo)
    except ValueError:
        pass
    else:
        exclusions.append(f":(top,exclude,literal){relative(repo, stage)}")
    status = run(
        ["git", "status", "--porcelain=v1", "--untracked-files=all", "--", "."]
        + exclusions,
        repo,
    ).stdout
    if status:
        raise GateError("candidate checkout changed during evidence generation")
    head = run(["git", "rev-parse", "HEAD"], repo).stdout.strip()
    tag_commit = run(["git", "rev-parse", f"{tag}^{{commit}}"], repo).stdout.strip()
    if head != commit or tag_commit != commit:
        raise GateError("candidate HEAD or signed tag changed during evidence generation")


def publish_evidence_directory(stage: Path, output: Path) -> None:
    if os.path.lexists(output):
        raise GateError(f"output directory already exists: {output}")
    try:
        stage.rename(output)
    except OSError as exc:
        raise GateError(f"cannot atomically publish evidence directory: {output}") from exc


def sign_checksum_manifest(
    repo: Path,
    checksum_path: Path,
    created: str,
    expected_signer: str,
) -> Path:
    signature = checksum_path.with_name("SHA256SUMS.asc")
    try:
        timestamp = int(datetime.fromisoformat(created.replace("Z", "+00:00")).timestamp())
    except ValueError as exc:
        raise GateError("release creation time cannot drive checksum signing") from exc
    run(
        [
            "gpg",
            "--batch",
            "--yes",
            "--faked-system-time",
            str(timestamp),
            "--local-user",
            expected_signer,
            "--armor",
            "--detach-sign",
            "--output",
            str(signature),
            str(checksum_path),
        ],
        repo,
    )
    regular_file(signature, "detached SHA256SUMS signature")
    verification = run(
        ["gpg", "--batch", "--status-fd=1", "--verify", str(signature), str(checksum_path)],
        repo,
    )
    status_text = f"{verification.stdout}\n{verification.stderr}"
    signers = {
        match.group(1).upper()
        for match in re.finditer(
            r"^\[GNUPG:\] VALIDSIG ([0-9A-F]+)\b", status_text, re.MULTILINE
        )
    }
    if signers != {expected_signer.upper()}:
        raise GateError("SHA256SUMS signer does not match the signed-tag fingerprint")
    return signature


def canonical_evidence_archive(source: Path, destination: Path, prefix: str) -> None:
    if not source.is_dir() or source.is_symlink():
        raise GateError("detailed evidence staging directory is unsafe")
    try:
        paths = sorted(source.rglob("*"), key=lambda path: path.relative_to(source).as_posix())
        with destination.open("xb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as zipped:
                with tarfile.open(fileobj=zipped, mode="w", format=tarfile.GNU_FORMAT) as archive_file:
                    root = tarfile.TarInfo(prefix)
                    root.type = tarfile.DIRTYPE
                    root.mode = 0o755
                    root.uid = root.gid = 0
                    root.uname = root.gname = ""
                    root.mtime = 0
                    archive_file.addfile(root)
                    for path in paths:
                        relative_path = path.relative_to(source).as_posix()
                        metadata = os.lstat(path)
                        member = tarfile.TarInfo(f"{prefix}/{relative_path}")
                        member.uid = member.gid = 0
                        member.uname = member.gname = ""
                        member.mtime = 0
                        if stat.S_ISDIR(metadata.st_mode):
                            member.type = tarfile.DIRTYPE
                            member.mode = 0o755
                            archive_file.addfile(member)
                            continue
                        if not stat.S_ISREG(metadata.st_mode):
                            raise GateError("detailed evidence contains a non-regular path")
                        witness = capture_artifact(source, path)
                        data = witness_bytes(
                            witness,
                            f"detailed evidence file {relative_path}",
                            1024 * 1024 * 1024,
                        )
                        member.size = len(data)
                        member.mode = 0o644
                        archive_file.addfile(member, io.BytesIO(data))
    except (OSError, tarfile.TarError) as exc:
        raise GateError("cannot create deterministic detailed evidence archive") from exc


def publish_flat_release_set(
    repo: Path,
    detail_stage: Path,
    output: Path,
    tag: str,
    created: str,
    tag_signer: str,
    source_name: str,
    artifacts: list[ArtifactWitness],
    public_assets: list[tuple[ArtifactWitness, str]],
) -> None:
    artifact_names = [witness.path.name for witness in artifacts]
    artifact_names.extend(name for _, name in public_assets)
    evidence_name = f"teslatlas-hub-{tag}-evidence.tar.gz"
    reserved = {source_name, evidence_name, "SHA256SUMS", "SHA256SUMS.asc"}
    if len(set(artifact_names)) != len(artifact_names) or set(artifact_names) & reserved:
        raise GateError("release artifact basenames must be unique and must not collide with evidence assets")
    flat_stage = Path(tempfile.mkdtemp(prefix=f".{output.name}.flat.", dir=output.parent))
    try:
        for witness in artifacts:
            copy_witness_to(repo, witness, flat_stage / witness.path.name)
        for witness, name in public_assets:
            copy_witness_to(repo, witness, flat_stage / name)
        staged_source = detail_stage / source_name
        regular_file(staged_source, "generated source archive")
        staged_source.rename(flat_stage / source_name)
        evidence_prefix = f"teslatlas-hub-{tag}-evidence"
        canonical_evidence_archive(
            detail_stage,
            flat_stage / evidence_name,
            evidence_prefix,
        )
        checksum_records = [
            (path.name, sha256(path))
            for path in flat_stage.iterdir()
            if path.name not in {"SHA256SUMS", "SHA256SUMS.asc"}
        ]
        checksum_path = flat_stage / "SHA256SUMS"
        checksum_path.write_text(
            "\n".join(
                f"{digest}  {name}" for name, digest in sorted(checksum_records)
            )
            + "\n",
            encoding="utf-8",
        )
        sign_checksum_manifest(repo, checksum_path, created, tag_signer)
        run(["shasum", "-a", "256", "-c", "SHA256SUMS"], flat_stage)
        shutil.rmtree(detail_stage)
        publish_evidence_directory(flat_stage, output)
    except BaseException:
        shutil.rmtree(flat_stage, ignore_errors=True)
        raise


def archive(repo: Path, commit: str, tag: str, destination: Path) -> None:
    tar = subprocess.Popen(
        ["git", "archive", "--format=tar", f"--prefix=teslatlas-hub-{tag}/", commit],
        cwd=repo, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    assert tar.stdout is not None
    try:
        compressed = subprocess.run(
            ["gzip", "-n"], stdin=tar.stdout, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, check=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        tar.kill()
        tar.wait()
        raise GateError("deterministic source archive failed") from exc
    finally:
        tar.stdout.close()
    error = tar.stderr.read().decode(errors="replace")
    tar_rc = tar.wait()
    if tar_rc != 0:
        raise GateError(f"git archive failed: {error.strip()}")
    destination.write_bytes(compressed.stdout)


def cargo_metadata(repo: Path) -> dict:
    env = os.environ.copy()
    env["CARGO_NET_OFFLINE"] = "true"
    result = run(
        ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1",
         "--manifest-path", str(repo / "Cargo.toml")], repo, env=env,
    )
    try:
        metadata = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise GateError("cargo metadata did not return JSON") from exc

    return metadata


def portable_cargo_metadata(metadata: dict, repo: Path) -> dict:
    """Remove machine-local source paths while preserving package graph identity."""
    repo = repo.resolve()
    replacements: list[tuple[str, str]] = [(str(repo), "workspace://")]
    id_replacements: dict[str, str] = {}
    portable_ids: set[str] = set()
    packages = metadata.get("packages")
    if not isinstance(packages, list) or not packages:
        raise GateError("cargo metadata contains no packages")
    for package in packages:
        if not isinstance(package, dict):
            raise GateError("cargo metadata package entry is invalid")
        name = package.get("name")
        version = package.get("version")
        package_id = package.get("id")
        manifest = package.get("manifest_path")
        source = package.get("source")
        if not all(isinstance(item, str) and item for item in (name, version, package_id, manifest)):
            raise GateError("cargo metadata package identity is invalid")
        manifest_path = Path(manifest)
        if not manifest_path.is_absolute():
            raise GateError("cargo metadata manifest path is not absolute")
        source_identity = source if isinstance(source, str) and source else "workspace"
        source_digest = hashlib.sha256(source_identity.encode()).hexdigest()[:12]
        portable_id = f"cargo:{name}@{version}?source={source_digest}"
        if portable_id in portable_ids:
            raise GateError("cargo metadata has ambiguous portable package identities")
        portable_ids.add(portable_id)
        id_replacements[package_id] = portable_id
        package_root = manifest_path.parent
        try:
            relative_root = package_root.resolve().relative_to(repo)
        except ValueError:
            if not isinstance(source, str):
                raise GateError("cargo metadata has an external package without a source")
            scheme = "cargo-registry" if source.startswith("registry+") else "cargo-git"
            if scheme == "cargo-git" and not source.startswith("git+"):
                raise GateError("cargo metadata has an unsupported external package source")
            virtual_root = f"{scheme}://{source_digest}/{name}/{version}"
        else:
            suffix = relative_root.as_posix()
            virtual_root = "workspace://" + (suffix if suffix != "." else "")
        replacements.append((str(package_root), virtual_root))

    replacements.sort(key=lambda item: len(item[0]), reverse=True)

    def scrub(value: object) -> object:
        if isinstance(value, dict):
            return {key: scrub(item) for key, item in value.items()}
        if isinstance(value, list):
            return [scrub(item) for item in value]
        if not isinstance(value, str):
            return value
        if value in id_replacements:
            return id_replacements[value]
        normalized = value
        for raw_root, portable_root in replacements:
            if normalized == raw_root:
                normalized = portable_root
                break
            if normalized.startswith(raw_root + os.sep):
                normalized = portable_root.rstrip("/") + "/" + normalized[len(raw_root) + 1:]
                break
        if os.path.isabs(normalized) or normalized.startswith("file:///"):
            raise GateError("cargo metadata contains an unportable absolute path")
        return normalized

    portable = scrub(metadata)
    serialized = json.dumps(portable, ensure_ascii=False, sort_keys=True)
    forbidden = (str(repo), str(Path.home()), "/.cargo/registry/", "\\.cargo\\registry\\")
    if any(item and item in serialized for item in forbidden):
        raise GateError("portable cargo metadata retains a machine-local path")
    return portable  # type: ignore[return-value]


def cargo_lock_checksums(repo: Path) -> dict[tuple[str, str, str], str]:
    lock_path = repo / "Cargo.lock"
    witness = capture_artifact(repo, lock_path)
    data = witness_bytes(witness, "Cargo lockfile", MAX_CARGO_LOCK_BYTES)
    text = strict_utf8_document(data, "Cargo lockfile")
    lock_version: int | None = None
    packages: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    in_package = False

    def finish_package() -> None:
        nonlocal current
        if current is not None:
            packages.append(current)
            current = None

    for raw_line in text.splitlines():
        stripped = raw_line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped == "[[package]]":
            finish_package()
            current = {}
            in_package = True
            continue
        if re.fullmatch(r"\[\[?[^\[\]\r\n]+\]\]?", stripped):
            finish_package()
            in_package = False
            continue
        if not in_package:
            version_match = re.fullmatch(r"version\s*=\s*([0-9]+)", stripped)
            if version_match is not None:
                if lock_version is not None:
                    raise GateError("Cargo lockfile has duplicate format versions")
                lock_version = int(version_match.group(1))
            elif re.match(r"version(?:\s|=)", stripped):
                raise GateError("Cargo lockfile format version is invalid")
            continue
        assert current is not None
        field_match = re.fullmatch(
            r'(name|version|source|checksum)\s*=\s*"([^"\\\x00-\x1f]+)"',
            stripped,
        )
        if field_match is not None:
            field, value = field_match.groups()
            if field in current:
                raise GateError(f"Cargo lockfile package has duplicate field: {field}")
            current[field] = value
        elif re.match(r"(?:name|version|source|checksum)(?:\s|=)", stripped):
            raise GateError("Cargo lockfile package identity field is invalid")
    finish_package()
    if lock_version not in {3, 4}:
        raise GateError("Cargo lockfile format version is unsupported")
    if not packages:
        raise GateError("Cargo lockfile contains no packages")
    checksums: dict[tuple[str, str, str], str] = {}
    for package in packages:
        name = package.get("name")
        version = package.get("version")
        source = package.get("source")
        checksum = package.get("checksum")
        if not all(isinstance(value, str) and value for value in (name, version)):
            raise GateError("Cargo lockfile package identity is invalid")
        if source is None:
            continue
        if not isinstance(source, str):
            raise GateError("Cargo lockfile package source is invalid")
        if not source.startswith("registry+"):
            continue
        if not isinstance(checksum, str) or not re.fullmatch(r"[0-9a-f]{64}", checksum):
            raise GateError(f"registry dependency has no valid lockfile checksum: {name}")
        key = (name, version, source)
        if key in checksums and checksums[key] != checksum:
            raise GateError(f"Cargo lockfile has conflicting checksums: {name}")
        checksums[key] = checksum
    return checksums


def normalize_spdx_expression(expression: str, package_name: str) -> str:
    if expression == "NOASSERTION":
        return expression
    # Cargo historically used this exact dual-license spelling. Do not guess
    # the meaning of any other legacy separator.
    stripped = expression.strip()
    legacy_normalizations = {
        "MIT/Apache-2.0": "MIT OR Apache-2.0",
        "Apache-2.0 / MIT": "Apache-2.0 OR MIT",
        "Unlicense/MIT": "Unlicense OR MIT",
    }
    candidate = legacy_normalizations.get(stripped, stripped)
    if "/" in candidate or "," in candidate or "+" in candidate:
        raise GateError(f"dependency has ambiguous legacy license expression: {package_name}")
    tokens = re.findall(r"\(|\)|AND|OR|WITH|[A-Za-z0-9][A-Za-z0-9.-]*", candidate)
    if not tokens or "".join(tokens) != re.sub(r"\s+", "", candidate):
        raise GateError(f"dependency has invalid SPDX license expression: {package_name}")
    index = 0

    def parse_primary() -> None:
        nonlocal index
        if index >= len(tokens):
            raise ValueError
        if tokens[index] == "(":
            index += 1
            parse_or()
            if index >= len(tokens) or tokens[index] != ")":
                raise ValueError
            index += 1
            return
        if tokens[index] in {"AND", "OR", "WITH", ")"}:
            raise ValueError
        index += 1

    def parse_with() -> None:
        nonlocal index
        parse_primary()
        if index < len(tokens) and tokens[index] == "WITH":
            index += 1
            if index >= len(tokens) or tokens[index] in {"(", ")", "AND", "OR", "WITH"}:
                raise ValueError
            index += 1

    def parse_and() -> None:
        nonlocal index
        parse_with()
        while index < len(tokens) and tokens[index] == "AND":
            index += 1
            parse_with()

    def parse_or() -> None:
        nonlocal index
        parse_and()
        while index < len(tokens) and tokens[index] == "OR":
            index += 1
            parse_and()

    try:
        parse_or()
    except ValueError as exc:
        raise GateError(f"dependency has invalid SPDX license expression: {package_name}") from exc
    if index != len(tokens):
        raise GateError(f"dependency has invalid SPDX license expression: {package_name}")
    return " ".join(tokens).replace("( ", "(").replace(" )", ")")


def package_license_material(
    package: dict, repo: Path, license_expression: str
) -> tuple[str, str, list[str], list[str], str]:
    local_paths = package_local_legal_paths(package, repo)
    if local_paths:
        names: list[str] = []
        texts: list[str] = []
        sources: list[str] = []
        notice_sources: list[str] = []
        copyright_texts: list[str] = []
        for path in local_paths:
            try:
                text = dependency_legal_text(path, package, repo)
            except (OSError, UnicodeError) as exc:
                raise GateError(
                    f"dependency legal text is unreadable: {package.get('name')}"
                ) from exc
            names.append(path.name)
            texts.append(f"===== {path.name} =====\n{text}")
            sources.append(path.name)
            lowered = path.name.lower()
            if lowered.startswith(("notice", "copyright")):
                notice_sources.append(path.name)
            if lowered.startswith("copyright"):
                copyright_texts.append(text.rstrip())
        copyright_evidence = "\n\n".join(copyright_texts) or "NOASSERTION"
        return (
            " + ".join(names),
            "\n".join(texts),
            sources,
            notice_sources,
            copyright_evidence,
        )

    corpus = repo / "LICENSES"
    if not corpus.is_dir() or corpus.is_symlink():
        raise GateError(
            f"dependency license text is unavailable and SPDX corpus is missing: "
            f"{package.get('name')}"
        )
    identifiers = spdx_identifiers(license_expression)
    if not identifiers:
        raise GateError(f"dependency license text is unavailable: {package.get('name')}")
    names: list[str] = []
    texts: list[str] = []
    sources: list[str] = []
    for identifier in identifiers:
        path = corpus / f"{identifier}.txt"
        regular_file(path, f"SPDX corpus license {identifier}")
        try:
            text = path.read_text(encoding="utf-8", errors="strict")
        except (OSError, UnicodeError) as exc:
            raise GateError(f"SPDX corpus license is unreadable: {identifier}") from exc
        names.append(path.name)
        texts.append(text)
        sources.append(f"LICENSES/{path.name}")
    return " + ".join(names), "".join(texts), sources, [], "NOASSERTION"


def package_sort(package: dict) -> tuple[str, str, str]:
    return (package.get("name", ""), package.get("version", ""), package.get("source", ""))


def spdx_identifiers(expression: str) -> list[str]:
    identifiers: list[str] = []
    for token in re.findall(r"\(|\)|AND|OR|WITH|[A-Za-z0-9][A-Za-z0-9.-]*", expression):
        if token not in {"(", ")", "AND", "OR", "WITH", "NOASSERTION"} \
                and token not in identifiers:
            identifiers.append(token)
    return identifiers


def package_local_legal_paths(package: dict, repo: Path) -> list[Path]:
    manifest = package.get("manifest_path")
    if not manifest:
        return []
    manifest_path = Path(manifest)
    if not manifest_path.is_absolute():
        manifest_path = repo / manifest_path
    try:
        manifest_metadata = os.lstat(manifest_path)
        root = manifest_path.parent
        root_metadata = os.lstat(root)
        root_resolved = root.resolve(strict=True)
        manifest_resolved = manifest_path.resolve(strict=True)
    except OSError as exc:
        raise GateError(
            f"dependency manifest path is unsafe: {package.get('name')}"
        ) from exc
    if (
        not stat.S_ISREG(manifest_metadata.st_mode)
        or manifest_metadata.st_nlink != 1
        or not stat.S_ISDIR(root_metadata.st_mode)
        or manifest_resolved.parent != root_resolved
    ):
        raise GateError(f"dependency manifest path is unsafe: {package.get('name')}")
    is_project = root_resolved == repo.resolve()
    if is_project:
        exact = repo / "LICENSE"
        regular_file(exact, "project license")
        if os.lstat(exact).st_nlink != 1:
            raise GateError("project license must not be hard-linked")
        return [exact]
    declared = package.get("license_file")
    candidates: list[Path] = []
    if declared:
        if not isinstance(declared, str):
            raise GateError(f"dependency license_file is invalid: {package.get('name')}")
        declared_path = Path(declared)
        if (
            declared_path.is_absolute()
            or not declared_path.parts
            or any(part in {"", ".", ".."} for part in declared_path.parts)
        ):
            raise GateError(f"dependency license_file escapes package root: {package.get('name')}")
        candidates.append(root / declared_path)
    try:
        candidates.extend(sorted(
            path for path in root.iterdir()
            if re.match(
                r"(?i)^(license|licence|copying|notice|copyright)([-_.].*)?$",
                path.name,
            )
        ))
    except OSError as exc:
        raise GateError(f"dependency package root is unreadable: {package.get('name')}") from exc
    selected: list[Path] = []
    for candidate in candidates:
        try:
            metadata = os.lstat(candidate)
            resolved = candidate.resolve(strict=True)
        except OSError as exc:
            raise GateError(f"dependency legal path is unsafe: {package.get('name')}") from exc
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or resolved.parent != root_resolved
        ):
            raise GateError(f"dependency legal path is unsafe: {package.get('name')}")
        if resolved not in selected:
            selected.append(resolved)
    return selected


def dependency_legal_text(path: Path, package: dict, repo: Path) -> str:
    """Read a package legal file without following links or accepting replacement."""
    label = str(package.get("name", "unknown"))
    try:
        before = os.lstat(path)
    except OSError as exc:
        raise GateError(f"dependency legal text is unreadable: {label}") from exc
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_nlink != 1
        or before.st_size <= 0
        or before.st_size > MAX_LEGAL_FILE_BYTES
    ):
        raise GateError(f"dependency legal text is unsafe or oversized: {label}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise GateError(f"dependency legal text is unreadable: {label}") from exc
    try:
        opened = os.fstat(descriptor)
        if (
            (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)
            or opened.st_nlink != 1
        ):
            raise GateError(f"dependency legal text changed while opening: {label}")
        data = bytearray()
        while len(data) <= MAX_LEGAL_FILE_BYTES:
            block = os.read(
                descriptor,
                min(1024 * 1024, MAX_LEGAL_FILE_BYTES + 1 - len(data)),
            )
            if not block:
                break
            data.extend(block)
        after = os.fstat(descriptor)
        if (
            len(data) != opened.st_size
            or after.st_size != opened.st_size
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
            or after.st_nlink != 1
        ):
            raise GateError(f"dependency legal text changed while reading: {label}")
    finally:
        os.close(descriptor)
    try:
        current = os.lstat(path)
    except OSError as exc:
        raise GateError(f"dependency legal text changed after reading: {label}") from exc
    if (
        (current.st_dev, current.st_ino) != (opened.st_dev, opened.st_ino)
        or current.st_size != opened.st_size
        or current.st_mtime_ns != opened.st_mtime_ns
        or current.st_nlink != 1
    ):
        raise GateError(f"dependency legal text changed after reading: {label}")
    try:
        return bytes(data).decode("utf-8", errors="strict")
    except UnicodeError as exc:
        raise GateError(f"dependency legal text is not UTF-8: {label}") from exc


def sbom_and_notices(metadata: dict, repo: Path) -> tuple[dict, dict, str]:
    project_notices = repo / "docs/legal/third-party-notices.md"
    regular_file(project_notices, "project notices")
    packages = sorted(metadata.get("packages", []), key=package_sort)
    if not packages:
        raise GateError("cargo metadata contains no packages")
    ids = {package["id"]: f"SPDXRef-Package-{index:04d}" for index, package in enumerate(packages, 1)}
    spdx_packages = []
    inventory = []
    license_texts: dict[str, tuple[str, str]] = {}
    lock_checksums = cargo_lock_checksums(repo)
    for package in packages:
        original_license_expression = package.get("license") or "NOASSERTION"
        if not isinstance(original_license_expression, str):
            raise GateError(f"dependency has invalid license metadata: {package.get('name')}")
        license_expression = normalize_spdx_expression(
            original_license_expression, str(package.get("name", "unknown"))
        )
        if license_expression == "NOASSERTION" and not package.get("license_file"):
            raise GateError(f"dependency has no declared license: {package.get('name')}")
        source = package.get("source") or "NOASSERTION"
        checksum = package.get("checksum")
        source_key = package.get("source")
        if source_key and not checksum and isinstance(source_key, str):
            checksum = lock_checksums.get(
                (str(package.get("name", "")), str(package.get("version", "")), source_key)
            )
        if checksum is not None and (
            not isinstance(checksum, str) or not re.fullmatch(r"[0-9a-f]{64}", checksum)
        ):
            raise GateError(f"dependency has an invalid Cargo checksum: {package.get('name')}")
        if package.get("source") and not checksum and not package["source"].startswith("git+"):
            raise GateError(f"dependency has no Cargo checksum: {package.get('name')}")
        (
            license_name,
            text,
            license_sources,
            package_notice_sources,
            copyright_evidence,
        ) = package_license_material(
            package, repo, license_expression
        )
        text_hash = hashlib.sha256(text.encode()).hexdigest()
        license_texts.setdefault(text_hash, (license_name, text))
        package_id = ids[package["id"]]
        spdx_package = {
            "SPDXID": package_id,
            "name": package["name"],
            "versionInfo": package["version"],
            "downloadLocation": source,
            "licenseConcluded": license_expression,
            "licenseDeclared": license_expression,
            "copyrightText": copyright_evidence,
            "filesAnalyzed": False,
            "annotations": [{"annotationType": "OTHER", "annotator": "Tool: teslatlas-release-evidence",
                              "annotationDate": "1970-01-01T00:00:00Z",
                              "comment": f"license-text-sha256={text_hash}"}],
        }
        if checksum:
            spdx_package["checksums"] = [{"algorithm": "SHA256", "checksumValue": checksum}]
        if package.get("repository"):
            spdx_package["externalRefs"] = [{
                "referenceCategory": "PACKAGE-MANAGER", "referenceType": "purl",
                "referenceLocator": f"pkg:cargo/{package['name']}@{package['version']}",
            }]
        spdx_packages.append(spdx_package)
        inventory_record = {
            "name": package["name"], "version": package["version"], "source": source,
            "checksum": checksum, "license": license_expression, "license_text_sha256": text_hash,
            "license_text_sources": license_sources,
            "package_notice": (
                "captured" if package_notice_sources else "absent-in-crate-archive"
            ),
            "package_notice_sources": package_notice_sources,
            "authors": package.get("authors") or [],
            "repository": package.get("repository"),
            "package_id": package_id,
        }
        if original_license_expression != license_expression:
            inventory_record["license_original"] = original_license_expression
        inventory.append(inventory_record)

    relationships = []
    resolve_root = (metadata.get("resolve") or {}).get("root")
    root_package_id = resolve_root if resolve_root in ids else None
    workspace_members = metadata.get("workspace_members") or []
    if root_package_id is None:
        root_package_id = next((item for item in workspace_members if item in ids), None)
    if root_package_id is None:
        root_package_id = next((package["id"] for package in packages if package.get("name") == "teslatlas-hub"), None)
    if root_package_id is None:
        raise GateError("cargo metadata has no workspace root package")
    root_id = ids[root_package_id]
    relationships.append({"spdxElementId": "SPDXRef-DOCUMENT", "relationshipType": "DESCRIBES",
                          "relatedSpdxElement": root_id})
    resolve = metadata.get("resolve") or {}
    for node in resolve.get("nodes", []):
        source_id = ids.get(node.get("id"))
        if not source_id:
            continue
        for dependency in node.get("deps", []):
            target_id = ids.get(dependency.get("pkg"))
            if target_id:
                relationships.append({"spdxElementId": source_id, "relationshipType": "DEPENDS_ON",
                                      "relatedSpdxElement": target_id})
    relationships.sort(key=lambda item: (item["spdxElementId"], item["relatedSpdxElement"]))
    spdx = {
        "spdxVersion": "SPDX-2.3", "dataLicense": "CC0-1.0", "SPDXID": "SPDXRef-DOCUMENT",
        "name": "Teslatlas Hub dependency SBOM", "documentNamespace": "urn:teslatlas:sbom",
        "creationInfo": {"created": "1970-01-01T00:00:00Z", "creators": ["Tool: teslatlas-release-evidence"]},
        "packages": spdx_packages, "relationships": relationships,
    }
    notice_lines = [
        "# Generated dependency notices", "",
        "Generated from offline `cargo metadata --locked --all-features`.",
        "This file contains the exact dependency inventory and captured license texts.", "",
        "## Project notices", "", project_notices.read_text(encoding="utf-8"), "",
        "## Dependency inventory", "",
    ]
    for item in inventory:
        notice_lines.extend([f"### {item['name']} {item['version']}", "",
                             f"- Source: `{item['source']}`",
                             f"- License: `{item['license']}`",
                             f"- License text source(s): `{', '.join(item['license_text_sources'])}`",
                             f"- Package notice: `{item['package_notice']}`",
                             f"- Package notice source(s): `{', '.join(item['package_notice_sources']) or 'none'}`",
                             f"- License text SHA-256: `{item['license_text_sha256']}`", ""])
    notice_lines.extend(["## License texts", ""])
    for text_hash, (name, text) in sorted(license_texts.items()):
        notice_lines.extend([f"### {name} ({text_hash})", "", "```text", text.rstrip(), "```", ""])
    return spdx, {"schema": SCHEMA, "packages": inventory}, "\n".join(notice_lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--tag", required=True)
    parser.add_argument("--tag-signer-fingerprint", required=True,
                        help="approved OpenPGP maintainer fingerprint for the signed tag")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--signing-key", type=Path, required=True,
                        help="unencrypted PEM private key used only for provenance signing")
    parser.add_argument("--public-key-sha256", required=True,
                        help="independently recorded SHA-256 of the PEM public-key trust anchor")
    parser.add_argument("--artifact", action="append", type=Path, required=True,
                        help="final artifact path; repeat for each pkg/zip/deb")
    parser.add_argument("--legal-bundle", type=Path, required=True,
                        help="prebuilt exact dependency legal bundle embedded in every artifact")
    parser.add_argument(
        "--rust-source-evidence",
        type=Path,
        required=True,
        help="exact vendored Rust source archive and offline locked rebuild evidence",
    )
    parser.add_argument("--go-proxy-evidence", type=Path,
                        help="evidence generated for the unsigned Tesla proxy in macOS artifacts")
    parser.add_argument("--fleet-telemetry-evidence", type=Path,
                        help="evidence generated for the unsigned Fleet Telemetry receiver in macOS artifacts")
    parser.add_argument(
        "--linux-go-proxy-evidence",
        action="append",
        default=[],
        metavar="ARCH=DIR",
        help="cross-target clean-rebuild evidence for a Debian command proxy; repeat per architecture",
    )
    parser.add_argument(
        "--linux-fleet-telemetry-evidence",
        action="append",
        default=[],
        metavar="ARCH=DIR",
        help="cross-target clean-rebuild evidence for a Debian Fleet receiver; repeat per architecture",
    )
    parser.add_argument(
        "--debian-attestation",
        action="append",
        type=Path,
        default=[],
        help="verified native attestation directory; repeat once per Debian package",
    )
    parser.add_argument(
        "--debian-attestation-public-key",
        type=Path,
        help="Ed25519 public key used to verify Debian native attestations",
    )
    parser.add_argument(
        "--debian-attestation-public-key-sha256",
        help="independently pinned SHA-256 of the Debian attestation public key",
    )
    args = parser.parse_args()
    if not TAG_RE.fullmatch(args.tag):
        raise GateError("tag contains unsafe filename characters")
    if not FINGERPRINT_RE.fullmatch(args.tag_signer_fingerprint):
        raise GateError("tag-signer-fingerprint must be a full OpenPGP fingerprint")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", args.public_key_sha256):
        raise GateError("public-key-sha256 must be 64 hexadecimal characters")
    repo = args.repo.resolve()
    output = args.output_dir.resolve()
    if not (repo / "Cargo.toml").is_file():
        raise GateError("repo has no Cargo.toml")
    package_version = cargo_package_version(repo)
    expected_tag = f"v{package_version}"
    if args.tag != expected_tag:
        raise GateError(
            f"release tag does not match Cargo package version; expected {expected_tag}"
        )
    if os.path.lexists(output):
        raise GateError(f"output directory already exists: {output}")
    if not output.parent.is_dir():
        raise GateError("output parent does not exist")
    private_signing_key(args.signing_key.resolve())
    release_signing_key = capture_artifact(repo, repo / "RELEASE_SIGNING_KEY.asc")
    for command in ("git", "cargo", "gpg", "gzip", "openssl", "shasum"):
        if shutil.which(command) is None:
            raise GateError(f"required tool is unavailable: {command}")
    artifacts = []
    for raw in args.artifact:
        path = raw.resolve()
        regular_file(path, "artifact")
        try:
            path.relative_to(repo)
        except ValueError as exc:
            raise GateError(f"artifact must be inside repo: {path}") from exc
        artifacts.append(path)
    if len({path for path in artifacts}) != len(artifacts):
        raise GateError("duplicate artifact path")
    has_macos_artifact = any(requires_external_proxy_notice(path) for path in artifacts)
    if has_macos_artifact and any(
        shutil.which(command) is None
        for command in ("codesign", "pkgutil", "spctl", "xcrun")
    ):
        raise GateError(
            "codesign, pkgutil, spctl, and xcrun are required for macOS release evidence"
        )
    if has_macos_artifact and args.go_proxy_evidence is None:
        raise GateError("macOS artifacts require --go-proxy-evidence")
    if has_macos_artifact and args.fleet_telemetry_evidence is None:
        raise GateError("macOS artifacts require --fleet-telemetry-evidence")
    if (args.go_proxy_evidence is None) != (args.fleet_telemetry_evidence is None):
        raise GateError("Go and Fleet Telemetry evidence must be supplied together")
    rust_source_evidence_dir = Path(os.path.abspath(args.rust_source_evidence))
    rust_source_evidence_witnesses = capture_rust_source_evidence(
        repo, rust_source_evidence_dir
    )
    go_evidence_dir: Path | None = None
    go_evidence_witnesses: list[ArtifactWitness] = []
    if args.go_proxy_evidence is not None:
        go_evidence_dir = args.go_proxy_evidence.resolve()
        go_evidence_witnesses = capture_go_evidence(repo, go_evidence_dir)
    fleet_telemetry_evidence_dir: Path | None = None
    fleet_telemetry_evidence_witnesses: list[ArtifactWitness] = []
    if args.fleet_telemetry_evidence is not None:
        fleet_telemetry_evidence_dir = args.fleet_telemetry_evidence.resolve()
        fleet_telemetry_evidence_witnesses = capture_fleet_telemetry_evidence(
            repo, fleet_telemetry_evidence_dir
        )
    legal_bundle_dir = args.legal_bundle.resolve()
    legal_bundle_witnesses = capture_legal_bundle(
        repo,
        legal_bundle_dir,
        go_evidence_dir,
        fleet_telemetry_evidence_dir,
    )
    legal_bundle_bytes: dict[str, bytes] = {}
    for witness in legal_bundle_witnesses:
        data = witness.path.read_bytes()
        if len(data) != witness.size or hashlib.sha256(data).hexdigest() != witness.digest:
            raise GateError("dependency legal bundle changed before artifact validation")
        legal_bundle_bytes[witness.path.name] = data
    bundle_has_sidecars = bool(set(legal_bundle_bytes) & set(LEGAL_BUNDLE_SIDECAR_NAMES))
    artifact_witnesses = [capture_artifact(repo, path) for path in artifacts]
    packages_by_architecture, debian_sidecars = validate_debian_artifacts(
        repo,
        artifact_witnesses,
        package_version,
        legal_bundle_bytes,
        bundle_has_sidecars,
    )
    linux_go_directories = architecture_evidence_directories(
        args.linux_go_proxy_evidence, "--linux-go-proxy-evidence"
    )
    linux_fleet_directories = architecture_evidence_directories(
        args.linux_fleet_telemetry_evidence, "--linux-fleet-telemetry-evidence"
    )
    required_linux_architectures = set(debian_sidecars)
    require_linux_evidence_coverage(
        required_linux_architectures,
        linux_go_directories,
        linux_fleet_directories,
    )
    linux_go_evidence: dict[str, tuple[Path, list[ArtifactWitness], dict[str, object]]] = {}
    linux_fleet_evidence: dict[
        str, tuple[Path, list[ArtifactWitness], dict[str, object]]
    ] = {}
    for architecture in sorted(required_linux_architectures):
        go_directory = linux_go_directories[architecture]
        go_witnesses = capture_go_evidence(repo, go_directory)
        go_manifest_value = captured_component_manifest(
            go_witnesses, "go-component-manifest.json", "Linux Go proxy evidence"
        )
        fleet_directory = linux_fleet_directories[architecture]
        fleet_witnesses = capture_fleet_telemetry_evidence(repo, fleet_directory)
        fleet_manifest_value = captured_component_manifest(
            fleet_witnesses,
            "fleet-telemetry-component-manifest.json",
            "Linux Fleet Telemetry evidence",
        )
        validate_linux_sidecar_evidence_subjects(
            architecture,
            go_manifest_value,
            fleet_manifest_value,
            debian_sidecars[architecture],
        )
        linux_go_evidence[architecture] = (
            go_directory,
            go_witnesses,
            go_manifest_value,
        )
        linux_fleet_evidence[architecture] = (
            fleet_directory,
            fleet_witnesses,
            fleet_manifest_value,
        )
    has_debian_artifact = bool(packages_by_architecture)
    attestation_key: ArtifactWitness | None = None
    debian_attestations: list[DebianAttestation] = []
    if has_debian_artifact:
        if args.debian_attestation_public_key is None:
            raise GateError("Debian packages require --debian-attestation-public-key")
        if args.debian_attestation_public_key_sha256 is None or not re.fullmatch(
            r"[0-9a-fA-F]{64}", args.debian_attestation_public_key_sha256
        ):
            raise GateError(
                "Debian packages require a 64-hex --debian-attestation-public-key-sha256"
            )
        key_path = Path(os.path.abspath(args.debian_attestation_public_key))
        attestation_key = capture_artifact(repo, key_path)
        if attestation_key.size > MAX_ATTESTATION_PUBLIC_KEY_BYTES:
            raise GateError("Debian attestation public key is oversized")
        if attestation_key.digest.lower() != args.debian_attestation_public_key_sha256.lower():
            raise GateError("Debian attestation public key does not match its pinned SHA-256")
        debian_attestations = capture_debian_attestations(
            repo,
            args.debian_attestation,
            packages_by_architecture,
            debian_sidecars,
        )
    elif (
        args.debian_attestation
        or args.debian_attestation_public_key is not None
        or args.debian_attestation_public_key_sha256 is not None
    ):
        raise GateError("Debian attestation inputs require a Debian package artifact")
    ignored_paths: list[Path] = []
    release_bundle_path: Path | None = None
    release_receipt_witnesses: list[ArtifactWitness] = []
    if has_macos_artifact:
        assert go_evidence_dir is not None and fleet_telemetry_evidence_dir is not None
        release_bundle = capture_macos_release_bundle(
            repo,
            artifact_witnesses,
            go_evidence_dir,
            go_evidence_witnesses,
            fleet_telemetry_evidence_dir,
            fleet_telemetry_evidence_witnesses,
            legal_bundle_dir,
            legal_bundle_witnesses,
        )
        if release_bundle is not None:
            release_bundle_path, release_receipt_witnesses = release_bundle
            ignored_paths.extend(
                witness.path
                for witness in [
                    *go_evidence_witnesses,
                    *fleet_telemetry_evidence_witnesses,
                    *legal_bundle_witnesses,
                    *release_receipt_witnesses,
                ]
            )
    if go_evidence_dir is not None and release_bundle_path is None:
        try:
            go_evidence_dir.relative_to(repo)
        except ValueError:
            pass
        else:
            ignored_paths.extend(witness.path for witness in go_evidence_witnesses)
    if fleet_telemetry_evidence_dir is not None and release_bundle_path is None:
        try:
            fleet_telemetry_evidence_dir.relative_to(repo)
        except ValueError:
            pass
        else:
            ignored_paths.extend(witness.path for witness in fleet_telemetry_evidence_witnesses)
    for directory, witnesses, _ in [
        *linux_go_evidence.values(),
        *linux_fleet_evidence.values(),
    ]:
        try:
            directory.relative_to(repo)
        except ValueError:
            pass
        else:
            ignored_paths.extend(witness.path for witness in witnesses)
    try:
        legal_bundle_dir.relative_to(repo)
    except ValueError:
        pass
    else:
        ignored_paths.extend(witness.path for witness in legal_bundle_witnesses)
    try:
        rust_source_evidence_dir.relative_to(repo)
    except ValueError:
        pass
    else:
        ignored_paths.extend(witness.path for witness in rust_source_evidence_witnesses)
    if attestation_key is not None:
        for witness in [
            attestation_key,
            *(
                item
                for attestation in debian_attestations
                for item in (attestation.receipt, attestation.signature)
            ),
        ]:
            try:
                witness.path.relative_to(repo)
            except ValueError:
                pass
            else:
                ignored_paths.append(witness.path)
    commit, created, tag_signer = clean_and_tag(
        repo, args.tag, artifacts, ignored_paths, args.tag_signer_fingerprint
    )
    validate_release_signing_key(repo, release_signing_key, tag_signer)
    if attestation_key is not None:
        assert args.debian_attestation_public_key_sha256 is not None
        verify_debian_attestations(
            repo,
            args.tag,
            args.tag_signer_fingerprint,
            debian_attestations,
            attestation_key,
            args.debian_attestation_public_key_sha256.lower(),
        )

    stage = Path(tempfile.mkdtemp(prefix=f".{output.name}.", dir=output.parent))
    try:
        go_manifest: dict | None = None
        fleet_telemetry_manifest: dict | None = None
        rust_source_manifest: dict | None = None
        debian_attestation_records: list[dict[str, str]] = []
        linux_sidecar_records: dict[str, dict[str, object]] = {}
        legal_output = stage / "dependency-legal"
        legal_output.mkdir(mode=0o700)
        for witness in legal_bundle_witnesses:
            copy_witness_to(repo, witness, legal_output / witness.path.name)
        staged_legal_bundle = {
            name: (legal_output / name).read_bytes()
            for name in sorted(legal_bundle_bytes)
        }
        if staged_legal_bundle != legal_bundle_bytes:
            raise GateError("staged dependency legal bundle does not match its input")
        rust_source_output = stage / "rust-source-evidence"
        rust_source_output.mkdir(mode=0o700)
        rust_source_by_name = {
            witness.path.name: witness for witness in rust_source_evidence_witnesses
        }
        for name in RUST_SOURCE_EVIDENCE_NAMES:
            copy_witness_to(repo, rust_source_by_name[name], rust_source_output / name)
        rust_source_helper = repo / "scripts" / "rust-source-evidence.py"
        run(
            [
                sys.executable,
                str(rust_source_helper),
                "--repo",
                str(repo),
                "--verify-dir",
                str(rust_source_output),
                "--rebuild",
            ],
            repo,
        )
        try:
            rust_source_manifest = json.loads(
                (rust_source_output / "rust-source-evidence-manifest.json").read_text()
            )
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise GateError("staged Rust source evidence manifest is unreadable") from exc
        if debian_attestations:
            attestation_output = stage / "debian-native-attestations"
            attestation_output.mkdir(mode=0o700)
            assert attestation_key is not None
            copy_witness_to(
                repo,
                attestation_key,
                stage / DEBIAN_ATTESTATION_PUBLIC_KEY_NAME,
            )
            for attestation in debian_attestations:
                architecture_output = attestation_output / attestation.architecture
                architecture_output.mkdir(mode=0o700)
                copy_witness_to(
                    repo,
                    attestation.receipt,
                    architecture_output / DEBIAN_ATTESTATION_RECEIPT_NAME,
                )
                copy_witness_to(
                    repo,
                    attestation.signature,
                    architecture_output / DEBIAN_ATTESTATION_SIGNATURE_NAME,
                )
                debian_attestation_records.append(
                    {
                        "architecture": attestation.architecture,
                        "package_sha256": attestation.package.digest,
                        "public_key_sha256": attestation_key.digest,
                        "receipt_sha256": attestation.receipt.digest,
                        "signature_sha256": attestation.signature.digest,
                    }
                )
        if go_evidence_witnesses:
            go_output = stage / "go-proxy-evidence"
            go_output.mkdir(mode=0o700)
            by_name = {witness.path.name: witness for witness in go_evidence_witnesses}
            for name in GO_EVIDENCE_NAMES:
                copy_witness_to(repo, by_name[name], go_output / name)
            helper = repo / "scripts" / "go-proxy-evidence.py"
            run(
                [
                    sys.executable,
                    str(helper),
                    "--repo",
                    str(repo),
                    "--verify-dir",
                    str(go_output),
                ],
                repo,
            )
            try:
                go_manifest = json.loads(
                    (go_output / "go-component-manifest.json").read_text()
                )
            except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
                raise GateError("staged Go component manifest is unreadable") from exc
        if fleet_telemetry_evidence_witnesses:
            fleet_output = stage / "fleet-telemetry-evidence"
            fleet_output.mkdir(mode=0o700)
            by_name = {witness.path.name: witness for witness in fleet_telemetry_evidence_witnesses}
            for name in FLEET_TELEMETRY_EVIDENCE_NAMES:
                copy_witness_to(repo, by_name[name], fleet_output / name)
            helper = repo / "scripts" / "fleet-telemetry-evidence.py"
            run([sys.executable, str(helper), "--repo", str(repo), "--verify-dir", str(fleet_output)], repo)
            try:
                fleet_telemetry_manifest = json.loads(
                    (fleet_output / "fleet-telemetry-component-manifest.json").read_text()
                )
            except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
                raise GateError("staged Fleet Telemetry component manifest is unreadable") from exc
        for architecture in sorted(required_linux_architectures):
            architecture_output = stage / "linux-sidecar-evidence" / architecture
            go_output = architecture_output / "go-proxy"
            fleet_output = architecture_output / "fleet-telemetry"
            go_output.mkdir(parents=True, mode=0o700)
            fleet_output.mkdir(mode=0o700)
            _, go_witnesses, _ = linux_go_evidence[architecture]
            _, fleet_witnesses, _ = linux_fleet_evidence[architecture]
            for witness in go_witnesses:
                copy_witness_to(repo, witness, go_output / witness.path.name)
            for witness in fleet_witnesses:
                copy_witness_to(repo, witness, fleet_output / witness.path.name)
            run(
                [
                    sys.executable,
                    str(repo / "scripts" / "go-proxy-evidence.py"),
                    "--repo",
                    str(repo),
                    "--verify-dir",
                    str(go_output),
                ],
                repo,
            )
            run(
                [
                    sys.executable,
                    str(repo / "scripts" / "fleet-telemetry-evidence.py"),
                    "--repo",
                    str(repo),
                    "--verify-dir",
                    str(fleet_output),
                ],
                repo,
            )
            staged_go_manifest = component_manifest(
                go_output, "go-component-manifest.json", "staged Linux Go proxy evidence"
            )
            staged_fleet_manifest = component_manifest(
                fleet_output,
                "fleet-telemetry-component-manifest.json",
                "staged Linux Fleet Telemetry evidence",
            )
            validate_linux_sidecar_evidence_subjects(
                architecture,
                staged_go_manifest,
                staged_fleet_manifest,
                debian_sidecars[architecture],
            )
            linux_sidecar_records[architecture] = {
                "go_proxy": {
                    "target": staged_go_manifest["target"],
                    "subject": staged_go_manifest["subject"],
                    "manifest_sha256": sha256(
                        go_output / "go-component-manifest.json"
                    ),
                },
                "fleet_telemetry": {
                    "target": staged_fleet_manifest["subject"]["target"],
                    "subject": staged_fleet_manifest["subject"],
                    "manifest_sha256": sha256(
                        fleet_output / "fleet-telemetry-component-manifest.json"
                    ),
                },
            }
        if release_receipt_witnesses:
            receipt_output = stage / "macos-release-receipts"
            log_output = receipt_output / "notary-logs"
            log_output.mkdir(parents=True, mode=0o700)
            for witness in release_receipt_witnesses:
                destination = (
                    log_output / witness.path.name
                    if witness.path.parent.name == "notary-logs"
                    else receipt_output / witness.path.name
                )
                copy_witness_to(repo, witness, destination)
        if has_macos_artifact:
            assert go_manifest is not None and fleet_telemetry_manifest is not None
            validate_macos_artifacts(
                repo,
                artifact_witnesses,
                package_version,
                go_manifest,
                sha256(stage / "go-proxy-evidence/go-component-manifest.json"),
                fleet_telemetry_manifest,
                sha256(stage / "fleet-telemetry-evidence/fleet-telemetry-component-manifest.json"),
                staged_legal_bundle,
                sha256(stage / "dependency-legal/legal-bundle-manifest.json"),
                stage,
            )
        source_name = f"teslatlas-hub-{args.tag}-source.tar.gz"
        source_path = stage / source_name
        archive(repo, commit, args.tag, source_path)
        metadata = cargo_metadata(repo)
        write_json(stage / "cargo-metadata.json", portable_cargo_metadata(metadata, repo))
        spdx, inventory, notices = sbom_and_notices(metadata, repo)
        expected_rust_legal = {
            "RUST_THIRD_PARTY_NOTICES.generated.md": notices.encode(),
            "rust-dependency-inventory.json": (
                json.dumps(inventory, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
            ).encode(),
            "rust-sbom.spdx.json": (
                json.dumps(spdx, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
            ).encode(),
        }
        for name, expected_data in expected_rust_legal.items():
            if staged_legal_bundle.get(name) != expected_data:
                raise GateError(f"dependency legal bundle Rust material mismatch: {name}")
        spdx["documentNamespace"] = f"urn:teslatlas:sbom:{args.tag}:{commit}"
        spdx["creationInfo"]["created"] = created
        write_json(stage / "sbom.spdx.json", spdx)
        write_json(stage / "dependency-inventory.json", inventory)
        (stage / "THIRD_PARTY_NOTICES.generated.md").write_text(notices, encoding="utf-8")

        artifact_records = [
            {"path": witness.path.name, "size": witness.size, "sha256": witness.digest}
            for witness in sorted(artifact_witnesses, key=lambda item: item.path.name)
        ]
        generated_names = [source_name, "cargo-metadata.json", "sbom.spdx.json",
                           "dependency-inventory.json", "THIRD_PARTY_NOTICES.generated.md"]
        for name in RUST_SOURCE_EVIDENCE_NAMES:
            generated_names.append(f"rust-source-evidence/{name}")
        if go_evidence_witnesses:
            for name in GO_EVIDENCE_NAMES:
                generated_names.append(f"go-proxy-evidence/{name}")
        if fleet_telemetry_evidence_witnesses:
            for name in FLEET_TELEMETRY_EVIDENCE_NAMES:
                generated_names.append(f"fleet-telemetry-evidence/{name}")
        for architecture in sorted(required_linux_architectures):
            for name in GO_EVIDENCE_NAMES:
                generated_names.append(
                    f"linux-sidecar-evidence/{architecture}/go-proxy/{name}"
                )
            for name in FLEET_TELEMETRY_EVIDENCE_NAMES:
                generated_names.append(
                    f"linux-sidecar-evidence/{architecture}/fleet-telemetry/{name}"
                )
        for attestation in debian_attestations:
            generated_names.extend(
                [
                    f"debian-native-attestations/{attestation.architecture}/"
                    f"{DEBIAN_ATTESTATION_RECEIPT_NAME}",
                    f"debian-native-attestations/{attestation.architecture}/"
                    f"{DEBIAN_ATTESTATION_SIGNATURE_NAME}",
                ]
            )
        if debian_attestation_records:
            generated_names.append(DEBIAN_ATTESTATION_PUBLIC_KEY_NAME)
        for name in sorted(staged_legal_bundle):
            generated_names.append(f"dependency-legal/{name}")
        if release_receipt_witnesses:
            generated_names.append("macos-release-receipts/SHA256SUMS")
            generated_names.extend(
                f"macos-release-receipts/notary-logs/{witness.path.name}"
                for witness in release_receipt_witnesses
                if witness.path.parent.name == "notary-logs"
            )
        evidence_prefix = f"teslatlas-hub-{args.tag}-evidence"
        manifest = {
            "schema": SCHEMA,
            "tag": args.tag,
            "commit": commit,
            "artifacts": artifact_records,
            "generated": [
                {
                    "path": (
                        name
                        if name == source_name
                        else f"{evidence_prefix}/{name}"
                    ),
                    "sha256": sha256(stage / name),
                }
                for name in generated_names
            ],
        }
        if go_manifest is not None:
            manifest["go_proxy_evidence"] = {
                "target": go_manifest["target"],
                "subject": go_manifest["subject"],
                "manifest_sha256": sha256(stage / "go-proxy-evidence/go-component-manifest.json"),
            }
        if fleet_telemetry_manifest is not None:
            manifest["fleet_telemetry_evidence"] = {
                "target": fleet_telemetry_manifest["subject"]["target"],
                "subject": fleet_telemetry_manifest["subject"],
                "manifest_sha256": sha256(stage / "fleet-telemetry-evidence/fleet-telemetry-component-manifest.json"),
            }
        if linux_sidecar_records:
            manifest["linux_sidecar_evidence"] = linux_sidecar_records
        manifest["dependency_legal_bundle"] = {
            "manifest_sha256": sha256(stage / "dependency-legal/legal-bundle-manifest.json"),
            "contains_sidecar_material": bundle_has_sidecars,
        }
        assert rust_source_manifest is not None
        manifest["rust_source_evidence"] = {
            "manifest_sha256": sha256(
                stage / "rust-source-evidence/rust-source-evidence-manifest.json"
            ),
            "vendor_archive_sha256": rust_source_manifest["vendor_archive_sha256"],
            "vendor_archive_size": rust_source_manifest["vendor_archive_size"],
            "offline_locked_build": rust_source_manifest["offline_locked_build"],
        }
        if debian_attestation_records:
            manifest["debian_native_attestations"] = debian_attestation_records
        write_json(stage / "artifact-manifest.json", manifest)

        public_key = stage / "provenance-public-key.pem"
        run(["openssl", "pkey", "-in", str(args.signing_key.resolve()), "-pubout", "-out", str(public_key)], repo)
        public_key_digest = sha256(public_key)
        if public_key_digest.lower() != args.public_key_sha256.lower():
            raise GateError("public-key-sha256 does not match the supplied signing key")
        provenance = {"schema": SCHEMA, "tag": args.tag, "commit": commit,
                      "tag_signature": {"verified": True, "signer_fingerprint": tag_signer},
                      "created": created,
                      "source_archive": {"path": source_name, "sha256": sha256(source_path)},
                      "artifact_manifest": {"path": f"{evidence_prefix}/artifact-manifest.json",
                                             "sha256": sha256(stage / "artifact-manifest.json")},
                      "sbom_sha256": sha256(stage / "sbom.spdx.json"),
                      "dependency_inventory_sha256": sha256(stage / "dependency-inventory.json"),
                      "notices_sha256": sha256(stage / "THIRD_PARTY_NOTICES.generated.md"),
                      "signing": {"algorithm": "openssl-sha256", "public_key_sha256": public_key_digest}}
        if go_manifest is not None:
            provenance["go_proxy_evidence"] = {
                "target": go_manifest["target"],
                "subject": go_manifest["subject"],
                "manifest_sha256": sha256(stage / "go-proxy-evidence/go-component-manifest.json"),
            }
        if fleet_telemetry_manifest is not None:
            provenance["fleet_telemetry_evidence"] = {
                "target": fleet_telemetry_manifest["subject"]["target"],
                "subject": fleet_telemetry_manifest["subject"],
                "manifest_sha256": sha256(stage / "fleet-telemetry-evidence/fleet-telemetry-component-manifest.json"),
            }
        if linux_sidecar_records:
            provenance["linux_sidecar_evidence"] = linux_sidecar_records
        provenance["dependency_legal_bundle"] = {
            "manifest_sha256": sha256(stage / "dependency-legal/legal-bundle-manifest.json"),
            "contains_sidecar_material": bundle_has_sidecars,
        }
        provenance["rust_source_evidence"] = manifest["rust_source_evidence"]
        if debian_attestation_records:
            provenance["debian_native_attestations"] = debian_attestation_records
        provenance_path = stage / "provenance.json"
        write_json(provenance_path, provenance)
        signature = stage / "provenance.sig"
        run(["openssl", "dgst", "-sha256", "-sign", str(args.signing_key.resolve()),
             "-out", str(signature), str(provenance_path)], repo)
        run(["openssl", "dgst", "-sha256", "-verify", str(public_key), "-signature", str(signature),
             str(provenance_path)], repo)

        assert_candidate_unchanged(
            repo, args.tag, commit, artifacts, ignored_paths, stage
        )
        for witness in artifact_witnesses:
            verify_artifact_unchanged(repo, witness)
        for witness in go_evidence_witnesses:
            verify_go_evidence_unchanged(repo, witness)
        for witness in fleet_telemetry_evidence_witnesses:
            verify_artifact_unchanged(repo, witness)
        for _, witnesses, _ in linux_go_evidence.values():
            for witness in witnesses:
                verify_go_evidence_unchanged(repo, witness)
        for _, witnesses, _ in linux_fleet_evidence.values():
            for witness in witnesses:
                verify_artifact_unchanged(repo, witness)
        for witness in legal_bundle_witnesses:
            verify_artifact_unchanged(repo, witness)
        for witness in rust_source_evidence_witnesses:
            verify_artifact_unchanged(repo, witness)
        for witness in release_receipt_witnesses:
            verify_artifact_unchanged(repo, witness)
        for attestation in debian_attestations:
            verify_artifact_unchanged(repo, attestation.receipt)
            verify_artifact_unchanged(repo, attestation.signature)
            verify_debian_attestation_structure(attestation)
        if attestation_key is not None:
            verify_artifact_unchanged(repo, attestation_key)
        verify_artifact_unchanged(repo, release_signing_key)
        if release_bundle_path is not None:
            assert go_evidence_dir is not None
            verify_macos_release_bundle_structure(
                release_bundle_path,
                artifact_witnesses,
                go_evidence_dir,
                fleet_telemetry_evidence_dir,
                legal_bundle_dir,
                release_receipt_witnesses,
            )
        publish_flat_release_set(
            repo,
            stage,
            output,
            args.tag,
            created,
            tag_signer,
            source_name,
            artifact_witnesses,
            [
                (release_signing_key, "RELEASE_SIGNING_KEY.asc"),
                *(
                    [(attestation_key, DEBIAN_ATTESTATION_PUBLIC_KEY_NAME)]
                    if attestation_key is not None
                    else []
                ),
            ],
        )
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise
    print(output)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as exc:
        print(f"release-evidence: BLOCKED: {exc}", file=sys.stderr)
        raise SystemExit(1)
