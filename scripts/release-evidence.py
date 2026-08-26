#!/usr/bin/env python3
"""Create local, candidate-bound release evidence without building anything."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import gzip
import hashlib
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
import tempfile
import zipfile
from datetime import datetime, timezone


SCHEMA = "teslatlas.release-evidence/v1"
TAG_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
FINGERPRINT_RE = re.compile(r"^(?:[0-9A-Fa-f]{40}|[0-9A-Fa-f]{64})$")
GO_EVIDENCE_NAMES = (
    "GO_THIRD_PARTY_NOTICES.generated.md",
    "go-build-receipt.json",
    "go-component-manifest.json",
    "go-dependency-inventory.json",
    "go-sbom.spdx.json",
    "tesla-http-proxy.unsigned",
    "tesla-http-proxy-go-sources.tar.gz",
)


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
    if not stat.S_ISREG(before.st_mode):
        raise GateError(f"artifact must be a regular, non-symlink file: {path}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise GateError(f"artifact cannot be safely opened: {path}") from exc
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
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


def private_signing_key(path: Path) -> None:
    regular_file(path, "signing key")
    metadata = path.stat()
    if metadata.st_uid != os.geteuid() or metadata.st_mode & 0o077:
        raise GateError("signing key must be owned by the current user and mode 0600 or stricter")


def requires_external_proxy_notice(path: Path) -> bool:
    name = path.name.lower()
    return path.suffix.lower() in {".pkg", ".zip"} or "macos" in name


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


def capture_macos_release_bundle(
    repo: Path,
    artifacts: list[ArtifactWitness],
    go_evidence: Path,
    go_witnesses: list[ArtifactWitness],
) -> tuple[Path, list[ArtifactWitness]] | None:
    try:
        go_evidence.relative_to(repo)
    except ValueError:
        return None
    zip_artifacts = [item for item in artifacts if item.path.suffix.lower() == ".zip"]
    package_artifacts = [item for item in artifacts if item.path.suffix.lower() == ".pkg"]
    if len(zip_artifacts) != 1 or len(package_artifacts) != 1:
        return None
    bundle = zip_artifacts[0].path.parent
    if package_artifacts[0].path.parent != bundle or go_evidence.parent != bundle:
        return None
    logs = bundle / "notary-logs"
    checksums = bundle / "SHA256SUMS"
    expected_top_level = {
        zip_artifacts[0].path.name,
        package_artifacts[0].path.name,
        go_evidence.name,
        logs.name,
        checksums.name,
    }
    if {path.name for path in bundle.iterdir()} != expected_top_level:
        raise GateError("macOS release bundle contains unexpected or missing sidecars")
    for directory, label in (
        (bundle, "macOS release bundle"),
        (go_evidence, "Go proxy evidence"),
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
    }
    expected_digests = {
        zip_artifacts[0].path.name: zip_artifacts[0].digest,
        package_artifacts[0].path.name: package_artifacts[0].digest,
        **{
            f"{go_evidence.name}/{witness.path.name}": witness.digest
            for witness in go_witnesses
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


def verify_macos_release_bundle_structure(
    bundle: Path,
    artifacts: list[ArtifactWitness],
    go_evidence: Path,
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
        logs.name,
        "SHA256SUMS",
    }
    for directory, label in (
        (bundle, "macOS release bundle"),
        (go_evidence, "Go proxy evidence"),
        (logs, "macOS notary logs"),
    ):
        if not directory.is_dir() or directory.is_symlink():
            raise GateError(f"{label} changed during evidence generation")
    if {path.name for path in bundle.iterdir()} != expected_top_level:
        raise GateError("macOS release bundle changed during evidence generation")
    if {path.name for path in go_evidence.iterdir()} != set(GO_EVIDENCE_NAMES):
        raise GateError("Go proxy evidence file set changed during evidence generation")
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


def unsigned_code_digest(repo: Path, source: Path, destination: Path) -> str:
    witness = capture_artifact(source.parent, source)
    copy_witness_to(repo, witness, destination)
    run(["codesign", "--remove-signature", str(destination)], repo)
    regular_file(destination, "signature-stripped Tesla proxy")
    return canonical_macho_digest(destination)


def validate_macos_artifacts(
    repo: Path,
    artifacts: list[ArtifactWitness],
    go_manifest: dict,
    go_manifest_digest: str,
    stage: Path,
) -> None:
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
        proxy_path = app / "Contents" / "Resources" / "tesla-http-proxy"
        package_path = app / "Contents" / "Resources" / "TeslatlasHubService.pkg"
        for path, label in (
            (info_path, "Info.plist"),
            (proxy_path, "Tesla proxy"),
            (package_path, "service package"),
        ):
            regular_file(path, f"macOS release {label}")
        try:
            info = plistlib.loads(info_path.read_bytes())
        except (OSError, plistlib.InvalidFileException) as exc:
            raise GateError("macOS release Info.plist is invalid") from exc
        subject = go_manifest.get("subject")
        if not isinstance(subject, dict) or not isinstance(subject.get("sha256"), str):
            raise GateError("Go component manifest subject is invalid")
        if info.get("TeslatlasOfficialRelease") is not True:
            raise GateError("macOS artifact is not marked as an official release")
        if info.get("TeslatlasReleaseTeamIdentifier") != team:
            raise GateError("macOS release Team ID metadata does not match its signature")
        if info.get("TeslatlasUnsignedProxySHA256") != subject["sha256"]:
            raise GateError("macOS artifact does not bind the locked unsigned proxy")
        if info.get("TeslatlasGoEvidenceManifestSHA256") != go_manifest_digest:
            raise GateError("macOS artifact does not bind the supplied Go evidence")
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

    def scrub(value: object) -> object:
        if isinstance(value, dict):
            return {key: scrub(item) for key, item in value.items()}
        if isinstance(value, list):
            return [scrub(item) for item in value]
        if isinstance(value, str):
            repo_text = str(repo)
            if value == repo_text:
                return "."
            if value.startswith(repo_text + os.sep):
                return "./" + relative(repo, Path(value))
        return value

    return scrub(metadata)  # type: ignore[return-value]


def package_sort(package: dict) -> tuple[str, str, str]:
    return (package.get("name", ""), package.get("version", ""), package.get("source", ""))


def package_license_path(package: dict, repo: Path) -> Path | None:
    manifest = package.get("manifest_path")
    if not manifest:
        return None
    manifest_path = Path(manifest)
    if not manifest_path.is_absolute():
        manifest_path = repo / manifest_path
    root = manifest_path.parent
    declared = package.get("license_file")
    candidates: list[Path] = []
    if declared:
        candidates.append(root / declared)
    try:
        candidates.extend(sorted(
            path for path in root.iterdir()
            if path.is_file() and re.match(r"(?i)^(license|licence|copying|notice)([-_.].*)?$", path.name)
        ))
    except OSError:
        return None
    for candidate in candidates:
        if candidate.is_file() and not candidate.is_symlink():
            return candidate
    return None


def sbom_and_notices(metadata: dict, repo: Path) -> tuple[dict, dict, str]:
    project_notices = repo / "THIRD_PARTY_NOTICES.md"
    regular_file(project_notices, "project notices")
    packages = sorted(metadata.get("packages", []), key=package_sort)
    if not packages:
        raise GateError("cargo metadata contains no packages")
    ids = {package["id"]: f"SPDXRef-Package-{index:04d}" for index, package in enumerate(packages, 1)}
    spdx_packages = []
    inventory = []
    license_texts: dict[str, tuple[str, str]] = {}
    for package in packages:
        license_expression = package.get("license") or "NOASSERTION"
        if license_expression == "NOASSERTION" and not package.get("license_file"):
            raise GateError(f"dependency has no declared license: {package.get('name')}")
        source = package.get("source") or "NOASSERTION"
        checksum = package.get("checksum")
        if package.get("source") and not checksum and not package["source"].startswith("git+"):
            raise GateError(f"dependency has no Cargo checksum: {package.get('name')}")
        license_path = package_license_path(package, repo)
        if license_path is None:
            raise GateError(f"dependency license text is unavailable: {package.get('name')}")
        text = license_path.read_text(encoding="utf-8", errors="strict")
        text_hash = hashlib.sha256(text.encode()).hexdigest()
        license_texts.setdefault(text_hash, (license_path.name, text))
        package_id = ids[package["id"]]
        spdx_package = {
            "SPDXID": package_id,
            "name": package["name"],
            "versionInfo": package["version"],
            "downloadLocation": source,
            "licenseConcluded": license_expression,
            "licenseDeclared": license_expression,
            "copyrightText": "NOASSERTION",
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
        inventory.append({
            "name": package["name"], "version": package["version"], "source": source,
            "checksum": checksum, "license": license_expression, "license_text_sha256": text_hash,
            "package_id": package_id,
        })

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
    parser.add_argument("--go-proxy-evidence", type=Path,
                        help="evidence generated for the unsigned Tesla proxy in macOS artifacts")
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
    if os.path.lexists(output):
        raise GateError(f"output directory already exists: {output}")
    if not output.parent.is_dir():
        raise GateError("output parent does not exist")
    private_signing_key(args.signing_key.resolve())
    for command in ("git", "cargo", "gzip", "openssl", "shasum"):
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
    go_evidence_dir: Path | None = None
    go_evidence_witnesses: list[ArtifactWitness] = []
    if args.go_proxy_evidence is not None:
        go_evidence_dir = args.go_proxy_evidence.resolve()
        go_evidence_witnesses = capture_go_evidence(repo, go_evidence_dir)
    artifact_witnesses = [capture_artifact(repo, path) for path in artifacts]
    ignored_paths: list[Path] = []
    release_bundle_path: Path | None = None
    release_receipt_witnesses: list[ArtifactWitness] = []
    if has_macos_artifact:
        assert go_evidence_dir is not None
        release_bundle = capture_macos_release_bundle(
            repo,
            artifact_witnesses,
            go_evidence_dir,
            go_evidence_witnesses,
        )
        if release_bundle is not None:
            release_bundle_path, release_receipt_witnesses = release_bundle
            ignored_paths.extend(
                witness.path
                for witness in [
                    *go_evidence_witnesses,
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
    commit, created, tag_signer = clean_and_tag(
        repo, args.tag, artifacts, ignored_paths, args.tag_signer_fingerprint
    )

    stage = Path(tempfile.mkdtemp(prefix=f".{output.name}.", dir=output.parent))
    try:
        go_manifest: dict | None = None
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
            assert go_manifest is not None
            validate_macos_artifacts(
                repo,
                artifact_witnesses,
                go_manifest,
                sha256(stage / "go-proxy-evidence/go-component-manifest.json"),
                stage,
            )
        source_name = f"teslatlas-hub-{args.tag}-source.tar.gz"
        source_path = stage / source_name
        archive(repo, commit, args.tag, source_path)
        metadata = cargo_metadata(repo)
        write_json(stage / "cargo-metadata.json", metadata)
        spdx, inventory, notices = sbom_and_notices(metadata, repo)
        spdx["documentNamespace"] = f"urn:teslatlas:sbom:{args.tag}:{commit}"
        spdx["creationInfo"]["created"] = created
        write_json(stage / "sbom.spdx.json", spdx)
        write_json(stage / "dependency-inventory.json", inventory)
        (stage / "THIRD_PARTY_NOTICES.generated.md").write_text(notices, encoding="utf-8")

        artifact_records = [
            {"path": witness.relative_path, "size": witness.size, "sha256": witness.digest}
            for witness in sorted(artifact_witnesses, key=lambda item: item.relative_path)
        ]
        generated_names = [source_name, "cargo-metadata.json", "sbom.spdx.json",
                           "dependency-inventory.json", "THIRD_PARTY_NOTICES.generated.md"]
        if go_evidence_witnesses:
            for name in GO_EVIDENCE_NAMES:
                generated_names.append(f"go-proxy-evidence/{name}")
        if release_receipt_witnesses:
            generated_names.append("macos-release-receipts/SHA256SUMS")
            generated_names.extend(
                f"macos-release-receipts/notary-logs/{witness.path.name}"
                for witness in release_receipt_witnesses
                if witness.path.parent.name == "notary-logs"
            )
        manifest = {"schema": SCHEMA, "tag": args.tag, "commit": commit,
                    "artifacts": artifact_records,
                    "generated": [{"path": f"{relative(repo, output)}/{name}", "sha256": sha256(stage / name)}
                                   for name in generated_names]}
        if go_manifest is not None:
            manifest["go_proxy_evidence"] = {
                "subject": go_manifest["subject"],
                "manifest_sha256": sha256(stage / "go-proxy-evidence/go-component-manifest.json"),
            }
        write_json(stage / "artifact-manifest.json", manifest)

        public_key = stage / "provenance-public-key.pem"
        run(["openssl", "pkey", "-in", str(args.signing_key.resolve()), "-pubout", "-out", str(public_key)], repo)
        public_key_digest = sha256(public_key)
        if public_key_digest.lower() != args.public_key_sha256.lower():
            raise GateError("public-key-sha256 does not match the supplied signing key")
        provenance = {"schema": SCHEMA, "tag": args.tag, "commit": commit,
                      "tag_signature": {"verified": True, "signer_fingerprint": tag_signer},
                      "created": created,
                      "source_archive": {"path": f"{relative(repo, output)}/{source_name}", "sha256": sha256(source_path)},
                      "artifact_manifest": {"path": f"{relative(repo, output)}/artifact-manifest.json",
                                             "sha256": sha256(stage / "artifact-manifest.json")},
                      "sbom_sha256": sha256(stage / "sbom.spdx.json"),
                      "dependency_inventory_sha256": sha256(stage / "dependency-inventory.json"),
                      "notices_sha256": sha256(stage / "THIRD_PARTY_NOTICES.generated.md"),
                      "signing": {"algorithm": "openssl-sha256", "public_key_sha256": public_key_digest}}
        if go_manifest is not None:
            provenance["go_proxy_evidence"] = {
                "subject": go_manifest["subject"],
                "manifest_sha256": sha256(stage / "go-proxy-evidence/go-component-manifest.json"),
            }
        provenance_path = stage / "provenance.json"
        write_json(provenance_path, provenance)
        signature = stage / "provenance.sig"
        run(["openssl", "dgst", "-sha256", "-sign", str(args.signing_key.resolve()),
             "-out", str(signature), str(provenance_path)], repo)
        run(["openssl", "dgst", "-sha256", "-verify", str(public_key), "-signature", str(signature),
             str(provenance_path)], repo)

        checksum_records = [(witness.relative_path, witness.digest) for witness in artifact_witnesses]
        checksum_records.extend(
            (relative(repo, output / path.relative_to(stage)), sha256(path))
            for path in [
                stage / name
                for name in generated_names
                + [
                    "artifact-manifest.json",
                    "provenance.json",
                    "provenance.sig",
                    "provenance-public-key.pem",
                ]
            ]
        )
        checksum_lines = [
            f"{digest}  {path}" for path, digest in sorted(checksum_records)
        ]
        (stage / "SHA256SUMS").write_text("\n".join(checksum_lines) + "\n", encoding="utf-8")
        assert_candidate_unchanged(
            repo, args.tag, commit, artifacts, ignored_paths, stage
        )
        for witness in artifact_witnesses:
            verify_artifact_unchanged(repo, witness)
        for witness in go_evidence_witnesses:
            verify_go_evidence_unchanged(repo, witness)
        for witness in release_receipt_witnesses:
            verify_artifact_unchanged(repo, witness)
        if release_bundle_path is not None:
            assert go_evidence_dir is not None
            verify_macos_release_bundle_structure(
                release_bundle_path,
                artifact_witnesses,
                go_evidence_dir,
                release_receipt_witnesses,
            )
        publish_evidence_directory(stage, output)
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
