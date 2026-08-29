#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Generate or verify a signed native Debian release attestation."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import selectors
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
from typing import Any


SCHEMA = "teslatlas.debian-native-release-attestation/v1"
RECEIPT_NAME = "debian-native-attestation.json"
SIGNATURE_NAME = "debian-native-attestation.sig"
GENERATOR_PATH = "scripts/debian-release-attestation.py"
PACKAGE_NAME = "teslatlas-hub"
HUB_PATH = "usr/bin/teslatlas-hub"
GO_PROXY_PATH = "usr/lib/teslatlas-hub/tesla-http-proxy"
FLEET_TELEMETRY_PATH = "usr/lib/teslatlas-hub/fleet-telemetry"
SIDECAR_SUBJECTS = {
    "go_proxy": (GO_PROXY_PATH, "tesla-http-proxy"),
    "fleet_telemetry": (FLEET_TELEMETRY_PATH, "fleet-telemetry"),
}
MAX_PACKAGE_BYTES = 256 * 1024 * 1024
MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_FILE_BYTES = 128 * 1024 * 1024
MAX_LEGAL_BYTES = 16 * 1024 * 1024
MAX_RECEIPT_BYTES = 1024 * 1024
MAX_MEMBERS = 4096
MAX_COMMAND_OUTPUT = 64 * 1024
TAG_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$")
FINGERPRINT_RE = re.compile(r"^(?:[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64})$")
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
COMMIT_RE = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
SAFE_FILENAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._+~-]{0,190}\.deb$")
ED25519_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")

LEGAL_FILES = {
    "usr/share/doc/teslatlas-hub/copyright": "LICENSE",
    "usr/share/doc/teslatlas-hub/NOTICE": "NOTICE",
    "usr/share/doc/teslatlas-hub/THIRD_PARTY_NOTICES.md": "THIRD_PARTY_NOTICES.md",
    "usr/share/doc/teslatlas-hub/PROVENANCE.md": "PROVENANCE.md",
    "usr/share/doc/teslatlas-hub/ADDITIONAL_TERMS.md": "ADDITIONAL_TERMS.md",
    "usr/share/doc/teslatlas-hub/SOURCE_AVAILABILITY.md": "SOURCE_AVAILABILITY.md",
    "usr/share/doc/teslatlas-hub/RELEASE_VERIFICATION.md": "RELEASE_VERIFICATION.md",
}
BASE_STATIC_FILES = {
    "lib/systemd/system/teslatlas-hub.service": "packaging/linux/teslatlas-hub.service",
    "lib/systemd/system/teslatlas-hub-terminal-failure.target":
        "packaging/linux/teslatlas-hub-terminal-failure.target",
    "etc/teslatlas-hub/config.toml": "packaging/linux/config.toml",
    **LEGAL_FILES,
}
FLEET_STATIC_FILES = {
    "lib/systemd/system/teslatlas-command-proxy.service":
        "packaging/linux/teslatlas-command-proxy.service",
    "lib/systemd/system/teslatlas-fleet-telemetry.service":
        "packaging/linux/teslatlas-fleet-telemetry.service",
    "etc/teslatlas-hub/command-proxy.env": "packaging/linux/command-proxy.env",
    "etc/teslatlas-hub/fleet-telemetry.json": "packaging/linux/fleet-telemetry.json",
}
BASE_DEPENDENCY_LEGAL = {
    "RUST_THIRD_PARTY_NOTICES.generated.md",
    "rust-dependency-inventory.json",
    "rust-sbom.spdx.json",
}
FLEET_DEPENDENCY_LEGAL = {
    "FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md",
    "GO_THIRD_PARTY_NOTICES.generated.md",
    "fleet-telemetry-bridge-lock.json",
    "fleet-telemetry-dependency-inventory.json",
    "fleet-telemetry-legal-lock.json",
    "fleet-telemetry-license-material.tar.gz",
    "fleet-telemetry-sbom.spdx.json",
    "go-dependency-inventory.json",
    "go-sbom.spdx.json",
}
DEPENDENCY_PREFIX = "usr/share/doc/teslatlas-hub/dependency-legal/"
DEPENDENCY_MANIFEST = DEPENDENCY_PREFIX + "legal-bundle-manifest.json"
SIDECAR_PATHS = {
    GO_PROXY_PATH,
    FLEET_TELEMETRY_PATH,
    "usr/share/doc/teslatlas-hub/SIDECAR_SHA256SUMS",
    "usr/share/doc/teslatlas-hub/SIDECAR_BUILD_LOCK",
}
MAINTAINER_SCRIPTS = {
    "preinst": "packaging/linux/preinst",
    "postinst": "packaging/linux/postinst",
    "prerm": "packaging/linux/prerm",
    "postrm": "packaging/linux/postrm",
}
TOOL_COMMANDS = {
    "cargo": ["cargo", "-Vv"],
    "cc": ["cc", "--version"],
    "dpkg": ["dpkg", "--version"],
    "dpkg_deb": ["dpkg-deb", "--version"],
    "openssl": ["openssl", "version", "-a"],
    "python": [sys.executable, "--version"],
    "readelf": ["readelf", "--version"],
    "rustc": ["rustc", "-Vv"],
}


class GateError(RuntimeError):
    pass


@dataclass(frozen=True)
class FileWitness:
    path: Path
    device: int
    inode: int
    size: int
    mtime_ns: int
    ctime_ns: int
    sha256: str
    data: bytes


@dataclass(frozen=True)
class TarEntry:
    path: str
    mode: int
    uid: int
    gid: int
    data: bytes

    @property
    def sha256(self) -> str:
        return hashlib.sha256(self.data).hexdigest()


def fail(message: str) -> None:
    raise GateError(message)


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            fail(f"duplicate JSON key: {key}")
        value[key] = item
    return value


def canonical_json(value: object) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True)
        + "\n"
    ).encode("ascii")


def read_regular(path: Path, label: str, maximum: int) -> FileWitness:
    path = Path(os.path.abspath(path))
    try:
        before = os.lstat(path)
    except OSError as exc:
        raise GateError(f"{label} is missing: {path}") from exc
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_nlink != 1
        or before.st_size <= 0
        or before.st_size > maximum
    ):
        fail(f"{label} must be a bounded regular non-symlink file: {path}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise GateError(f"cannot safely open {label}: {path}") from exc
    try:
        opened = os.fstat(descriptor)
        if (
            (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)
            or opened.st_nlink != 1
        ):
            fail(f"{label} changed while opening: {path}")
        digest = hashlib.sha256()
        data = bytearray()
        while len(data) <= maximum:
            block = os.read(descriptor, min(1024 * 1024, maximum + 1 - len(data)))
            if not block:
                break
            digest.update(block)
            data.extend(block)
        after = os.fstat(descriptor)
        if len(data) > maximum:
            fail(f"{label} exceeds the size limit: {path}")
        if (
            len(data) != opened.st_size
            or after.st_size != opened.st_size
            or after.st_nlink != 1
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
        ):
            fail(f"{label} changed while reading: {path}")
        return FileWitness(
            path=path,
            device=opened.st_dev,
            inode=opened.st_ino,
            size=opened.st_size,
            mtime_ns=opened.st_mtime_ns,
            ctime_ns=opened.st_ctime_ns,
            sha256=digest.hexdigest(),
            data=bytes(data),
        )
    finally:
        os.close(descriptor)


def reread_matches(witness: FileWitness, label: str, maximum: int) -> None:
    current = read_regular(witness.path, label, maximum)
    if (
        current.device != witness.device
        or current.inode != witness.inode
        or current.size != witness.size
        or current.mtime_ns != witness.mtime_ns
        or current.ctime_ns != witness.ctime_ns
        or current.sha256 != witness.sha256
    ):
        fail(f"{label} changed during attestation: {witness.path}")


def real_directory(path: Path, label: str) -> Path:
    path = Path(os.path.abspath(path))
    try:
        metadata = os.lstat(path)
    except OSError as exc:
        raise GateError(f"{label} is missing: {path}") from exc
    if not stat.S_ISDIR(metadata.st_mode):
        fail(f"{label} must be a real non-symlink directory: {path}")
    return path.resolve()


def run(
    command: list[str],
    cwd: Path,
    label: str,
    *,
    timeout: int = 60,
    maximum: int = MAX_COMMAND_OUTPUT,
) -> subprocess.CompletedProcess[bytes]:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            capture_output=True,
            timeout=timeout,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise GateError(f"{label} could not complete") from exc
    if len(result.stdout) > maximum or len(result.stderr) > maximum:
        fail(f"{label} produced excessive output")
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        fail(f"{label} failed" + (f": {detail}" if detail else ""))
    return result


def safe_text(data: bytes, label: str) -> str:
    try:
        value = data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise GateError(f"{label} is not UTF-8") from exc
    if "\0" in value:
        fail(f"{label} contains NUL")
    return value


def git_output(repo: Path, arguments: list[str], label: str) -> str:
    result = run(["git", *arguments], repo, label)
    return safe_text(result.stdout, label)


def git_file(repo: Path, tag: str, path: str) -> bytes:
    if not TAG_RE.fullmatch(tag) or not path or path.startswith("/") or ".." in PurePosixPath(path).parts:
        fail("unsafe tagged source path")
    return run(
        ["git", "show", f"{tag}:{path}"],
        repo,
        f"read tagged source {path}",
        maximum=MAX_LEGAL_BYTES,
    ).stdout


def cargo_version(repo: Path, tag: str) -> str:
    try:
        manifest = tomllib.loads(safe_text(git_file(repo, tag, "Cargo.toml"), "Cargo.toml"))
        version = manifest["package"]["version"]
    except (tomllib.TOMLDecodeError, KeyError, TypeError) as exc:
        raise GateError("tagged Cargo package version is unreadable") from exc
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        fail("tagged Cargo package version is not a supported SemVer")
    return version


def debian_version(version: str) -> str:
    if "-" not in version:
        return f"{version}-1"
    base, prerelease = version.split("-", 1)
    return f"{base}~{prerelease}-1"


def verify_tag(
    repo: Path,
    tag: str,
    expected_signer: str,
    *,
    require_clean_head: bool,
) -> tuple[str, str, str]:
    if not TAG_RE.fullmatch(tag):
        fail("tag contains unsafe characters")
    if not FINGERPRINT_RE.fullmatch(expected_signer):
        fail("tag signer fingerprint must be 40 or 64 hexadecimal characters")
    expected_signer = expected_signer.upper()
    if git_output(repo, ["cat-file", "-t", tag], "read tag type").strip() != "tag":
        fail("release tag must be a signed annotated tag")
    commit = git_output(repo, ["rev-parse", f"{tag}^{{commit}}"], "resolve tag").strip()
    if not COMMIT_RE.fullmatch(commit):
        fail("tag commit has an unsupported object identifier")
    verification = run(["git", "verify-tag", "--raw", tag], repo, "verify signed tag")
    status_text = safe_text(
        verification.stdout + b"\n" + verification.stderr,
        "tag verification status",
    )
    signers = {
        match.group(1).upper()
        for match in re.finditer(
            r"^\[GNUPG:\] VALIDSIG ([0-9A-F]+)\b", status_text, re.MULTILINE
        )
    }
    if signers != {expected_signer}:
        fail("tag signer does not match the pinned maintainer fingerprint")
    version = cargo_version(repo, tag)
    if tag != f"v{version}":
        fail(f"release tag does not match Cargo version; expected v{version}")
    if require_clean_head:
        status = git_output(
            repo,
            ["status", "--porcelain=v1", "--untracked-files=all"],
            "inspect candidate checkout",
        )
        if status:
            fail("candidate checkout is not clean")
        head = git_output(repo, ["rev-parse", "HEAD"], "resolve candidate HEAD").strip()
        if head != commit:
            fail("candidate HEAD does not match the signed tag commit")
    timestamp = git_output(
        repo, ["show", "-s", "--format=%cI", commit], "read commit timestamp"
    ).strip()
    try:
        created = datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
    except ValueError as exc:
        raise GateError("tag commit timestamp is invalid") from exc
    return (
        commit,
        created.astimezone(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z"),
        expected_signer,
    )


def ar_members(data: bytes) -> dict[str, bytes]:
    if not data.startswith(b"!<arch>\n"):
        fail("Debian package is not an ar archive")
    offset = 8
    members: dict[str, bytes] = {}
    while offset < len(data):
        if len(data) - offset < 60:
            fail("Debian ar archive has a truncated member header")
        header = data[offset:offset + 60]
        if header[58:60] != b"`\n":
            fail("Debian ar archive has an invalid member header")
        try:
            raw_name = header[:16].decode("ascii").strip()
            size = int(header[48:58].decode("ascii").strip(), 10)
        except (UnicodeDecodeError, ValueError) as exc:
            raise GateError("Debian ar archive metadata is invalid") from exc
        if raw_name.startswith(("/", "#1/")):
            fail("Debian ar archive uses an unsupported member name")
        name = raw_name[:-1] if raw_name.endswith("/") else raw_name
        if "/" in name or "\\" in name:
            fail("Debian ar archive uses an unsupported member name")
        if not name or name in members or size < 0:
            fail("Debian ar archive contains invalid duplicate members")
        start = offset + 60
        end = start + size
        if end > len(data):
            fail("Debian ar archive has a truncated member")
        members[name] = data[start:end]
        if size % 2 and (end >= len(data) or data[end:end + 1] != b"\n"):
            fail("Debian ar archive has invalid member padding")
        offset = end + (size % 2)
    if offset != len(data):
        fail("Debian ar archive has invalid trailing data")
    control = [name for name in members if name.startswith("control.tar.")]
    payload = [name for name in members if name.startswith("data.tar.")]
    if (
        members.get("debian-binary") != b"2.0\n"
        or len(control) != 1
        or len(payload) != 1
        or set(members) != {"debian-binary", control[0], payload[0]}
    ):
        fail("Debian package must contain exactly version, control, and data members")
    return members


def decoded_tar(data: bytes, archive_name: str, label: str) -> bytes:
    if not archive_name.endswith(".zst"):
        return data
    try:
        from compression import zstd  # type: ignore[attr-defined]
    except ImportError:
        zstd_binary = shutil.which("zstd")
        if zstd_binary is None:
            fail(f"{label} uses zstd but no bounded decoder is available")
        with tempfile.TemporaryDirectory(prefix="teslatlas-deb-zstd-") as raw:
            source = Path(raw) / "archive.tar.zst"
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
                while len(output) <= MAX_ARCHIVE_BYTES:
                    block = process.stdout.read(
                        min(1024 * 1024, MAX_ARCHIVE_BYTES + 1 - len(output))
                    )
                    if not block:
                        break
                    output.extend(block)
                if len(output) > MAX_ARCHIVE_BYTES:
                    process.kill()
                    process.wait()
                    fail(f"{label} expands beyond the safety limit")
                if process.wait(timeout=60) != 0:
                    fail(f"{label} zstd decompression failed")
            except subprocess.TimeoutExpired as exc:
                process.kill()
                process.wait()
                raise GateError(f"{label} zstd decompression timed out") from exc
            finally:
                process.stdout.close()
            return bytes(output)
    try:
        with zstd.open(io.BytesIO(data), "rb") as source:
            output = source.read(MAX_ARCHIVE_BYTES + 1)
    except (OSError, EOFError) as exc:
        raise GateError(f"{label} zstd decompression failed") from exc
    if len(output) > MAX_ARCHIVE_BYTES:
        fail(f"{label} expands beyond the safety limit")
    return output


def normalized_member_path(raw: str, label: str) -> str | None:
    while raw.startswith("./"):
        raw = raw[2:]
    if raw in {"", "."}:
        return None
    path = PurePosixPath(raw)
    if (
        not raw
        or path.is_absolute()
        or any(part in {"", ".", ".."} for part in path.parts)
    ):
        fail(f"{label} contains an unsafe path")
    return path.as_posix()


def tar_entries(data: bytes, archive_name: str, label: str) -> dict[str, TarEntry]:
    data = decoded_tar(data, archive_name, label)
    try:
        archive = tarfile.open(fileobj=io.BytesIO(data), mode="r:*")
    except (tarfile.TarError, OSError) as exc:
        raise GateError(f"{label} is not a readable tar archive") from exc
    entries: dict[str, TarEntry] = {}
    seen: set[str] = set()
    total = 0
    count = 0
    try:
        for member in archive:
            count += 1
            if count > MAX_MEMBERS:
                fail(f"{label} contains too many members")
            name = normalized_member_path(member.name, label)
            if name is None:
                if member.isdir():
                    continue
                fail(f"{label} contains an unsafe root member")
            if name in seen:
                fail(f"{label} contains a duplicate path: {name}")
            seen.add(name)
            if member.isdir():
                continue
            if not member.isfile():
                fail(f"{label} contains a non-regular member: {name}")
            if member.mode & 0o7000:
                fail(f"{label} contains a privileged file mode: {name}")
            if member.uid != 0 or member.gid != 0:
                fail(f"{label} contains a non-root-owned file: {name}")
            maximum = MAX_LEGAL_BYTES if "share/doc" in name else MAX_FILE_BYTES
            if member.size <= 0 or member.size > maximum:
                fail(f"{label} contains an invalid file size: {name}")
            total += member.size
            if total > MAX_ARCHIVE_BYTES:
                fail(f"{label} expands beyond the safety limit")
            source = archive.extractfile(member)
            if source is None:
                fail(f"{label} cannot read member: {name}")
            content = source.read(maximum + 1)
            if len(content) != member.size:
                fail(f"{label} contains a truncated member: {name}")
            entries[name] = TarEntry(
                path=name,
                mode=member.mode & 0o7777,
                uid=member.uid,
                gid=member.gid,
                data=content,
            )
    except (tarfile.TarError, OSError) as exc:
        raise GateError(f"{label} cannot be read safely") from exc
    finally:
        archive.close()
    return entries


def control_fields(data: bytes) -> dict[str, str]:
    text = safe_text(data, "Debian control file")
    fields: dict[str, str] = {}
    current: str | None = None
    ended = False
    for line in text.splitlines():
        if not line:
            if fields:
                ended = True
            current = None
            continue
        if ended:
            fail("Debian control file contains multiple paragraphs")
        if line.startswith((" ", "\t")):
            if current is None:
                fail("Debian control file has an orphan continuation")
            fields[current] += "\n" + line[1:]
            continue
        if ":" not in line:
            fail("Debian control file is malformed")
        name, value = line.split(":", 1)
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]*", name) or name in fields:
            fail("Debian control file has an invalid or duplicate field")
        fields[name] = value.strip()
        current = name
    return fields


def validate_elf(data: bytes, architecture: str, label: str) -> None:
    if len(data) < 64 or data[:7] != b"\x7fELF\x02\x01\x01":
        fail(f"{label} is not a little-endian ELF64 binary")
    os_abi = data[7]
    elf_type = struct.unpack_from("<H", data, 16)[0]
    machine = struct.unpack_from("<H", data, 18)[0]
    version = struct.unpack_from("<I", data, 20)[0]
    expected = {"amd64": 62, "arm64": 183}.get(architecture)
    if expected is None or machine != expected:
        fail(f"{label} architecture does not match Debian control metadata")
    if os_abi not in {0, 3} or elf_type not in {2, 3} or version != 1:
        fail(f"{label} has an unsupported ELF executable header")


def parse_json(data: bytes, label: str) -> Any:
    try:
        return json.loads(
            safe_text(data, label),
            object_pairs_hook=reject_duplicate_keys,
        )
    except json.JSONDecodeError as exc:
        raise GateError(f"{label} is invalid JSON") from exc


def validate_dependency_legal(
    payload: dict[str, TarEntry],
    fleet: bool,
    tagged_cargo_lock: bytes,
) -> None:
    expected_names = set(BASE_DEPENDENCY_LEGAL)
    if fleet:
        expected_names.update(FLEET_DEPENDENCY_LEGAL)
    component_paths = {DEPENDENCY_PREFIX + name for name in expected_names}
    manifest_entry = payload[DEPENDENCY_MANIFEST]
    manifest = parse_json(manifest_entry.data, "dependency legal manifest")
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema", "cargo_lock_sha256", "contains_sidecar_material", "components"
    }:
        fail("dependency legal manifest has an unexpected schema shape")
    if manifest["schema"] != "teslatlas.dependency-legal-bundle/v1":
        fail("dependency legal manifest schema is unsupported")
    if manifest["cargo_lock_sha256"] != hashlib.sha256(tagged_cargo_lock).hexdigest():
        fail("dependency legal manifest does not bind the tagged Cargo.lock")
    if manifest["contains_sidecar_material"] is not fleet:
        fail("dependency legal manifest sidecar state is inconsistent")
    records = manifest["components"]
    if not isinstance(records, list) or len(records) != len(expected_names):
        fail("dependency legal manifest component set is incomplete")
    observed: set[str] = set()
    for record in records:
        if not isinstance(record, dict) or set(record) != {"path", "sha256", "size"}:
            fail("dependency legal manifest component record is invalid")
        name = record["path"]
        if not isinstance(name, str) or name not in expected_names or name in observed:
            fail("dependency legal manifest component name is invalid or duplicated")
        observed.add(name)
        entry = payload[DEPENDENCY_PREFIX + name]
        if record["sha256"] != entry.sha256 or record["size"] != len(entry.data):
            fail(f"dependency legal manifest component mismatch: {name}")
        if isinstance(record["size"], bool) or not isinstance(record["size"], int):
            fail(f"dependency legal manifest component size is invalid: {name}")
        if name.endswith(".json"):
            if not isinstance(parse_json(entry.data, f"dependency legal component {name}"), dict):
                fail(f"dependency legal component is not a JSON object: {name}")
    if observed != expected_names or component_paths - set(payload):
        fail("dependency legal payload is incomplete")


def parse_digest_lines(data: bytes, label: str) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in safe_text(data, label).splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9._+-]+)", line)
        if match is None or match.group(2) in result:
            fail(f"{label} is malformed or duplicated")
        result[match.group(2)] = match.group(1)
    if not result:
        fail(f"{label} is empty")
    return result


def validate_sidecars(payload: dict[str, TarEntry], architecture: str, tag: str, repo: Path) -> None:
    proxy = payload[GO_PROXY_PATH]
    receiver = payload[FLEET_TELEMETRY_PATH]
    validate_elf(proxy.data, architecture, "Tesla command proxy")
    validate_elf(receiver.data, architecture, "Fleet Telemetry receiver")
    sums = parse_digest_lines(
        payload["usr/share/doc/teslatlas-hub/SIDECAR_SHA256SUMS"].data,
        "sidecar checksum file",
    )
    expected = {
        "tesla-http-proxy": proxy.sha256,
        "fleet-telemetry": receiver.sha256,
    }
    if sums != expected:
        fail("sidecar checksums do not match packaged binaries")
    if proxy.mode != 0o755 or receiver.mode != 0o755:
        fail("packaged sidecar executable mode is invalid")
    for path in (
        "usr/share/doc/teslatlas-hub/SIDECAR_SHA256SUMS",
        "usr/share/doc/teslatlas-hub/SIDECAR_BUILD_LOCK",
    ):
        if payload[path].mode != 0o644:
            fail(f"packaged sidecar evidence mode is invalid: {path}")
    lock = git_file(repo, tag, "packaging/linux/sidecar-sha256.lock")
    if payload["usr/share/doc/teslatlas-hub/SIDECAR_BUILD_LOCK"].data != lock:
        fail("packaged sidecar lock does not match the signed tag")
    selected: tuple[str, str] | None = None
    seen: set[str] = set()
    for raw in safe_text(lock, "sidecar build lock").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if (
            len(parts) != 3
            or parts[0] not in {"amd64", "arm64"}
            or parts[0] in seen
            or not DIGEST_RE.fullmatch(parts[1])
            or not DIGEST_RE.fullmatch(parts[2])
        ):
            fail("signed sidecar build lock is invalid")
        seen.add(parts[0])
        if parts[0] == architecture:
            selected = (parts[1], parts[2])
    if seen != {"amd64", "arm64"} or selected != (proxy.sha256, receiver.sha256):
        fail("packaged sidecars do not match the signed architecture lock")


def validate_control_archive(
    control: dict[str, TarEntry],
    version: str,
    architecture: str,
    fleet: bool,
    repo: Path,
    tag: str,
) -> None:
    required = {"control", "conffiles", *MAINTAINER_SCRIPTS}
    if set(control) not in {frozenset(required), frozenset(required | {"md5sums"})}:
        fail("Debian control archive file set is incomplete or unexpected")
    fields = control_fields(control["control"].data)
    expected_fields = {
        "Package", "Version", "Section", "Priority", "Architecture",
        "Depends", "Maintainer", "Description",
    }
    if set(fields) != expected_fields:
        fail("Debian control metadata field set is incomplete or unexpected")
    if fields["Package"] != PACKAGE_NAME:
        fail("Debian package name is not teslatlas-hub")
    if fields["Version"] != debian_version(version):
        fail("Debian package version does not match Cargo version")
    if fields["Architecture"] != architecture:
        fail("Debian package architecture is inconsistent")
    if (
        fields["Section"] != "utils"
        or fields["Priority"] != "optional"
        or fields["Maintainer"] != "Magrathean UK Ltd <contact@magrathean.uk>"
        or not fields["Description"].startswith("Self-hosted multi-car Tesla telemetry hub")
        or not re.fullmatch(r"[A-Za-z0-9+().,:|<>=~\- ]{1,2048}", fields["Depends"])
        or not fields["Depends"].startswith("adduser, ca-certificates, systemd")
    ):
        fail("Debian control metadata does not match the release policy")
    conffiles = [line for line in safe_text(control["conffiles"].data, "conffiles").splitlines() if line]
    expected_conffiles = ["/etc/teslatlas-hub/config.toml"]
    if fleet:
        expected_conffiles.extend([
            "/etc/teslatlas-hub/command-proxy.env",
            "/etc/teslatlas-hub/fleet-telemetry.json",
        ])
    if conffiles != expected_conffiles:
        fail("Debian conffiles do not match the packaged feature set")
    for name in set(control) - set(MAINTAINER_SCRIPTS):
        if control[name].mode != 0o644:
            fail(f"Debian control file mode is invalid: {name}")
    for packaged, source in MAINTAINER_SCRIPTS.items():
        if control[packaged].data != git_file(repo, tag, source):
            fail(f"Debian maintainer script does not match the signed tag: {packaged}")
        if control[packaged].mode != 0o755:
            fail(f"Debian maintainer script mode is invalid: {packaged}")


def payload_manifest(payload: dict[str, TarEntry]) -> tuple[str, int]:
    records = [
        {
            "mode": entry.mode,
            "path": path,
            "sha256": entry.sha256,
            "size": len(entry.data),
        }
        for path, entry in sorted(payload.items())
    ]
    return hashlib.sha256(canonical_json(records)).hexdigest(), len(records)


def validate_package(
    repo: Path,
    tag: str,
    version: str,
    package: FileWitness,
    expected_architecture: str,
) -> dict[str, Any]:
    if not SAFE_FILENAME_RE.fullmatch(package.path.name):
        fail("Debian package filename is unsafe or unsupported")
    members = ar_members(package.data)
    control_name = next(name for name in members if name.startswith("control.tar."))
    data_name = next(name for name in members if name.startswith("data.tar."))
    control = tar_entries(members[control_name], control_name, "Debian control archive")
    payload = tar_entries(members[data_name], data_name, "Debian data archive")
    if HUB_PATH not in payload:
        fail("Debian package has no Hub binary")
    hub = payload[HUB_PATH]
    if hub.mode != 0o755:
        fail("packaged Hub binary mode is not 0755")
    fields = control_fields(control.get("control", TarEntry("", 0, 0, 0, b"")).data)
    architecture = fields.get("Architecture", "")
    if architecture not in {"amd64", "arm64"} or architecture != expected_architecture:
        fail("Debian package does not match the expected architecture")
    validate_elf(hub.data, architecture, "packaged Hub binary")
    fleet = bool(set(payload) & SIDECAR_PATHS)
    dependency_names = set(BASE_DEPENDENCY_LEGAL)
    if fleet:
        dependency_names.update(FLEET_DEPENDENCY_LEGAL)
    expected_paths = {
        HUB_PATH,
        *BASE_STATIC_FILES,
        DEPENDENCY_MANIFEST,
        *(DEPENDENCY_PREFIX + name for name in dependency_names),
    }
    if fleet:
        expected_paths.update(FLEET_STATIC_FILES)
        expected_paths.update(SIDECAR_PATHS)
    if set(payload) != expected_paths:
        fail("Debian payload file set is incomplete or unexpected")
    for packaged_path, source_path in {
        **BASE_STATIC_FILES,
        **(FLEET_STATIC_FILES if fleet else {}),
    }.items():
        entry = payload[packaged_path]
        if entry.data != git_file(repo, tag, source_path):
            fail(f"Debian payload does not match the signed tag: {packaged_path}")
        if entry.mode != 0o644:
            fail(f"Debian payload file mode is invalid: {packaged_path}")
    for path in expected_paths:
        if path == HUB_PATH or path in {
            "usr/lib/teslatlas-hub/tesla-http-proxy",
            "usr/lib/teslatlas-hub/fleet-telemetry",
        }:
            continue
        if payload[path].mode != 0o644:
            fail(f"Debian payload file mode is invalid: {path}")
    validate_control_archive(control, version, architecture, fleet, repo, tag)
    validate_dependency_legal(payload, fleet, git_file(repo, tag, "Cargo.lock"))
    if fleet:
        validate_sidecars(payload, architecture, tag, repo)
    sidecars: dict[str, dict[str, Any] | None] = {
        "go_proxy": None,
        "fleet_telemetry": None,
    }
    if fleet:
        for key, (path, name) in SIDECAR_SUBJECTS.items():
            entry = payload[path]
            sidecars[key] = {
                "help": None,
                "name": name,
                "path": path,
                "sha256": entry.sha256,
                "size": len(entry.data),
            }
    manifest_sha256, file_count = payload_manifest(payload)
    return {
        "architecture": architecture,
        "binary_path": HUB_PATH,
        "binary_sha256": hub.sha256,
        "binary_size": len(hub.data),
        "binary_version_output": f"teslatlas-hub {version}\n",
        "contains_fleet_sidecars": fleet,
        "debian_version": debian_version(version),
        "package_filename": package.path.name,
        "package_name": PACKAGE_NAME,
        "package_sha256": package.sha256,
        "package_size": package.size,
        "payload_file_count": file_count,
        "payload_manifest_sha256": manifest_sha256,
        "sidecars": sidecars,
    }


def kill_process_group(process: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        try:
            process.kill()
        except ProcessLookupError:
            pass


def bounded_native_command(
    binary: Path,
    arguments: list[str],
    cwd: Path,
    label: str,
    *,
    timeout: int,
) -> tuple[int, bytes, bytes]:
    environment = {
        "HOME": str(cwd),
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": "/usr/bin:/bin",
        "TMPDIR": str(cwd),
        "TZ": "UTC",
    }
    try:
        process = subprocess.Popen(
            [str(binary), *arguments],
            cwd=cwd,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as exc:
        raise GateError(f"{label} could not execute natively") from exc
    assert process.stdout is not None and process.stderr is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    output = {"stdout": bytearray(), "stderr": bytearray()}
    deadline = time.monotonic() + timeout
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                fail(f"{label} timed out")
            for key, _ in selector.select(timeout=min(remaining, 1)):
                block = os.read(key.fileobj.fileno(), 8192)
                if not block:
                    selector.unregister(key.fileobj)
                    continue
                target = output[key.data]
                target.extend(block)
                if len(target) > MAX_COMMAND_OUTPUT:
                    fail(f"{label} produced excessive output")
        try:
            returncode = process.wait(timeout=max(0.1, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            fail(f"{label} did not exit")
    except BaseException:
        if process.poll() is None:
            kill_process_group(process)
            process.wait()
        raise
    finally:
        selector.close()
        process.stdout.close()
        process.stderr.close()
    return returncode, bytes(output["stdout"]), bytes(output["stderr"])


def bounded_binary_version(binary: Path, version: str, cwd: Path) -> None:
    returncode, stdout, stderr = bounded_native_command(
        binary,
        ["--version"],
        cwd,
        "packaged Hub --version",
        timeout=30,
    )
    expected = f"teslatlas-hub {version}\n".encode()
    if returncode != 0 or stdout != expected or stderr:
        fail("packaged Hub --version does not exactly match Cargo version")


def normalized_sidecar_help(binary: Path, key: str, cwd: Path) -> dict[str, Any]:
    if key not in SIDECAR_SUBJECTS:
        fail("unknown packaged sidecar")
    returncode, stdout, raw_stderr = bounded_native_command(
        binary,
        ["--help"],
        cwd,
        f"packaged {SIDECAR_SUBJECTS[key][1]} --help",
        timeout=15,
    )
    if returncode != 0:
        fail(f"packaged {SIDECAR_SUBJECTS[key][1]} --help did not exit zero")
    if stdout:
        fail(f"packaged {SIDECAR_SUBJECTS[key][1]} --help wrote unexpected stdout")
    stderr = safe_text(raw_stderr, f"packaged {SIDECAR_SUBJECTS[key][1]} --help stderr")
    if "\r" in stderr:
        fail(f"packaged {SIDECAR_SUBJECTS[key][1]} --help stderr contains carriage return")
    if key == "go_proxy":
        raw_prefix = f"Usage: {binary} [OPTION...]\n"
        if not stderr.startswith(raw_prefix):
            fail("packaged tesla-http-proxy --help usage is unexpected")
        stderr = "Usage: tesla-http-proxy [OPTION...]\n" + stderr[len(raw_prefix):]
    else:
        first_line, separator, remainder = stderr.partition("\n")
        if (
            not separator
            or re.fullmatch(
                r"(?:\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} )?maxprocs: [^\r\n]{1,512}",
                first_line,
            )
            is None
        ):
            fail("packaged fleet-telemetry --help maxprocs preamble is unexpected")
        raw_prefix = f"Usage of {binary}:\n"
        if not remainder.startswith(raw_prefix):
            fail("packaged fleet-telemetry --help usage is unexpected")
        stderr = (
            "maxprocs: <runtime>\n"
            "Usage of fleet-telemetry:\n"
            + remainder[len(raw_prefix):]
        )
    return {
        "arguments": ["--help"],
        "exit_code": returncode,
        "stderr": stderr,
        "stdout": "",
    }


def extract_executable(
    directory: Path,
    name: str,
    data: bytes,
    expected_sha256: str,
    label: str,
) -> Path:
    binary = directory / name
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(binary, flags, 0o700)
    try:
        view = memoryview(data)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                fail(f"short write while extracting {label}")
            view = view[written:]
        os.fchmod(descriptor, 0o700)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    witness = read_regular(binary, f"securely extracted {label}", MAX_FILE_BYTES)
    if witness.size != len(data) or witness.sha256 != expected_sha256:
        fail(f"securely extracted {label} bytes changed")
    return binary


def execute_packaged_hub(package: FileWitness, subject: dict[str, Any], repo: Path) -> None:
    members = ar_members(package.data)
    data_name = next(name for name in members if name.startswith("data.tar."))
    payload = tar_entries(members[data_name], data_name, "Debian data archive")
    with tempfile.TemporaryDirectory(prefix="teslatlas-native-attestation-") as raw:
        directory = Path(raw)
        os.chmod(directory, 0o700)
        binary = extract_executable(
            directory,
            "teslatlas-hub",
            payload[HUB_PATH].data,
            subject["binary_sha256"],
            "Hub binary",
        )
        bounded_binary_version(binary, subject["binary_version_output"].strip().split(" ", 1)[1], directory)
        if subject["contains_fleet_sidecars"]:
            for key, (path, name) in SIDECAR_SUBJECTS.items():
                record = subject["sidecars"][key]
                if not isinstance(record, dict):
                    fail("packaged sidecar receipt record is missing")
                sidecar = extract_executable(
                    directory,
                    name,
                    payload[path].data,
                    record["sha256"],
                    name,
                )
                record["help"] = normalized_sidecar_help(sidecar, key, directory)


def executable_record(command: list[str], repo: Path, label: str) -> dict[str, str]:
    executable = shutil.which(command[0]) if not os.path.isabs(command[0]) else command[0]
    if executable is None:
        fail(f"required tool is unavailable: {command[0]}")
    resolved = Path(executable).resolve()
    witness = read_regular(resolved, f"{label} executable", MAX_FILE_BYTES)
    result = run([str(resolved), *command[1:]], repo, f"read {label} version")
    combined = result.stdout + result.stderr
    text = safe_text(combined, f"{label} version").strip()
    if not text:
        fail(f"{label} version output is empty")
    return {"executable_sha256": witness.sha256, "version": text}


def linux_os_release(
    etc_path: Path = Path("/etc/os-release"),
    vendor_path: Path = Path("/usr/lib/os-release"),
    *,
    expected_owner_uid: int = 0,
) -> FileWitness:
    etc_path = Path(os.path.abspath(etc_path))
    vendor_path = Path(os.path.abspath(vendor_path))
    try:
        before = os.lstat(etc_path)
    except OSError as exc:
        raise GateError(f"Linux os-release is missing: {etc_path}") from exc
    if stat.S_ISLNK(before.st_mode):
        if before.st_uid != expected_owner_uid:
            fail("Linux os-release symlink is not owned by the expected system user")
        try:
            raw_target = os.readlink(etc_path)
        except OSError as exc:
            raise GateError("cannot safely inspect Linux os-release symlink") from exc
        target = Path(raw_target)
        if not target.is_absolute():
            target = etc_path.parent / target
        normalized_target = Path(os.path.abspath(target))
        if normalized_target != vendor_path:
            fail("Linux os-release symlink does not resolve to /usr/lib/os-release")
        witness = read_regular(vendor_path, "Linux vendor os-release", 64 * 1024)
        try:
            after = os.lstat(etc_path)
            after_target = os.readlink(etc_path)
        except OSError as exc:
            raise GateError("Linux os-release symlink changed while reading") from exc
        if (
            not stat.S_ISLNK(after.st_mode)
            or (after.st_dev, after.st_ino) != (before.st_dev, before.st_ino)
            or after.st_uid != before.st_uid
            or after.st_mtime_ns != before.st_mtime_ns
            or after.st_ctime_ns != before.st_ctime_ns
            or after_target != raw_target
        ):
            fail("Linux os-release symlink changed while reading")
    else:
        witness = read_regular(etc_path, "Linux os-release", 64 * 1024)
    try:
        metadata = os.lstat(witness.path)
    except OSError as exc:
        raise GateError("Linux os-release changed after reading") from exc
    if (
        not stat.S_ISREG(metadata.st_mode)
        or (metadata.st_dev, metadata.st_ino) != (witness.device, witness.inode)
        or metadata.st_size != witness.size
        or metadata.st_mtime_ns != witness.mtime_ns
        or metadata.st_ctime_ns != witness.ctime_ns
    ):
        fail("Linux os-release changed after reading")
    if metadata.st_uid != expected_owner_uid or metadata.st_mode & 0o022:
        fail("Linux os-release must be owner-controlled and not group/world writable")
    return witness


def native_host_and_toolchain(repo: Path, architecture: str) -> tuple[dict[str, str], dict[str, dict[str, str]]]:
    if sys.platform != "linux" or os.uname().sysname != "Linux":
        fail("attestation generation requires native Linux")
    machine = os.uname().machine
    expected_machine = {"amd64": "x86_64", "arm64": "aarch64"}[architecture]
    dpkg_architecture = git_safe_command(["dpkg", "--print-architecture"], repo, "read native Debian architecture")
    if machine != expected_machine or dpkg_architecture != architecture:
        fail("package architecture does not match the native Linux host")
    os_release = linux_os_release()
    facts: dict[str, str] = {}
    for line in safe_text(os_release.data, "Linux os-release").splitlines():
        if "=" not in line or line.startswith("#"):
            continue
        key, value = line.split("=", 1)
        value = value.strip().strip('"').strip("'")
        if key in {"ID", "VERSION_ID"}:
            facts[key] = value
    if facts.get("ID") != "debian" or facts.get("VERSION_ID") != "13":
        fail("native generation requires Debian 13")
    host = {
        "debian_architecture": dpkg_architecture,
        "kernel_release": os.uname().release,
        "machine": machine,
        "os_release_id": facts["ID"],
        "os_release_version_id": facts["VERSION_ID"],
        "sysname": "Linux",
    }
    toolchain = {
        name: executable_record(command, repo, name)
        for name, command in sorted(TOOL_COMMANDS.items())
    }
    return host, toolchain


def git_safe_command(command: list[str], cwd: Path, label: str) -> str:
    return safe_text(run(command, cwd, label).stdout, label).strip()


def ed25519_public_der(key: Path, cwd: Path, *, public: bool) -> bytes:
    command = ["openssl", "pkey", "-in", str(key)]
    if public:
        command.append("-pubin")
    command.extend(["-pubout", "-outform", "DER"])
    data = run(command, cwd, "inspect Ed25519 key", maximum=1024).stdout
    if len(data) != len(ED25519_SPKI_PREFIX) + 32 or not data.startswith(ED25519_SPKI_PREFIX):
        fail("attestation key must be Ed25519 PEM")
    return data


def private_key(path: Path, repo: Path) -> FileWitness:
    witness = read_regular(path, "attestation signing key", 64 * 1024)
    metadata = os.lstat(witness.path)
    if metadata.st_uid != os.geteuid() or metadata.st_mode & 0o077:
        fail("attestation signing key must be owned by the current user and mode 0600 or stricter")
    try:
        witness.path.relative_to(repo)
    except ValueError:
        pass
    else:
        fail("attestation signing key must be outside the source repository")
    ed25519_public_der(witness.path, repo, public=False)
    return witness


def write_new(path: Path, data: bytes, mode: int) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, mode)
    try:
        view = memoryview(data)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                fail(f"short write: {path}")
            view = view[written:]
        os.fchmod(descriptor, mode)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def sign_receipt(receipt: Path, signature: Path, key: Path, cwd: Path) -> None:
    run(
        [
            "openssl", "pkeyutl", "-sign", "-rawin",
            "-inkey", str(key), "-in", str(receipt), "-out", str(signature),
        ],
        cwd,
        "sign Debian native attestation",
    )
    os.chmod(signature, 0o644)
    signed = read_regular(signature, "attestation signature", 1024)
    if signed.size != 64:
        fail("Ed25519 attestation signature is not 64 bytes")


def verify_signature(receipt: Path, signature: Path, public_key: Path, cwd: Path) -> None:
    run(
        [
            "openssl", "pkeyutl", "-verify", "-rawin", "-pubin",
            "-inkey", str(public_key), "-sigfile", str(signature), "-in", str(receipt),
        ],
        cwd,
        "verify Debian native attestation signature",
    )


def expected_generator(repo: Path, tag: str) -> dict[str, str]:
    data = git_file(repo, tag, GENERATOR_PATH)
    return {"path": GENERATOR_PATH, "sha256": hashlib.sha256(data).hexdigest()}


def make_receipt(
    *,
    source: dict[str, str],
    subject: dict[str, Any],
    host: dict[str, str],
    toolchain: dict[str, dict[str, str]],
    generator: dict[str, str],
    created_utc: str,
) -> dict[str, Any]:
    return {
        "created_utc": created_utc,
        "generator": generator,
        "native_host": host,
        "schema": SCHEMA,
        "source": source,
        "subject": subject,
        "toolchain": toolchain,
    }


def require_exact_keys(value: Any, keys: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        fail(f"{label} has an unexpected schema shape")
    return value


def require_text(value: Any, label: str, maximum: int = 65536) -> str:
    if not isinstance(value, str) or not value or len(value) > maximum or "\0" in value:
        fail(f"{label} must be bounded nonempty text")
    return value


def validate_sidecar_subjects(subject: dict[str, Any]) -> None:
    sidecars = require_exact_keys(
        subject["sidecars"],
        set(SIDECAR_SUBJECTS),
        "attestation subject sidecars",
    )
    fleet = subject["contains_fleet_sidecars"]
    for key, (expected_path, expected_name) in SIDECAR_SUBJECTS.items():
        record = sidecars[key]
        if not fleet:
            if record is not None:
                fail("base-package attestation must explicitly contain no sidecars")
            continue
        record = require_exact_keys(
            record,
            {"help", "name", "path", "sha256", "size"},
            f"attestation subject sidecar {key}",
        )
        if record["name"] != expected_name or record["path"] != expected_path:
            fail(f"attestation subject sidecar {key} identity is invalid")
        if not isinstance(record["sha256"], str) or not DIGEST_RE.fullmatch(record["sha256"]):
            fail(f"attestation subject sidecar {key} digest is invalid")
        if (
            isinstance(record["size"], bool)
            or not isinstance(record["size"], int)
            or record["size"] <= 0
            or record["size"] > MAX_FILE_BYTES
        ):
            fail(f"attestation subject sidecar {key} size is invalid")
        help_record = require_exact_keys(
            record["help"],
            {"arguments", "exit_code", "stderr", "stdout"},
            f"attestation subject sidecar {key} help execution",
        )
        if help_record["arguments"] != ["--help"]:
            fail(f"attestation subject sidecar {key} help arguments are invalid")
        if type(help_record["exit_code"]) is not int or help_record["exit_code"] != 0:
            fail(f"attestation subject sidecar {key} help exit code is invalid")
        if help_record["stdout"] != "":
            fail(f"attestation subject sidecar {key} help stdout is invalid")
        stderr = help_record["stderr"]
        if (
            not isinstance(stderr, str)
            or not stderr
            or "\0" in stderr
            or "\r" in stderr
            or not stderr.endswith("\n")
            or len(stderr.encode("utf-8")) > MAX_COMMAND_OUTPUT
        ):
            fail(f"attestation subject sidecar {key} help stderr is invalid")
        expected_prefix = {
            "go_proxy": "Usage: tesla-http-proxy [OPTION...]\n",
            "fleet_telemetry": "maxprocs: <runtime>\nUsage of fleet-telemetry:\n",
        }[key]
        if not stderr.startswith(expected_prefix):
            fail(f"attestation subject sidecar {key} help usage is invalid")


def validate_receipt_schema(value: Any) -> dict[str, Any]:
    receipt = require_exact_keys(
        value,
        {"created_utc", "generator", "native_host", "schema", "source", "subject", "toolchain"},
        "attestation receipt",
    )
    if receipt["schema"] != SCHEMA:
        fail("attestation schema is unsupported")
    created = require_text(receipt["created_utc"], "attestation created_utc", 64)
    try:
        parsed = datetime.fromisoformat(created.replace("Z", "+00:00"))
    except ValueError as exc:
        raise GateError("attestation created_utc is invalid") from exc
    if parsed.tzinfo is None or not created.endswith("Z"):
        fail("attestation created_utc must be UTC")
    generator = require_exact_keys(receipt["generator"], {"path", "sha256"}, "attestation generator")
    if generator["path"] != GENERATOR_PATH or not isinstance(generator["sha256"], str) or not DIGEST_RE.fullmatch(generator["sha256"]):
        fail("attestation generator identity is invalid")
    source = require_exact_keys(
        receipt["source"],
        {"cargo_version", "commit", "commit_created_utc", "tag", "tag_signer_fingerprint"},
        "attestation source",
    )
    if (
        not VERSION_RE.fullmatch(require_text(source["cargo_version"], "source cargo_version", 128))
        or not COMMIT_RE.fullmatch(require_text(source["commit"], "source commit", 64))
        or not TAG_RE.fullmatch(require_text(source["tag"], "source tag", 128))
        or not FINGERPRINT_RE.fullmatch(require_text(source["tag_signer_fingerprint"], "source signer", 64))
    ):
        fail("attestation source identity is invalid")
    require_text(source["commit_created_utc"], "source commit_created_utc", 64)
    subject = require_exact_keys(
        receipt["subject"],
        {
            "architecture", "binary_path", "binary_sha256", "binary_size",
            "binary_version_output", "contains_fleet_sidecars", "debian_version",
            "package_filename", "package_name", "package_sha256", "package_size",
            "payload_file_count", "payload_manifest_sha256", "sidecars",
        },
        "attestation subject",
    )
    if subject["architecture"] not in {"amd64", "arm64"}:
        fail("attestation subject architecture is invalid")
    if subject["binary_path"] != HUB_PATH or subject["package_name"] != PACKAGE_NAME:
        fail("attestation subject package identity is invalid")
    for field in ("binary_sha256", "package_sha256", "payload_manifest_sha256"):
        if not isinstance(subject[field], str) or not DIGEST_RE.fullmatch(subject[field]):
            fail(f"attestation subject {field} is invalid")
    for field in ("binary_size", "package_size", "payload_file_count"):
        if isinstance(subject[field], bool) or not isinstance(subject[field], int) or subject[field] <= 0:
            fail(f"attestation subject {field} is invalid")
    if not isinstance(subject["contains_fleet_sidecars"], bool):
        fail("attestation subject sidecar state is invalid")
    validate_sidecar_subjects(subject)
    for field in ("binary_version_output", "debian_version", "package_filename"):
        require_text(subject[field], f"attestation subject {field}", 256)
    host = require_exact_keys(
        receipt["native_host"],
        {
            "debian_architecture", "kernel_release", "machine", "os_release_id",
            "os_release_version_id", "sysname",
        },
        "attestation native host",
    )
    if (
        host["sysname"] != "Linux"
        or host["os_release_id"] != "debian"
        or host["os_release_version_id"] != "13"
        or host["debian_architecture"] != subject["architecture"]
        or host["machine"] != {"amd64": "x86_64", "arm64": "aarch64"}[subject["architecture"]]
    ):
        fail("attestation native host does not match Debian 13 package architecture")
    require_text(host["kernel_release"], "native host kernel_release", 256)
    toolchain = require_exact_keys(receipt["toolchain"], set(TOOL_COMMANDS), "attestation toolchain")
    for name, record in toolchain.items():
        record = require_exact_keys(record, {"executable_sha256", "version"}, f"toolchain {name}")
        if not isinstance(record["executable_sha256"], str) or not DIGEST_RE.fullmatch(record["executable_sha256"]):
            fail(f"toolchain {name} executable digest is invalid")
        require_text(record["version"], f"toolchain {name} version")
    return receipt


def generate(args: argparse.Namespace) -> None:
    if sys.platform != "linux" or os.uname().sysname != "Linux":
        fail("attestation generation requires native Linux")
    repo = real_directory(args.repo, "repository")
    signing_key = private_key(args.signing_key, repo)
    package = read_regular(args.package, "Debian package", MAX_PACKAGE_BYTES)
    commit, commit_created, signer = verify_tag(
        repo,
        args.tag,
        args.tag_signer_fingerprint,
        require_clean_head=True,
    )
    version = cargo_version(repo, args.tag)
    subject = validate_package(repo, args.tag, version, package, args.architecture)
    host, toolchain = native_host_and_toolchain(repo, subject["architecture"])
    execute_packaged_hub(package, subject, repo)
    created = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    source = {
        "cargo_version": version,
        "commit": commit,
        "commit_created_utc": commit_created,
        "tag": args.tag,
        "tag_signer_fingerprint": signer,
    }
    receipt_value = make_receipt(
        source=source,
        subject=subject,
        host=host,
        toolchain=toolchain,
        generator=expected_generator(repo, args.tag),
        created_utc=created,
    )
    validate_receipt_schema(receipt_value)
    output = Path(os.path.abspath(args.output_dir))
    if os.path.lexists(output):
        fail(f"attestation output already exists: {output}")
    parent = real_directory(output.parent, "attestation output parent")
    output = parent / output.name
    stage = Path(tempfile.mkdtemp(prefix=".teslatlas-debian-attestation.", dir=parent))
    try:
        receipt_path = stage / RECEIPT_NAME
        signature_path = stage / SIGNATURE_NAME
        write_new(receipt_path, canonical_json(receipt_value), 0o644)
        sign_receipt(receipt_path, signature_path, signing_key.path, repo)
        with tempfile.TemporaryDirectory(prefix="teslatlas-attestation-key-") as raw_key_dir:
            public_key = Path(raw_key_dir) / "public.pem"
            run(
                [
                    "openssl", "pkey", "-in", str(signing_key.path),
                    "-pubout", "-out", str(public_key),
                ],
                repo,
                "derive attestation public key",
            )
            verify_signature(receipt_path, signature_path, public_key, repo)
        reread_matches(package, "Debian package", MAX_PACKAGE_BYTES)
        reread_matches(signing_key, "attestation signing key", 64 * 1024)
        status = git_output(
            repo,
            ["status", "--porcelain=v1", "--untracked-files=all"],
            "recheck candidate checkout",
        )
        if status:
            fail("candidate checkout changed during attestation")
        os.chmod(stage, 0o755)
        stage.rename(output)
    except BaseException:
        shutil.rmtree(stage, ignore_errors=True)
        raise
    print(output)


def verify(args: argparse.Namespace) -> None:
    repo = real_directory(args.repo, "repository")
    package = read_regular(args.package, "Debian package", MAX_PACKAGE_BYTES)
    receipt = read_regular(args.receipt, "attestation receipt", MAX_RECEIPT_BYTES)
    signature = read_regular(args.signature, "attestation signature", 1024)
    public_key = read_regular(args.public_key, "attestation public key", 64 * 1024)
    if signature.size != 64:
        fail("Ed25519 attestation signature is not 64 bytes")
    expected_public_digest = args.public_key_sha256.lower()
    if not DIGEST_RE.fullmatch(expected_public_digest):
        fail("public key SHA-256 must be 64 hexadecimal characters")
    if public_key.sha256 != expected_public_digest:
        fail("attestation public key does not match the pinned SHA-256 trust anchor")
    ed25519_public_der(public_key.path, repo, public=True)
    verify_signature(receipt.path, signature.path, public_key.path, repo)
    value = parse_json(receipt.data, "attestation receipt")
    if canonical_json(value) != receipt.data:
        fail("attestation receipt is not canonical JSON")
    value = validate_receipt_schema(value)
    commit, commit_created, signer = verify_tag(
        repo,
        args.tag,
        args.tag_signer_fingerprint,
        require_clean_head=False,
    )
    version = cargo_version(repo, args.tag)
    expected_source = {
        "cargo_version": version,
        "commit": commit,
        "commit_created_utc": commit_created,
        "tag": args.tag,
        "tag_signer_fingerprint": signer,
    }
    if value["source"] != expected_source:
        fail("attestation source does not match the signed tag")
    if value["generator"] != expected_generator(repo, args.tag):
        fail("attestation generator does not match the signed tag")
    expected_subject = validate_package(
        repo,
        args.tag,
        version,
        package,
        args.architecture,
    )
    if expected_subject["contains_fleet_sidecars"]:
        for key in SIDECAR_SUBJECTS:
            expected_subject["sidecars"][key]["help"] = value["subject"]["sidecars"][key]["help"]
    if value["subject"] != expected_subject:
        fail("attestation subject does not match the supplied Debian package")
    reread_matches(package, "Debian package", MAX_PACKAGE_BYTES)
    reread_matches(receipt, "attestation receipt", MAX_RECEIPT_BYTES)
    reread_matches(signature, "attestation signature", 1024)
    reread_matches(public_key, "attestation public key", 64 * 1024)
    print(
        "Debian native attestation verified: "
        f"{args.tag} {commit} {args.architecture} {package.sha256}"
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="mode", required=True)

    generate_parser = subparsers.add_parser("generate", help="generate on native Debian 13")
    generate_parser.add_argument("--repo", type=Path, required=True)
    generate_parser.add_argument("--tag", required=True)
    generate_parser.add_argument("--tag-signer-fingerprint", required=True)
    generate_parser.add_argument("--package", type=Path, required=True)
    generate_parser.add_argument("--architecture", choices=("amd64", "arm64"), required=True)
    generate_parser.add_argument("--signing-key", type=Path, required=True)
    generate_parser.add_argument("--output-dir", type=Path, required=True)

    verify_parser = subparsers.add_parser("verify", help="verify on Linux or macOS")
    verify_parser.add_argument("--repo", type=Path, required=True)
    verify_parser.add_argument("--tag", required=True)
    verify_parser.add_argument("--tag-signer-fingerprint", required=True)
    verify_parser.add_argument("--package", type=Path, required=True)
    verify_parser.add_argument("--architecture", choices=("amd64", "arm64"), required=True)
    verify_parser.add_argument("--receipt", type=Path, required=True)
    verify_parser.add_argument("--signature", type=Path, required=True)
    verify_parser.add_argument("--public-key", type=Path, required=True)
    verify_parser.add_argument("--public-key-sha256", required=True)
    return parser.parse_args()


def main() -> int:
    os.umask(0o077)
    args = parse_args()
    if args.mode == "generate":
        generate(args)
    else:
        verify(args)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as exc:
        print(f"debian-release-attestation: {exc}", file=sys.stderr)
        raise SystemExit(1)
