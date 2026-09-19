#!/usr/bin/env python3
"""Build and verify the deterministic Teslatlas Protocol developer bundle."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import sys
import tarfile
import tempfile
from typing import Iterable
import zlib


REPO = Path(__file__).resolve().parents[1]
ROOT_FILES = (
    "Dockerfile",
    "LICENSE",
    "README.md",
    "THIRD-PARTY-NOTICES.md",
    "VERSION",
    "pyproject.toml",
    "uv.lock",
)
TREE_ROOTS = (
    "compatibility",
    "conformance",
    "events",
    "examples",
    "fixtures",
    "openapi",
    "profiles",
    "schemas",
    "tests",
    "tools",
)
REQUIRED_PATHS = (
    "LICENSE",
    "README.md",
    "THIRD-PARTY-NOTICES.md",
    "VERSION",
    "pyproject.toml",
    "uv.lock",
    "openapi/teslatlas-v1.openapi.json",
    "schemas/common.schema.json",
    "events/teslatlas-v1.sse.json",
    "fixtures/manifest.json",
    "compatibility/manifest.json",
    "profiles/hub-http-v1/1.0.0/SHA256SUMS",
    "profiles/edge-delivery-v2/2.0.0/SHA256SUMS",
    "conformance/run",
    "tools/check",
    "tools/developer_bundle.py",
    "docs/developer-bundle.md",
)
GENERATED_PATHS = ("BUNDLE-MANIFEST.json", "BUNDLE-SHA256SUMS")
IGNORED_NAMES = {"__pycache__", ".DS_Store"}
IGNORED_SUFFIXES = {".pyc", ".pyo"}
GZIP_HEADER = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff"
MANIFEST_KEYS = {
    "schema_version",
    "bundle_id",
    "product_version",
    "python_requires",
    "dependency_lock",
    "contracts",
    "executable_paths",
    "files",
}
CONTRACTS = {
    "rich_profiles": ["1.0.0", "1.1.0", "1.2.0"],
    "current_hub": "hub-http-v1@1.0.0",
    "edge_delivery": "edge-delivery-v2@2.0.0",
}


class BundleError(RuntimeError):
    """Raised when a bundle is incomplete, unsafe, or content-divergent."""


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")


def is_ignored(path: Path) -> bool:
    return any(part in IGNORED_NAMES for part in path.parts) or path.suffix in IGNORED_SUFFIXES


def payload_paths(repo: Path = REPO) -> list[Path]:
    paths: list[Path] = []
    for relative in ROOT_FILES:
        paths.append(repo / relative)
    docs = repo / "docs"
    paths.extend(path for path in docs.glob("*.md") if path.is_file())
    for relative in TREE_ROOTS:
        root = repo / relative
        paths.extend(path for path in root.rglob("*") if path.is_file() and not is_ignored(path.relative_to(repo)))

    by_relative: dict[str, Path] = {}
    for path in paths:
        relative = path.relative_to(repo).as_posix()
        if relative in by_relative:
            continue
        if path.is_symlink() or not path.is_file():
            raise BundleError(f"bundle payload must be a regular file: {relative}")
        by_relative[relative] = path
    missing = sorted(set(REQUIRED_PATHS) - set(by_relative))
    if missing:
        raise BundleError(f"required bundle payload missing: {', '.join(missing)}")
    return [by_relative[relative] for relative in sorted(by_relative)]


def build_payload(repo: Path = REPO) -> tuple[str, dict[str, bytes]]:
    version = (repo / "VERSION").read_text(encoding="utf-8").strip()
    if not version or any(character.isspace() for character in version):
        raise BundleError("VERSION must contain one non-empty token")

    payload: dict[str, bytes] = {}
    executable: list[str] = []
    for path in payload_paths(repo):
        relative = path.relative_to(repo).as_posix()
        payload[relative] = path.read_bytes()
        if os.access(path, os.X_OK):
            executable.append(relative)

    lock_digest = sha256(payload["uv.lock"])
    manifest = {
        "schema_version": 1,
        "bundle_id": f"teslatlas-protocol-developer@{version}",
        "product_version": version,
        "python_requires": ">=3.11",
        "dependency_lock": {
            "path": "uv.lock",
            "sha256": lock_digest,
            "install": "uv sync --locked --group dev",
            "offline_after_cache_seed": "UV_OFFLINE=1 uv sync --locked --group dev",
        },
        "contracts": CONTRACTS,
        "executable_paths": sorted(executable),
        "files": [
            {"path": path, "sha256": sha256(data), "size": len(data)}
            for path, data in sorted(payload.items())
        ],
    }
    manifest_bytes = canonical_json(manifest)
    checksummed = dict(payload)
    checksummed["BUNDLE-MANIFEST.json"] = manifest_bytes
    sums = "".join(
        f"{sha256(data)}  {path}\n" for path, data in sorted(checksummed.items())
    ).encode("utf-8")
    payload["BUNDLE-MANIFEST.json"] = manifest_bytes
    payload["BUNDLE-SHA256SUMS"] = sums
    return version, payload


def tar_info(name: str, data: bytes, executable: bool) -> tarfile.TarInfo:
    info = tarfile.TarInfo(name=name)
    info.size = len(data)
    info.mode = 0o755 if executable else 0o644
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = 0
    return info


def canonical_tar(root: str, files: dict[str, bytes], modes: dict[str, int]) -> bytes:
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w", format=tarfile.GNU_FORMAT) as tar:
        for relative, data in sorted(files.items()):
            mode = modes[relative]
            if mode not in (0o644, 0o755):
                raise BundleError(f"archive member mode is not canonical: {relative}")
            name = f"{root}/{relative}"
            tar.addfile(tar_info(name, data, mode == 0o755), io.BytesIO(data))
    return output.getvalue()


def canonical_gzip(data: bytes) -> bytes:
    output = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=output, compresslevel=9, mtime=0) as zipped:
        zipped.write(data)
    return output.getvalue()


def build_archive(output_dir: Path, repo: Path = REPO) -> tuple[Path, str]:
    version, payload = build_payload(repo)
    root = f"teslatlas-protocol-{version}"
    output_dir.mkdir(parents=True, exist_ok=True)
    archive = output_dir / f"{root}.tar.gz"
    executable = set(json.loads(payload["BUNDLE-MANIFEST.json"])["executable_paths"])
    modes = {relative: 0o755 if relative in executable else 0o644 for relative in payload}

    temporary = archive.with_suffix(archive.suffix + ".tmp")
    try:
        temporary.write_bytes(canonical_gzip(canonical_tar(root, payload, modes)))
        archive_bytes = temporary.read_bytes()
        temporary.replace(archive)
    finally:
        temporary.unlink(missing_ok=True)

    digest = sha256(archive_bytes)
    checksum = archive.with_suffix(archive.suffix + ".sha256")
    checksum.write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    verify_archive(archive)
    return archive, digest


def safe_relative(member_name: str, root: str) -> str:
    path = PurePosixPath(member_name)
    if path.is_absolute() or ".." in path.parts or not path.parts or path.parts[0] != root:
        raise BundleError(f"unsafe or unexpected archive path: {member_name}")
    relative = PurePosixPath(*path.parts[1:])
    if not relative.parts or str(relative) == ".":
        raise BundleError(f"archive root is not a file: {member_name}")
    return relative.as_posix()


def verify_payload(files: dict[str, bytes]) -> dict[str, object]:
    expected_top = set(GENERATED_PATHS)
    if not expected_top.issubset(files):
        raise BundleError("bundle metadata files are missing")
    try:
        manifest = json.loads(files["BUNDLE-MANIFEST.json"])
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BundleError("BUNDLE-MANIFEST.json is invalid") from error
    if canonical_json(manifest) != files["BUNDLE-MANIFEST.json"]:
        raise BundleError("BUNDLE-MANIFEST.json is not canonical")
    if not isinstance(manifest, dict) or set(manifest) != MANIFEST_KEYS:
        raise BundleError("bundle manifest top-level schema is not closed")
    if manifest.get("schema_version") != 1:
        raise BundleError("unsupported bundle manifest schema")
    try:
        version = files["VERSION"].decode("utf-8").strip()
    except (KeyError, UnicodeDecodeError) as error:
        raise BundleError("bundle VERSION is missing or invalid") from error
    if manifest.get("product_version") != version or manifest.get("bundle_id") != f"teslatlas-protocol-developer@{version}":
        raise BundleError("bundle version identity mismatch")
    if manifest.get("python_requires") != ">=3.11":
        raise BundleError("bundle Python floor mismatch")
    contracts = manifest.get("contracts")
    if not isinstance(contracts, dict) or set(contracts) != set(CONTRACTS) or contracts != CONTRACTS:
        raise BundleError("bundle contract identities mismatch")

    entries = manifest.get("files")
    if not isinstance(entries, list):
        raise BundleError("bundle manifest files must be an array")
    declared: dict[str, dict[str, object]] = {}
    declared_order: list[str] = []
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"path", "sha256", "size"}:
            raise BundleError("bundle manifest file entry is malformed")
        path = entry.get("path")
        if not isinstance(path, str) or PurePosixPath(path).is_absolute() or ".." in PurePosixPath(path).parts:
            raise BundleError("bundle manifest contains an unsafe path")
        if path in declared or path in GENERATED_PATHS:
            raise BundleError(f"bundle manifest contains a duplicate or reserved path: {path}")
        declared[path] = entry
        declared_order.append(path)
    if declared_order != sorted(declared_order):
        raise BundleError("bundle manifest file entries are not in canonical path order")

    actual_payload = set(files) - set(GENERATED_PATHS)
    if set(declared) != actual_payload:
        missing = sorted(set(declared) - actual_payload)
        extra = sorted(actual_payload - set(declared))
        raise BundleError(f"bundle membership mismatch: missing={missing}, extra={extra}")
    required_missing = sorted(set(REQUIRED_PATHS) - actual_payload)
    if required_missing:
        raise BundleError(f"required bundle members missing: {required_missing}")

    for path, entry in declared.items():
        data = files[path]
        if entry["size"] != len(data) or entry["sha256"] != sha256(data):
            raise BundleError(f"bundle member checksum mismatch: {path}")

    lock = manifest.get("dependency_lock")
    if (
        not isinstance(lock, dict)
        or set(lock) != {"path", "sha256", "install", "offline_after_cache_seed"}
        or lock.get("path") != "uv.lock"
        or lock.get("sha256") != sha256(files["uv.lock"])
        or lock.get("install") != "uv sync --locked --group dev"
        or lock.get("offline_after_cache_seed") != "UV_OFFLINE=1 uv sync --locked --group dev"
    ):
        raise BundleError("dependency lock identity mismatch")

    executable = manifest.get("executable_paths")
    if (
        not isinstance(executable, list)
        or executable != sorted(executable)
        or len(executable) != len(set(executable))
        or any(not isinstance(path, str) or path not in actual_payload for path in executable)
    ):
        raise BundleError("bundle executable path list is invalid")

    expected_sums = "".join(
        f"{sha256(data)}  {path}\n"
        for path, data in sorted(files.items())
        if path != "BUNDLE-SHA256SUMS"
    ).encode("utf-8")
    if files["BUNDLE-SHA256SUMS"] != expected_sums:
        raise BundleError("BUNDLE-SHA256SUMS mismatch")
    return manifest


def read_archive(archive: Path) -> tuple[str, dict[str, bytes], dict[str, int]]:
    files: dict[str, bytes] = {}
    modes: dict[str, int] = {}
    roots: set[str] = set()
    names: list[str] = []
    try:
        archive_bytes = archive.read_bytes()
        if archive_bytes[:10] != GZIP_HEADER:
            raise BundleError("archive gzip header is not canonical")
        decompressor = zlib.decompressobj(wbits=16 + zlib.MAX_WBITS)
        decoded = decompressor.decompress(archive_bytes) + decompressor.flush()
        if not decompressor.eof or decompressor.unused_data or decompressor.unconsumed_tail:
            raise BundleError("archive must contain exactly one complete gzip member")
        members: list[tuple[str, bytes, int]] = []
        with tarfile.open(fileobj=io.BytesIO(decoded), mode="r:") as tar:
            for member in tar:
                names.append(member.name)
                path = PurePosixPath(member.name)
                if path.parts:
                    roots.add(path.parts[0])
                if not member.isfile():
                    raise BundleError(f"archive contains a non-regular member: {member.name}")
                if (
                    member.uid != 0
                    or member.gid != 0
                    or member.uname not in (None, "")
                    or member.gname not in (None, "")
                    or member.mtime != 0
                    or member.mode not in (0o644, 0o755)
                ):
                    raise BundleError(f"archive member metadata is not canonical: {member.name}")
                extracted = tar.extractfile(member)
                if extracted is None:
                    raise BundleError(f"archive member is unreadable: {member.name}")
                members.append((member.name, extracted.read(), member.mode))
            if names != sorted(names):
                raise BundleError("archive members are not in canonical path order")
            if len(names) != len(set(names)):
                raise BundleError("archive contains duplicate member names")
            if len(roots) != 1:
                raise BundleError("archive must have exactly one root directory")
            root = next(iter(roots))
        for member_name, data, mode in members:
            relative = safe_relative(member_name, root)
            if relative in files:
                raise BundleError(f"duplicate archive member: {relative}")
            files[relative] = data
            modes[relative] = mode
        paths = {PurePosixPath(relative) for relative in files}
        for path in paths:
            if any(parent in paths for parent in path.parents if str(parent) != "."):
                raise BundleError(f"archive contains a parent/file conflict: {path}")
        if decoded != canonical_tar(root, files, modes):
            raise BundleError("decoded tar stream is not canonical or has trailing bytes")
    except (tarfile.TarError, OSError, zlib.error) as error:
        raise BundleError(f"cannot read archive: {archive}") from error
    return root, files, modes


def validate_root(root: str, manifest: dict[str, object]) -> None:
    expected_root = f"teslatlas-protocol-{manifest['product_version']}"
    if root != expected_root:
        raise BundleError(f"archive root mismatch: expected {expected_root}, got {root}")


def verify_archive(archive: Path) -> dict[str, object]:
    root, files, modes = read_archive(archive)
    manifest = verify_payload(files)
    validate_root(root, manifest)
    executable = set(manifest["executable_paths"])
    for path, mode in modes.items():
        expected_mode = 0o755 if path in executable else 0o644
        if mode != expected_mode:
            raise BundleError(f"archive member mode mismatch: {path}")
    return manifest


def directory_files(root: Path) -> dict[str, bytes]:
    files: dict[str, bytes] = {}
    for path in root.rglob("*"):
        if path.is_symlink():
            raise BundleError(f"extracted bundle contains a symlink: {path.relative_to(root)}")
        if path.is_file():
            files[path.relative_to(root).as_posix()] = path.read_bytes()
    return files


def verify_directory(root: Path) -> dict[str, object]:
    if not root.is_dir() or root.is_symlink():
        raise BundleError(f"bundle directory does not exist: {root}")
    files = directory_files(root)
    manifest = verify_payload(files)
    executable = set(manifest["executable_paths"])
    for relative in files:
        mode = stat.S_IMODE((root / relative).stat().st_mode)
        expected_mode = 0o755 if relative in executable else 0o644
        if mode != expected_mode:
            raise BundleError(f"extracted bundle member mode mismatch: {relative}")
    return manifest


def extract_archive(archive: Path, destination: Path) -> Path:
    root, files, modes = read_archive(archive)
    manifest = verify_payload(files)
    validate_root(root, manifest)
    target = destination / root
    if target.exists():
        raise BundleError(f"extraction target already exists: {target}")
    destination.mkdir(parents=True, exist_ok=True)
    temporary = Path(tempfile.mkdtemp(prefix=f".{root}.", dir=destination))
    try:
        for relative, data in sorted(files.items()):
            output = temporary / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_bytes(data)
            output.chmod(modes[relative])
        verify_directory(temporary)
        temporary.replace(target)
    except BaseException:
        shutil.rmtree(temporary, ignore_errors=True)
        raise
    return target


def parser() -> argparse.ArgumentParser:
    command = argparse.ArgumentParser(description=__doc__)
    subparsers = command.add_subparsers(dest="command", required=True)
    build = subparsers.add_parser("build", help="build and verify a deterministic archive")
    build.add_argument("--output", type=Path, default=REPO / "dist")
    verify = subparsers.add_parser("verify", help="verify an archive or extracted bundle")
    verify.add_argument("path", type=Path)
    extract = subparsers.add_parser("extract", help="safely extract and verify an archive")
    extract.add_argument("archive", type=Path)
    extract.add_argument("--destination", type=Path, required=True)
    return command


def main(argv: Iterable[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        if arguments.command == "build":
            archive, digest = build_archive(arguments.output)
            print(f"{digest}  {archive}")
        elif arguments.command == "verify":
            manifest = verify_directory(arguments.path) if arguments.path.is_dir() else verify_archive(arguments.path)
            print(f"verified {manifest['bundle_id']}")
        else:
            target = extract_archive(arguments.archive, arguments.destination)
            print(target)
    except BundleError as error:
        print(f"developer bundle error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
