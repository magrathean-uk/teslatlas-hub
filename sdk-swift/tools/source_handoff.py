#!/usr/bin/env python3
"""Prepare and verify a deterministic four-library SwiftPM source handoff."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import subprocess
import tempfile
from typing import Any, Iterable


SCHEMA_VERSION = 1
CANONICAL_ROOT = "teslatlas-sdk-swift"
CONSUMER_ROOT = "external-four-library-consumer"
HANDOFF_MANIFEST = "source-handoff.json"
PROTOCOL_SOURCE_COMMIT = "53b5c6483990db84e5214176755f398e93d87b1b"
PUBLIC_PRODUCTS = (
    "TeslatlasHubSDK",
    "TeslatlasCommands",
    "TeslatlasHubV1Compatibility",
    "TeslatlasCurrentHub",
)
ROOT_FILES = ("Package.swift", "VERSION", "LICENSE", "README.md")
SOURCE_DIRECTORIES = (
    "Sources",
    "Tests",
    "Examples/TeslatlasHubSDKExample",
    "Examples/CurrentHubConsumer",
)
PUBLIC_DOCUMENTS = (
    "docs/architecture.md",
    "docs/current-hub.md",
    "docs/development.md",
    "docs/hub-v1-compatibility.md",
    "docs/product-versioning.md",
    "docs/protocol-dependency-gate.md",
    "docs/source-distribution.md",
)
IGNORED_NAMES = {".DS_Store", ".build", ".swiftpm", "DerivedData", "__pycache__"}
DIRECTORY_MODE = 0o755
REGULAR_FILE_MODE = 0o644
FIXED_TIMESTAMP_SECONDS = 946_684_800
FIXED_TIMESTAMP_NS = FIXED_TIMESTAMP_SECONDS * 1_000_000_000

CONSUMER_PACKAGE = """// swift-tools-version: 6.0

import PackageDescription

let package = Package(
  name: "ExternalFourLibraryConsumer",
  platforms: [.macOS(.v14), .iOS(.v17)],
  dependencies: [
    .package(path: "../teslatlas-sdk-swift")
  ],
  targets: [
    .executableTarget(
      name: "ExternalFourLibraryConsumer",
      dependencies: [
        .product(name: "TeslatlasHubSDK", package: "teslatlas-sdk-swift"),
        .product(name: "TeslatlasCommands", package: "teslatlas-sdk-swift"),
        .product(name: "TeslatlasHubV1Compatibility", package: "teslatlas-sdk-swift"),
        .product(name: "TeslatlasCurrentHub", package: "teslatlas-sdk-swift"),
      ]
    )
  ]
)
"""

CONSUMER_MAIN = """import TeslatlasCommands
import TeslatlasCurrentHub
import TeslatlasHubSDK
import TeslatlasHubV1Compatibility

let importedProducts = [
  "TeslatlasHubSDK",
  "TeslatlasCommands",
  "TeslatlasHubV1Compatibility",
  "TeslatlasCurrentHub",
]

print(importedProducts.joined(separator: ","))
"""


class HandoffError(RuntimeError):
    """A source handoff failed a strict validation rule."""


def _source_root() -> Path:
    return Path(__file__).resolve().parent.parent


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")


def _validate_relative_path(relative: str) -> PurePosixPath:
    path = PurePosixPath(relative)
    if not relative or path.is_absolute() or ".." in path.parts:
        raise HandoffError(f"unsafe relative path: {relative!r}")
    if path.as_posix() != relative:
        raise HandoffError(f"non-canonical relative path: {relative!r}")
    return path


def _resolved_beneath(root: Path, path: Path, label: str) -> Path:
    try:
        resolved = path.resolve(strict=True)
        resolved.relative_to(root)
    except (FileNotFoundError, RuntimeError, ValueError) as error:
        raise HandoffError(f"{label} escapes or is unresolved: {path}") from error
    return resolved


def _walk_selected_directory(source_root: Path, directory: Path) -> list[str]:
    selected: list[str] = []
    for current, directory_names, file_names in os.walk(
        directory,
        topdown=True,
        followlinks=False,
    ):
        current_path = Path(current)
        _resolved_beneath(source_root, current_path, "source directory")

        retained_directories: list[str] = []
        for name in directory_names:
            if name in IGNORED_NAMES:
                continue
            path = current_path / name
            information = path.lstat()
            relative = path.relative_to(source_root)
            if stat.S_ISLNK(information.st_mode):
                raise HandoffError(f"source symlink is not allowed: {relative}")
            if not stat.S_ISDIR(information.st_mode):
                raise HandoffError(f"special source entry is not allowed: {relative}")
            _resolved_beneath(source_root, path, "source directory")
            retained_directories.append(name)
        directory_names[:] = retained_directories

        for name in file_names:
            if name in IGNORED_NAMES:
                continue
            path = current_path / name
            information = path.lstat()
            relative = path.relative_to(source_root)
            if stat.S_ISLNK(information.st_mode):
                raise HandoffError(f"source symlink is not allowed: {relative}")
            if not stat.S_ISREG(information.st_mode):
                raise HandoffError(f"special source entry is not allowed: {relative}")
            _resolved_beneath(source_root, path, "source file")
            selected.append(relative.as_posix())
    return selected


def _selected_source_files(source_root: Path) -> list[str]:
    source_root = source_root.resolve(strict=True)
    selected: set[str] = set(ROOT_FILES + PUBLIC_DOCUMENTS)
    for directory_name in SOURCE_DIRECTORIES:
        directory = source_root / directory_name
        if directory.is_symlink():
            raise HandoffError(f"source selection root is a symlink: {directory_name}")
        try:
            information = directory.lstat()
        except FileNotFoundError as error:
            raise HandoffError(f"missing source directory: {directory_name}") from error
        if not stat.S_ISDIR(information.st_mode):
            raise HandoffError(f"missing source directory: {directory_name}")
        _resolved_beneath(source_root, directory, "source selection root")
        selected.update(_walk_selected_directory(source_root, directory))

    result = sorted(selected)
    for relative in result:
        path = source_root / _validate_relative_path(relative)
        if path.is_symlink():
            raise HandoffError(f"source symlink is not allowed: {relative}")
        try:
            information = path.lstat()
        except FileNotFoundError as error:
            raise HandoffError(f"missing regular source file: {relative}") from error
        if not stat.S_ISREG(information.st_mode):
            raise HandoffError(f"missing regular source file: {relative}")
        _resolved_beneath(source_root, path, "source file")
    return result


def _copy_files(
    source_root: Path,
    destination_root: Path,
    relatives: Iterable[str],
) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for relative in relatives:
        source = source_root / _validate_relative_path(relative)
        data = source.read_bytes()
        destination = destination_root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        records.append({"path": relative, "size": len(data), "sha256": _sha256(data)})
    return records


def _write_consumer(handoff_root: Path) -> list[dict[str, Any]]:
    consumer_root = handoff_root / CONSUMER_ROOT
    files = {
        "Package.swift": CONSUMER_PACKAGE.encode("utf-8"),
        "Sources/ExternalFourLibraryConsumer/main.swift": CONSUMER_MAIN.encode("utf-8"),
    }
    records: list[dict[str, Any]] = []
    for relative, data in sorted(files.items()):
        destination = consumer_root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        records.append({"path": relative, "size": len(data), "sha256": _sha256(data)})
    return records


def _identity_payload(files: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "canonical_root": CANONICAL_ROOT,
        "files": files,
    }


def _normalize_tree_metadata(root: Path) -> None:
    for current, directory_names, file_names in os.walk(
        root,
        topdown=False,
        followlinks=False,
    ):
        current_path = Path(current)
        for name in file_names:
            path = current_path / name
            information = path.lstat()
            if not stat.S_ISREG(information.st_mode):
                raise HandoffError(f"cannot normalize non-regular file: {path}")
            path.chmod(REGULAR_FILE_MODE)
            os.utime(path, ns=(FIXED_TIMESTAMP_NS, FIXED_TIMESTAMP_NS))
        for name in directory_names:
            path = current_path / name
            information = path.lstat()
            if stat.S_ISLNK(information.st_mode) or not stat.S_ISDIR(
                information.st_mode
            ):
                raise HandoffError(f"cannot normalize non-directory: {path}")
            path.chmod(DIRECTORY_MODE)
            os.utime(path, ns=(FIXED_TIMESTAMP_NS, FIXED_TIMESTAMP_NS))
        current_path.chmod(DIRECTORY_MODE)
        os.utime(current_path, ns=(FIXED_TIMESTAMP_NS, FIXED_TIMESTAMP_NS))


def prepare(source_root: Path, output: Path) -> dict[str, Any]:
    source_root = source_root.resolve(strict=True)
    output = output.resolve()
    if output.exists():
        raise HandoffError(f"output already exists: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)

    temporary = Path(tempfile.mkdtemp(prefix=f".{output.name}-", dir=output.parent))
    try:
        package_root = temporary / CANONICAL_ROOT
        package_root.mkdir()
        source_files = _copy_files(
            source_root,
            package_root,
            _selected_source_files(source_root),
        )
        consumer_files = _write_consumer(temporary)
        identity_payload = _identity_payload(source_files)
        manifest = {
            "schema_version": SCHEMA_VERSION,
            "kind": "teslatlas-swift-source-handoff",
            "source_only": True,
            "runtime_acceptance": False,
            "protocol_runtime_source_commit": PROTOCOL_SOURCE_COMMIT,
            "public_products": list(PUBLIC_PRODUCTS),
            "source_package": {
                **identity_payload,
                "identity_sha256": _sha256(_canonical_json(identity_payload)),
            },
            "external_consumer": {
                "root": CONSUMER_ROOT,
                "files": consumer_files,
            },
        }
        (temporary / HANDOFF_MANIFEST).write_bytes(
            json.dumps(manifest, indent=2, sort_keys=True).encode("utf-8") + b"\n"
        )
        _normalize_tree_metadata(temporary)
        temporary.rename(output)
    except BaseException:
        shutil.rmtree(temporary, ignore_errors=True)
        raise

    try:
        verify(output)
    except BaseException:
        shutil.rmtree(output, ignore_errors=True)
        raise
    return manifest


def _require_exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        raise HandoffError(
            f"{label} keys differ: expected {sorted(expected)}, got {sorted(actual)}"
        )


def _strict_json_loads(payload: bytes | str, label: str) -> Any:
    def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in pairs:
            if key in value:
                raise HandoffError(f"{label} contains duplicate key: {key}")
            value[key] = item
        return value

    try:
        return json.loads(payload, object_pairs_hook=reject_duplicate_keys)
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        raise HandoffError(f"invalid {label}: {error}") from error


def _read_manifest(handoff_root: Path) -> dict[str, Any]:
    manifest_path = handoff_root / HANDOFF_MANIFEST
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise HandoffError(f"missing regular manifest: {manifest_path}")
    value = _strict_json_loads(manifest_path.read_bytes(), "handoff manifest")
    if not isinstance(value, dict):
        raise HandoffError("handoff manifest must be an object")
    return value


def _expected_parent_directories(paths: Iterable[str]) -> list[str]:
    directories: set[str] = set()
    for relative in paths:
        parent = PurePosixPath(relative).parent
        while parent != PurePosixPath("."):
            directories.add(parent.as_posix())
            parent = parent.parent
    return sorted(directories)


def _verify_metadata(path: Path, expected_mode: int, label: str) -> None:
    information = path.lstat()
    actual_mode = stat.S_IMODE(information.st_mode)
    if actual_mode != expected_mode:
        raise HandoffError(
            f"{label} mode differs: expected {oct(expected_mode)}, got {oct(actual_mode)}"
        )
    if information.st_mtime_ns != FIXED_TIMESTAMP_NS:
        raise HandoffError(f"{label} modification timestamp differs: {path}")


def _inventory_tree(root: Path, label: str) -> tuple[list[str], list[str]]:
    information = root.lstat()
    if stat.S_ISLNK(information.st_mode) or not stat.S_ISDIR(information.st_mode):
        raise HandoffError(f"{label} root is missing or not a regular directory")
    _verify_metadata(root, DIRECTORY_MODE, f"{label} root")
    resolved_root = root.resolve(strict=True)
    files: list[str] = []
    directories: list[str] = []
    for current, directory_names, file_names in os.walk(
        root,
        topdown=True,
        followlinks=False,
    ):
        current_path = Path(current)
        _resolved_beneath(resolved_root, current_path, label)
        for name in directory_names:
            path = current_path / name
            relative = path.relative_to(root).as_posix()
            entry = path.lstat()
            if stat.S_ISLNK(entry.st_mode):
                raise HandoffError(f"{label} symlink is not allowed: {relative}")
            if not stat.S_ISDIR(entry.st_mode):
                raise HandoffError(f"{label} special entry is not allowed: {relative}")
            _resolved_beneath(resolved_root, path, label)
            _verify_metadata(path, DIRECTORY_MODE, f"{label} directory")
            directories.append(relative)
        for name in file_names:
            path = current_path / name
            relative = path.relative_to(root).as_posix()
            entry = path.lstat()
            if stat.S_ISLNK(entry.st_mode):
                raise HandoffError(f"{label} symlink is not allowed: {relative}")
            if not stat.S_ISREG(entry.st_mode):
                raise HandoffError(f"{label} special entry is not allowed: {relative}")
            _resolved_beneath(resolved_root, path, label)
            _verify_metadata(path, REGULAR_FILE_MODE, f"{label} file")
            files.append(relative)
    return sorted(files), sorted(directories)


def _verify_file_records(root: Path, records: Any, label: str) -> None:
    if not isinstance(records, list) or not records:
        raise HandoffError(f"{label} files must be a non-empty array")
    seen: list[str] = []
    for record in records:
        if not isinstance(record, dict):
            raise HandoffError(f"{label} file record must be an object")
        _require_exact_keys(record, {"path", "size", "sha256"}, f"{label} file")
        relative = record["path"]
        if not isinstance(relative, str):
            raise HandoffError(f"{label} path must be a string")
        if type(record["size"]) is not int or record["size"] < 0:
            raise HandoffError(f"{label} size must be a non-negative integer")
        digest = record["sha256"]
        if (
            not isinstance(digest, str)
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
        ):
            raise HandoffError(f"{label} sha256 must be lowercase hexadecimal")
        _validate_relative_path(relative)
        seen.append(relative)
    if seen != sorted(set(seen)):
        raise HandoffError(f"{label} file paths must be unique and sorted")

    actual_files, actual_directories = _inventory_tree(root, label)
    if actual_files != seen:
        missing = sorted(set(seen) - set(actual_files))
        extra = sorted(set(actual_files) - set(seen))
        raise HandoffError(
            f"{label} contains missing or extra paths: missing={missing}, extra={extra}"
        )
    expected_directories = _expected_parent_directories(seen)
    if actual_directories != expected_directories:
        missing = sorted(set(expected_directories) - set(actual_directories))
        extra = sorted(set(actual_directories) - set(expected_directories))
        raise HandoffError(
            f"{label} contains missing or extra directories: "
            f"missing={missing}, extra={extra}"
        )
    for record in records:
        path = root / record["path"]
        data = path.read_bytes()
        if record["size"] != len(data) or record["sha256"] != _sha256(data):
            raise HandoffError(f"{label} file bytes differ: {record['path']}")


def verify(handoff_root: Path) -> dict[str, Any]:
    handoff_root = handoff_root.absolute()
    try:
        root_information = handoff_root.lstat()
    except FileNotFoundError as error:
        raise HandoffError(
            f"handoff root is not a regular directory: {handoff_root}"
        ) from error
    if stat.S_ISLNK(root_information.st_mode) or not stat.S_ISDIR(
        root_information.st_mode
    ):
        raise HandoffError(f"handoff root is not a regular directory: {handoff_root}")
    handoff_root = handoff_root.resolve()
    manifest = _read_manifest(handoff_root)
    _require_exact_keys(
        manifest,
        {
            "schema_version",
            "kind",
            "source_only",
            "runtime_acceptance",
            "protocol_runtime_source_commit",
            "public_products",
            "source_package",
            "external_consumer",
        },
        "manifest",
    )
    if manifest["schema_version"] != SCHEMA_VERSION:
        raise HandoffError("unsupported schema version")
    if manifest["kind"] != "teslatlas-swift-source-handoff":
        raise HandoffError("unexpected handoff kind")
    if manifest["source_only"] is not True or manifest["runtime_acceptance"] is not False:
        raise HandoffError("handoff claim boundary changed")
    if manifest["protocol_runtime_source_commit"] != PROTOCOL_SOURCE_COMMIT:
        raise HandoffError("Protocol runtime-source identity changed")
    if manifest["public_products"] != list(PUBLIC_PRODUCTS):
        raise HandoffError("public product list changed")

    source = manifest["source_package"]
    if not isinstance(source, dict):
        raise HandoffError("source_package must be an object")
    _require_exact_keys(
        source,
        {"schema_version", "canonical_root", "files", "identity_sha256"},
        "source_package",
    )
    if (
        source["schema_version"] != SCHEMA_VERSION
        or source["canonical_root"] != CANONICAL_ROOT
    ):
        raise HandoffError("canonical source-package identity changed")
    package_root = handoff_root / CANONICAL_ROOT
    if package_root.name != CANONICAL_ROOT:
        raise HandoffError("source package root name changed")
    _verify_file_records(package_root, source["files"], "source package")
    identity = _identity_payload(source["files"])
    if source["identity_sha256"] != _sha256(_canonical_json(identity)):
        raise HandoffError("source package identity digest differs")

    consumer = manifest["external_consumer"]
    if not isinstance(consumer, dict):
        raise HandoffError("external_consumer must be an object")
    _require_exact_keys(consumer, {"root", "files"}, "external_consumer")
    if consumer["root"] != CONSUMER_ROOT:
        raise HandoffError("external consumer root changed")
    _verify_file_records(
        handoff_root / CONSUMER_ROOT,
        consumer["files"],
        "external consumer",
    )
    expected_consumer = {
        "Package.swift": CONSUMER_PACKAGE.encode("utf-8"),
        "Sources/ExternalFourLibraryConsumer/main.swift": CONSUMER_MAIN.encode("utf-8"),
    }
    for relative, expected in expected_consumer.items():
        if (handoff_root / CONSUMER_ROOT / relative).read_bytes() != expected:
            raise HandoffError(f"external consumer definition changed: {relative}")

    actual_files, actual_directories = _inventory_tree(handoff_root, "handoff")
    expected_files = sorted(
        [HANDOFF_MANIFEST]
        + [f"{CANONICAL_ROOT}/{record['path']}" for record in source["files"]]
        + [f"{CONSUMER_ROOT}/{record['path']}" for record in consumer["files"]]
    )
    expected_directories = sorted(
        [CANONICAL_ROOT, CONSUMER_ROOT]
        + [
            f"{CANONICAL_ROOT}/{relative}"
            for relative in _expected_parent_directories(
                record["path"] for record in source["files"]
            )
        ]
        + [
            f"{CONSUMER_ROOT}/{relative}"
            for relative in _expected_parent_directories(
                record["path"] for record in consumer["files"]
            )
        ]
    )
    if actual_files != expected_files:
        raise HandoffError("handoff contains missing or extra files")
    if actual_directories != expected_directories:
        raise HandoffError("handoff contains missing or extra directories")
    return manifest


def _run(command: list[str]) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        command,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise HandoffError(
            f"command failed with exit {completed.returncode}: {' '.join(command)}\n{detail}"
        )
    return completed


def _remove_swiftpm_generated_paths(handoff_root: Path) -> None:
    candidates = (
        handoff_root / CANONICAL_ROOT / ".build",
        handoff_root / CANONICAL_ROOT / ".swiftpm",
        handoff_root / CANONICAL_ROOT / "Package.resolved",
        handoff_root / CONSUMER_ROOT / ".build",
        handoff_root / CONSUMER_ROOT / ".swiftpm",
        handoff_root / CONSUMER_ROOT / "Package.resolved",
    )
    for path in candidates:
        if path.is_symlink() or path.is_file():
            path.unlink()
        elif path.is_dir():
            shutil.rmtree(path)


def smoke(handoff_root: Path) -> dict[str, Any]:
    handoff_root = handoff_root.absolute()
    manifest = verify(handoff_root)
    package_root = handoff_root / CANONICAL_ROOT
    try:
        dumped = _strict_json_loads(
            _run(
                [
                    "swift",
                    "package",
                    "dump-package",
                    "--package-path",
                    str(package_root),
                ]
            ).stdout,
            "SwiftPM package manifest output",
        )
        if not isinstance(dumped, dict):
            raise HandoffError("SwiftPM package manifest output must be an object")
        library_products = sorted(
            product["name"]
            for product in dumped.get("products", [])
            if isinstance(product, dict)
            and isinstance(product.get("type"), dict)
            and "library" in product["type"]
        )
        if library_products != sorted(PUBLIC_PRODUCTS):
            raise HandoffError(
                f"SwiftPM library products differ: expected {sorted(PUBLIC_PRODUCTS)}, "
                f"got {library_products}"
            )

        with tempfile.TemporaryDirectory(prefix="teslatlas-swift-smoke-") as scratch:
            _run(
                [
                    "swift",
                    "build",
                    "--package-path",
                    str(handoff_root / CONSUMER_ROOT),
                    "--scratch-path",
                    str(Path(scratch) / "build"),
                ]
            )
    finally:
        _remove_swiftpm_generated_paths(handoff_root)
        _normalize_tree_metadata(handoff_root)
        verify(handoff_root)
    return {
        "source_package_identity_sha256": manifest["source_package"]["identity_sha256"],
        "canonical_root": CANONICAL_ROOT,
        "public_products": list(PUBLIC_PRODUCTS),
        "swiftpm_manifest": "validated",
        "external_consumer_build": "passed",
        "runtime_executed": False,
        "scratch_cleaned": True,
    }


def exercise(source_root: Path) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="teslatlas-swift-handoff-") as temporary:
        handoff = Path(temporary) / "handoff"
        prepare(source_root, handoff)
        result = smoke(handoff)
    result["handoff_cleaned"] = True
    return result


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    prepare_parser = subparsers.add_parser("prepare")
    prepare_parser.add_argument("--source-root", type=Path, default=_source_root())
    prepare_parser.add_argument("--output", type=Path, required=True)

    verify_parser = subparsers.add_parser("verify")
    verify_parser.add_argument("--handoff", type=Path, required=True)

    smoke_parser = subparsers.add_parser("smoke")
    smoke_parser.add_argument("--handoff", type=Path, required=True)

    exercise_parser = subparsers.add_parser("exercise")
    exercise_parser.add_argument("--source-root", type=Path, default=_source_root())
    return parser


def main() -> int:
    arguments = _parser().parse_args()
    try:
        if arguments.command == "prepare":
            result = prepare(arguments.source_root, arguments.output)
            output: Any = {
                "handoff": str(arguments.output.resolve()),
                "source_package_identity_sha256": result["source_package"][
                    "identity_sha256"
                ],
            }
        elif arguments.command == "verify":
            result = verify(arguments.handoff)
            output = {
                "handoff": str(arguments.handoff.resolve()),
                "source_package_identity_sha256": result["source_package"][
                    "identity_sha256"
                ],
                "verified": True,
            }
        elif arguments.command == "smoke":
            output = smoke(arguments.handoff)
        else:
            output = exercise(arguments.source_root)
    except (HandoffError, OSError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        raise SystemExit(f"source handoff failed: {error}") from error
    print(json.dumps(output, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
