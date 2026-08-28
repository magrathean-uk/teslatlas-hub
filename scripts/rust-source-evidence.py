#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Create and verify exact offline-vendored Rust dependency source evidence."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from typing import Any


SCHEMA = "teslatlas.rust-source-evidence/v1"
ARCHIVE_NAME = "rust-vendored-sources.tar.gz"
INVENTORY_NAME = "rust-source-inventory.json"
MANIFEST_NAME = "rust-source-evidence-manifest.json"
FILES = (ARCHIVE_NAME, INVENTORY_NAME, MANIFEST_NAME)
CRATE_ARCHIVE_DIRECTORY = "crate-archives"
CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
VENDOR_CONFIG = b'''[source.crates-io]\nreplace-with = "vendored-sources"\n\n[source.vendored-sources]\ndirectory = "vendor"\n\n[net]\noffline = true\n'''
MAX_LOCK_BYTES = 8 * 1024 * 1024
MAX_INVENTORY_BYTES = 8 * 1024 * 1024
MAX_MANIFEST_BYTES = 2 * 1024 * 1024
MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_CRATE_BYTES = 128 * 1024 * 1024
MAX_FILE_BYTES = 64 * 1024 * 1024
MAX_CRATE_FILES = 100_000
MAX_PACKAGE_EXPANDED = 512 * 1024 * 1024
MAX_TOTAL_FILES = 1_000_000
MAX_TOTAL_EXPANDED = 4 * 1024 * 1024 * 1024
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
NAME_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$")
VERSION_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.+_-]{0,127}$")


class GateError(RuntimeError):
    pass


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise GateError(f"duplicate JSON key: {key}")
        value[key] = item
    return value


def json_bytes(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()


def parse_json(data: bytes, label: str) -> Any:
    try:
        return json.loads(data, object_pairs_hook=unique_object)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GateError(f"{label} is not valid UTF-8 JSON") from exc


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path, label: str, maximum: int) -> tuple[str, int]:
    data = regular_bytes(path, label, maximum)
    return sha256_bytes(data), len(data)


def regular_bytes(path: Path, label: str, maximum: int, allow_empty: bool = False) -> bytes:
    try:
        before = os.lstat(path)
    except OSError as exc:
        raise GateError(f"{label} is missing: {path}") from exc
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_nlink != 1
        or (before.st_size <= 0 and not allow_empty)
        or before.st_size > maximum
    ):
        raise GateError(f"{label} must be a bounded regular non-symlink file: {path}")
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
            raise GateError(f"{label} changed while opening: {path}")
        value = bytearray()
        while len(value) <= maximum:
            block = os.read(descriptor, min(1024 * 1024, maximum + 1 - len(value)))
            if not block:
                break
            value.extend(block)
        after = os.fstat(descriptor)
        if (
            len(value) != opened.st_size
            or after.st_size != opened.st_size
            or after.st_nlink != 1
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
        ):
            raise GateError(f"{label} changed while reading: {path}")
        if len(value) > maximum:
            raise GateError(f"{label} is oversized: {path}")
        return bytes(value)
    finally:
        os.close(descriptor)


def real_directory(path: Path, label: str) -> Path:
    try:
        metadata = os.lstat(path)
    except OSError as exc:
        raise GateError(f"{label} is missing: {path}") from exc
    if not stat.S_ISDIR(metadata.st_mode):
        raise GateError(f"{label} must be a real non-symlink directory: {path}")
    return path.resolve()


def child_directory(root: Path, parts: tuple[str, ...], label: str) -> Path:
    current = root
    for part in parts:
        current = current / part
        try:
            metadata = os.lstat(current)
        except OSError as exc:
            raise GateError(f"{label} is missing: {current}") from exc
        if not stat.S_ISDIR(metadata.st_mode):
            raise GateError(f"{label} contains a symlink or non-directory: {current}")
    return current


def safe_relative(value: str, label: str) -> str:
    pure = PurePosixPath(value)
    if (
        not value
        or len(value) > 4096
        or pure.is_absolute()
        or "\\" in value
        or "\x00" in value
        or any(part in ("", ".", "..") for part in pure.parts)
    ):
        raise GateError(f"{label} has an unsafe path")
    return value


def validate_identity(name: object, version: object, label: str) -> tuple[str, str]:
    if not isinstance(name, str) or NAME_RE.fullmatch(name) is None:
        raise GateError(f"{label}.name is invalid")
    if not isinstance(version, str) or VERSION_RE.fullmatch(version) is None:
        raise GateError(f"{label}.version is invalid")
    return name, version


def strict_toml_text(data: bytes, label: str) -> str:
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise GateError(f"{label} is not UTF-8") from exc
    if "\0" in text or "\r" in text or not text.endswith("\n"):
        raise GateError(f"{label} is not canonical UTF-8 text")
    if "'''" in text or '\"\"\"' in text:
        raise GateError(f"{label} uses unsupported multiline TOML strings")
    return text


def parse_lock_toml(data: bytes) -> list[dict[str, str]]:
    text = strict_toml_text(data, "Cargo.lock")
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
                    raise GateError("Cargo.lock has duplicate format versions")
                lock_version = int(version_match.group(1))
            elif re.match(r"version(?:\s|=)", stripped):
                raise GateError("Cargo.lock format version is invalid")
            continue
        assert current is not None
        field_match = re.fullmatch(
            r'(name|version|source|checksum)\s*=\s*"([^"\\\x00-\x1f]+)"',
            stripped,
        )
        if field_match is not None:
            field, value = field_match.groups()
            if field in current:
                raise GateError(f"Cargo.lock package has duplicate field: {field}")
            current[field] = value
        elif re.match(r"(?:name|version|source|checksum)(?:\s|=)", stripped):
            raise GateError("Cargo.lock package identity field is invalid")
    finish_package()
    if lock_version != 4:
        raise GateError("Cargo.lock must use reviewed lockfile format version 4")
    if not packages:
        raise GateError("Cargo.lock contains no packages")
    return packages


def parse_root_manifest_identity(data: bytes) -> tuple[str, str]:
    text = strict_toml_text(data, "Cargo.toml")
    in_package = False
    seen_package = False
    fields: dict[str, str] = {}
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
                    raise GateError("Cargo.toml has duplicate package tables")
                seen_package = True
                in_package = True
            else:
                in_package = False
            continue
        if not in_package:
            continue
        field_match = re.fullmatch(
            r'(name|version)\s*=\s*"([^"\\\x00-\x1f]+)"\s*(?:#.*)?',
            stripped,
        )
        if field_match is not None:
            field, value = field_match.groups()
            if field in fields:
                raise GateError(f"Cargo.toml package has duplicate field: {field}")
            fields[field] = value
        elif re.match(r"(?:name|version)(?:\s|=)", stripped):
            raise GateError("Cargo.toml root package identity must use literal basic strings")
    return validate_identity(fields.get("name"), fields.get("version"), "root Cargo package")


def load_lock(repo: Path) -> tuple[bytes, list[dict[str, str]], list[tuple[str, str]]]:
    data = regular_bytes(repo / "Cargo.lock", "Cargo.lock", MAX_LOCK_BYTES)
    packages = parse_lock_toml(data)
    registry: list[dict[str, str]] = []
    workspace: list[tuple[str, str]] = []
    seen: set[tuple[str, str, str | None]] = set()
    for index, candidate in enumerate(packages):
        name, version = validate_identity(candidate.get("name"), candidate.get("version"), f"lock package {index}")
        source = candidate.get("source")
        identity = (name, version, source)
        if identity in seen:
            raise GateError("Cargo.lock contains a duplicate package identity")
        seen.add(identity)
        if source is None:
            if "checksum" in candidate:
                raise GateError("workspace Cargo.lock package unexpectedly has a checksum")
            workspace.append((name, version))
            continue
        if source != CRATES_IO_SOURCE:
            raise GateError(f"unsupported non-crates.io dependency source: {source}")
        checksum = candidate.get("checksum")
        if not isinstance(checksum, str) or SHA256_RE.fullmatch(checksum) is None:
            raise GateError(f"Cargo.lock checksum is invalid for {name} {version}")
        registry.append({"name": name, "version": version, "source": source, "checksum": checksum})
    registry.sort(key=lambda item: (item["name"], item["version"], item["source"]))
    workspace.sort()
    if not registry or not workspace:
        raise GateError("Cargo.lock must contain workspace and registry packages")
    manifest_data = regular_bytes(repo / "Cargo.toml", "Cargo.toml", MAX_LOCK_BYTES)
    root_identity = parse_root_manifest_identity(manifest_data)
    if workspace != [root_identity]:
        raise GateError("only the tagged repository root package may be a path dependency")
    vendor_paths = [f"{item['name']}-{item['version']}" for item in registry]
    if len(vendor_paths) != len(set(vendor_paths)):
        raise GateError("Cargo.lock dependencies collide in versioned vendor paths")
    return data, registry, workspace


def cargo_environment(cargo_home: Path) -> dict[str, str]:
    value = {
        "PATH": os.environ.get("PATH", ""),
        "HOME": os.environ.get("HOME", ""),
        "CARGO_HOME": str(cargo_home),
        "CARGO_NET_OFFLINE": "true",
        "CARGO_TERM_COLOR": "never",
        "GIT_CONFIG_NOSYSTEM": "1",
    }
    if os.environ.get("RUSTUP_TOOLCHAIN"):
        value["RUSTUP_TOOLCHAIN"] = os.environ["RUSTUP_TOOLCHAIN"]
    return value


def run_output(command: list[str], cwd: Path, env: dict[str, str], label: str) -> str:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            text=True,
            capture_output=True,
            check=True,
            timeout=1800,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        detail = exc.stderr.strip() if isinstance(exc, subprocess.CalledProcessError) else ""
        raise GateError(f"{label} failed" + (f": {detail}" if detail else "")) from exc
    return result.stdout.strip()


def metadata_set(repo: Path, cargo: Path, cargo_home: Path) -> set[tuple[str, str, str | None]]:
    output = run_output(
        [str(cargo), "metadata", "--locked", "--offline", "--format-version", "1"],
        repo,
        cargo_environment(cargo_home),
        "offline locked Cargo metadata",
    )
    try:
        value = json.loads(output, object_pairs_hook=unique_object)
        packages = value["packages"]
        nodes = value["resolve"]["nodes"]
    except (json.JSONDecodeError, KeyError, TypeError) as exc:
        raise GateError("Cargo metadata output is invalid") from exc
    if not isinstance(packages, list) or not isinstance(nodes, list) or len(packages) != len(nodes):
        raise GateError("Cargo metadata package graph is incomplete")
    identities: set[tuple[str, str, str | None]] = set()
    ids: set[str] = set()
    for index, item in enumerate(packages):
        if not isinstance(item, dict):
            raise GateError("Cargo metadata package is invalid")
        name, version = validate_identity(item.get("name"), item.get("version"), f"metadata package {index}")
        source = item.get("source")
        if source is not None and source != CRATES_IO_SOURCE:
            raise GateError(f"Cargo metadata contains unsupported source: {source}")
        if source is None:
            manifest_path = item.get("manifest_path")
            if not isinstance(manifest_path, str):
                raise GateError("Cargo metadata path package lacks a manifest path")
            try:
                if Path(manifest_path).resolve(strict=True) != (repo / "Cargo.toml").resolve(strict=True):
                    raise GateError(
                        "only the tagged repository root package may be a path dependency"
                    )
            except OSError as exc:
                raise GateError("Cargo metadata path package manifest is unavailable") from exc
        identity = (name, version, source)
        if identity in identities:
            raise GateError("Cargo metadata contains duplicate package identities")
        identities.add(identity)
        package_id = item.get("id")
        if not isinstance(package_id, str) or package_id in ids:
            raise GateError("Cargo metadata package IDs are invalid")
        ids.add(package_id)
    if {node.get("id") for node in nodes if isinstance(node, dict)} != ids:
        raise GateError("Cargo metadata resolved graph does not cover every package")
    return identities


def crate_archive(cargo_home: Path, item: dict[str, str]) -> bytes:
    cache = child_directory(cargo_home, ("registry", "cache"), "Cargo registry archive cache")
    filename = f"{item['name']}-{item['version']}.crate"
    candidates: list[bytes] = []
    for entry in sorted(os.scandir(cache), key=lambda value: value.name):
        if entry.is_symlink():
            raise GateError("Cargo registry archive cache contains an unsafe entry")
        if not entry.is_dir(follow_symlinks=False):
            continue
        candidate = Path(entry.path) / filename
        if not os.path.lexists(candidate):
            continue
        data = regular_bytes(candidate, f"crate archive {filename}", MAX_CRATE_BYTES)
        if sha256_bytes(data) != item["checksum"]:
            raise GateError(f"crate archive does not match Cargo.lock: {filename}")
        candidates.append(data)
    if not candidates:
        raise GateError(f"locked crate archive is unavailable offline: {filename}")
    if any(value != candidates[0] for value in candidates[1:]):
        raise GateError(f"ambiguous crate archive cache content: {filename}")
    return candidates[0]


def write_new(path: Path, data: bytes, mode: int = 0o444) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, mode)
    try:
        view = memoryview(data)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise GateError(f"short write: {path}")
            view = view[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.chmod(path, mode)


def tree_hash(files: dict[str, str]) -> str:
    value = "".join(f"{files[name]}  {name}\n" for name in sorted(files)).encode()
    return sha256_bytes(value)


def unpack_crate(data: bytes, item: dict[str, str], vendor: Path) -> dict[str, Any]:
    prefix = f"{item['name']}-{item['version']}"
    destination = vendor / prefix
    destination.mkdir(mode=0o755)
    files: dict[str, str] = {}
    folded: set[str] = set()
    expanded = 0
    try:
        archive = tarfile.open(fileobj=io.BytesIO(data), mode="r|gz")
    except (OSError, tarfile.TarError) as exc:
        raise GateError(f"crate archive is invalid: {prefix}") from exc
    with archive:
        member_count = 0
        for member in archive:
            member_count += 1
            if member_count > MAX_CRATE_FILES:
                raise GateError(f"crate archive member count is invalid: {prefix}")
            name = safe_relative(member.name, f"crate archive {prefix}")
            if name != prefix and not name.startswith(prefix + "/"):
                raise GateError(f"crate archive has the wrong root: {prefix}")
            if name == prefix:
                if not member.isdir() or member.size != 0:
                    raise GateError(f"crate archive root is non-canonical: {prefix}")
                continue
            if member.isdir():
                if member.size != 0:
                    raise GateError(f"crate archive directory is non-canonical: {name}")
                continue
            if not member.isreg() or member.size > MAX_FILE_BYTES:
                raise GateError(f"crate archive contains a non-regular member: {name}")
            relative = safe_relative(name[len(prefix) + 1 :], f"crate archive {prefix}")
            if relative == ".cargo-checksum.json":
                raise GateError(f"crate archive reserves .cargo-checksum.json: {prefix}")
            if relative.casefold() in folded:
                raise GateError(f"crate archive has duplicate or case-colliding paths: {prefix}")
            folded.add(relative.casefold())
            expanded += member.size
            if expanded > MAX_PACKAGE_EXPANDED:
                raise GateError(f"crate archive expands beyond the safety limit: {prefix}")
            source = archive.extractfile(member)
            if source is None:
                raise GateError(f"cannot read crate archive member: {name}")
            content = source.read(member.size + 1)
            if len(content) != member.size:
                raise GateError(f"crate archive member has a short read: {name}")
            digest = sha256_bytes(content)
            files[relative] = digest
            write_new(destination / relative, content)
        if member_count == 0:
            raise GateError(f"crate archive member count is invalid: {prefix}")
    if not files or "Cargo.toml" not in files:
        raise GateError(f"crate archive lacks Cargo.toml: {prefix}")
    checksum = json_bytes({"files": dict(sorted(files.items())), "package": item["checksum"]})
    write_new(destination / ".cargo-checksum.json", checksum)
    return {
        **item,
        "vendor_path": prefix,
        "crate_size": len(data),
        "file_count": len(files),
        "expanded_size": expanded,
        "tree_sha256": tree_hash(files),
        "cargo_checksum_sha256": sha256_bytes(checksum),
    }


def scan_vendor_package(root: Path, item: dict[str, Any]) -> tuple[int, int]:
    package = real_directory(root / item["vendor_path"], f"vendor package {item['vendor_path']}")
    checksum_data = regular_bytes(
        package / ".cargo-checksum.json", f"vendor checksum {item['vendor_path']}", MAX_INVENTORY_BYTES
    )
    if sha256_bytes(checksum_data) != item["cargo_checksum_sha256"]:
        raise GateError(f"vendor checksum digest mismatch: {item['vendor_path']}")
    checksum = parse_json(checksum_data, f"vendor checksum {item['vendor_path']}")
    if not isinstance(checksum, dict) or set(checksum) != {"files", "package"}:
        raise GateError(f"vendor checksum schema is invalid: {item['vendor_path']}")
    if checksum["package"] != item["checksum"] or not isinstance(checksum["files"], dict):
        raise GateError(f"vendor checksum lock binding is invalid: {item['vendor_path']}")
    expected_files = checksum["files"]
    actual_files: set[str] = set()
    size = 0
    for current, directories, filenames in os.walk(package, topdown=True, followlinks=False):
        current_path = Path(current)
        for name in directories:
            path = current_path / name
            if stat.S_ISLNK(os.lstat(path).st_mode):
                raise GateError(f"vendor tree contains a symlink: {path}")
        for name in filenames:
            path = current_path / name
            relative = path.relative_to(package).as_posix()
            if relative == ".cargo-checksum.json":
                continue
            data = regular_bytes(path, f"vendor file {relative}", MAX_FILE_BYTES, True)
            digest, length = sha256_bytes(data), len(data)
            if expected_files.get(relative) != digest:
                raise GateError(f"vendor file does not match its checksum: {item['vendor_path']}/{relative}")
            actual_files.add(relative)
            size += length
    if actual_files != set(expected_files):
        raise GateError(f"vendor file set does not match checksums: {item['vendor_path']}")
    if (
        len(actual_files) != item["file_count"]
        or size != item["expanded_size"]
        or tree_hash(expected_files) != item["tree_sha256"]
    ):
        raise GateError(f"vendor inventory mismatch: {item['vendor_path']}")
    return len(actual_files), size


def archive_files(root: Path) -> list[tuple[str, Path]]:
    values: list[tuple[str, Path]] = []
    for current, directories, filenames in os.walk(root, topdown=True, followlinks=False):
        current_path = Path(current)
        directories.sort()
        filenames.sort()
        for name in directories:
            path = current_path / name
            if stat.S_ISLNK(os.lstat(path).st_mode):
                raise GateError(f"vendor source contains a symlink: {path}")
        for name in filenames:
            path = current_path / name
            relative = safe_relative(path.relative_to(root).as_posix(), "vendor archive")
            metadata = os.lstat(path)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
                raise GateError(f"vendor source contains a non-regular file: {path}")
            values.append((relative, path))
    values.sort(key=lambda item: item[0])
    return values


def write_archive(root: Path, output: Path) -> None:
    with output.open("xb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as archive:
                for name, path in archive_files(root):
                    data = regular_bytes(path, f"vendor archive member {name}", MAX_FILE_BYTES, True)
                    member = tarfile.TarInfo(name)
                    member.size = len(data)
                    member.mode = 0o444
                    member.uid = member.gid = 0
                    member.uname = member.gname = ""
                    member.mtime = 0
                    archive.addfile(member, io.BytesIO(data))
        raw.flush()
        os.fsync(raw.fileno())
    os.chmod(output, 0o600)


def extract_archive(data: bytes, output: Path) -> None:
    if len(data) < 10 or data[:3] != b"\x1f\x8b\x08" or data[3] != 0 or data[4:8] != b"\0\0\0\0":
        raise GateError("Rust vendor source archive has a non-canonical gzip header")
    seen: set[str] = set()
    names: list[str] = []
    total_size = 0
    try:
        archive = tarfile.open(fileobj=io.BytesIO(data), mode="r|gz")
    except (OSError, tarfile.TarError) as exc:
        raise GateError("Rust vendor source archive is invalid") from exc
    with archive:
        member_count = 0
        for member in archive:
            member_count += 1
            if member_count > MAX_TOTAL_FILES:
                raise GateError("Rust vendor source archive member count is invalid")
            name = safe_relative(member.name, "Rust vendor source archive")
            if name.casefold() in seen:
                raise GateError("Rust vendor source archive has duplicate or case-colliding paths")
            seen.add(name.casefold())
            names.append(name)
            if (
                not member.isreg()
                or member.size > MAX_FILE_BYTES
                or member.mode != 0o444
                or member.uid != 0
                or member.gid != 0
                or member.uname not in ("", None)
                or member.gname not in ("", None)
                or member.mtime != 0
            ):
                raise GateError(f"Rust vendor source archive member is non-canonical: {name}")
            total_size += member.size
            if total_size > MAX_TOTAL_EXPANDED:
                raise GateError("Rust vendor source archive expands beyond the safety limit")
            source = archive.extractfile(member)
            if source is None:
                raise GateError(f"cannot read Rust vendor source member: {name}")
            content = source.read(member.size + 1)
            if len(content) != member.size:
                raise GateError(f"Rust vendor source member has a short read: {name}")
            write_new(output / name, content)
        if member_count == 0:
            raise GateError("Rust vendor source archive member count is invalid")
    if names != sorted(names):
        raise GateError("Rust vendor source archive is not sorted")


def compare_trees(expected: Path, actual: Path, label: str) -> None:
    expected_files = {name: path for name, path in archive_files(expected)}
    actual_files = {name: path for name, path in archive_files(actual)}
    if set(expected_files) != set(actual_files):
        raise GateError(f"{label} file set differs from locked crate reconstruction")
    for name in sorted(expected_files):
        expected_data = regular_bytes(
            expected_files[name], f"{label} expected file {name}", MAX_FILE_BYTES, True
        )
        actual_data = regular_bytes(
            actual_files[name], f"{label} reconstructed file {name}", MAX_FILE_BYTES, True
        )
        if expected_data != actual_data:
            raise GateError(f"{label} differs from locked crate reconstruction: {name}")


def validate_inventory(value: object, lock_data: bytes, registry: list[dict[str, str]]) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != {
        "schema", "cargo_lock_sha256", "dependency_count", "packages"
    }:
        raise GateError("Rust source inventory schema is invalid")
    if value["schema"] != SCHEMA or value["cargo_lock_sha256"] != sha256_bytes(lock_data):
        raise GateError("Rust source inventory does not bind the exact Cargo.lock")
    packages = value["packages"]
    if not isinstance(packages, list) or value["dependency_count"] != len(packages):
        raise GateError("Rust source inventory dependency count is invalid")
    keys = {
        "name", "version", "source", "checksum", "vendor_path", "crate_size",
        "file_count", "expanded_size", "tree_sha256", "cargo_checksum_sha256",
    }
    for index, item in enumerate(packages):
        if not isinstance(item, dict) or set(item) != keys:
            raise GateError(f"Rust source inventory package {index} schema is invalid")
        validate_identity(item["name"], item["version"], f"inventory package {index}")
        if item["source"] != CRATES_IO_SOURCE:
            raise GateError("Rust source inventory has an unsupported source")
        for field in ("checksum", "tree_sha256", "cargo_checksum_sha256"):
            if not isinstance(item[field], str) or SHA256_RE.fullmatch(item[field]) is None:
                raise GateError(f"Rust source inventory {field} is invalid")
        if item["vendor_path"] != f"{item['name']}-{item['version']}":
            raise GateError("Rust source inventory vendor path is invalid")
        for field in ("crate_size", "file_count", "expanded_size"):
            if not isinstance(item[field], int) or item[field] <= 0:
                raise GateError(f"Rust source inventory {field} is invalid")
    identities = [
        {key: item[key] for key in ("name", "version", "source", "checksum")}
        for item in packages
    ]
    if identities != registry:
        raise GateError("Rust source inventory package set does not match Cargo.lock")
    return value


def build_receipt(repo: Path, cargo: Path, vendor_root: Path, binary: str, work: Path) -> dict[str, Any]:
    cargo_home = work / "offline-cargo-home"
    (cargo_home / ".cargo-placeholder").parent.mkdir(parents=True, exist_ok=True)
    config = (
        '[source.crates-io]\nreplace-with = "vendored-sources"\n\n'
        '[source.vendored-sources]\ndirectory = "'
        + str(vendor_root / "vendor").replace("\\", "\\\\").replace('"', '\\"')
        + '"\n\n[net]\noffline = true\n'
    ).encode()
    write_new(cargo_home / "config.toml", config, 0o600)
    target = work / "offline-target"
    env = cargo_environment(cargo_home)
    env["CARGO_TARGET_DIR"] = str(target)
    command = [str(cargo), "build", "--locked", "--offline", "--release", "--bin", binary]
    run_output(command, repo, env, "offline locked vendored Cargo rebuild")
    artifact = target / "release" / (binary + (".exe" if os.name == "nt" else ""))
    regular_bytes(artifact, "offline rebuilt Rust binary", MAX_ARCHIVE_BYTES)
    cargo_version = run_output([str(cargo), "--version"], repo, env, "Cargo version query")
    rustc = shutil.which("rustc", path=env["PATH"])
    if rustc is None:
        raise GateError("rustc is unavailable")
    rustc_version = run_output([rustc, "--version", "--verbose"], repo, env, "rustc version query")
    host = next((line.split(":", 1)[1].strip() for line in rustc_version.splitlines() if line.startswith("host:")), None)
    if not host:
        raise GateError("rustc did not report a host target")
    return {
        "command": ["cargo", "build", "--locked", "--offline", "--release", "--bin", binary],
        "network_mode": "cargo-resolver-offline-with-vendored-source-replacement",
        "cargo_version": cargo_version,
        "rustc_version": rustc_version,
        "host_target": host,
        "artifact_name": binary,
        "passed": True,
    }


def manifest_value(
    lock_data: bytes, inventory_data: bytes, archive_data: bytes, receipt: dict[str, Any]
) -> dict[str, Any]:
    return {
        "schema": SCHEMA,
        "cargo_lock_sha256": sha256_bytes(lock_data),
        "inventory_sha256": sha256_bytes(inventory_data),
        "vendor_archive_sha256": sha256_bytes(archive_data),
        "vendor_archive_size": len(archive_data),
        "offline_locked_build": receipt,
    }


def validate_receipt(value: object) -> dict[str, Any]:
    keys = {
        "command", "network_mode", "cargo_version", "rustc_version", "host_target",
        "artifact_name", "passed",
    }
    if not isinstance(value, dict) or set(value) != keys:
        raise GateError("Rust offline build receipt schema is invalid")
    if (
        value["command"]
        != ["cargo", "build", "--locked", "--offline", "--release", "--bin", value["artifact_name"]]
        or value["network_mode"] != "cargo-resolver-offline-with-vendored-source-replacement"
        or value["passed"] is not True
        or not isinstance(value["cargo_version"], str)
        or not value["cargo_version"].startswith("cargo ")
        or not isinstance(value["rustc_version"], str)
        or not value["rustc_version"].startswith("rustc ")
        or not isinstance(value["host_target"], str)
        or not value["host_target"]
        or not isinstance(value["artifact_name"], str)
        or NAME_RE.fullmatch(value["artifact_name"]) is None
    ):
        raise GateError("Rust offline build receipt is invalid")
    return value


def validate_directory(repo: Path, directory: Path, rebuild: bool = False) -> dict[str, Any]:
    directory = real_directory(directory, "Rust source evidence")
    if {entry.name for entry in directory.iterdir()} != set(FILES):
        raise GateError("Rust source evidence file set is incomplete or unexpected")
    lock_data, registry, _ = load_lock(repo)
    inventory_data = regular_bytes(directory / INVENTORY_NAME, "Rust source inventory", MAX_INVENTORY_BYTES)
    inventory = validate_inventory(parse_json(inventory_data, "Rust source inventory"), lock_data, registry)
    if inventory_data != json_bytes(inventory):
        raise GateError("Rust source inventory is not canonical JSON")
    archive_data = regular_bytes(directory / ARCHIVE_NAME, "Rust vendor source archive", MAX_ARCHIVE_BYTES)
    manifest_data = regular_bytes(directory / MANIFEST_NAME, "Rust source evidence manifest", MAX_MANIFEST_BYTES)
    manifest = parse_json(manifest_data, "Rust source evidence manifest")
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema", "cargo_lock_sha256", "inventory_sha256", "vendor_archive_sha256",
        "vendor_archive_size", "offline_locked_build",
    }:
        raise GateError("Rust source evidence manifest schema is invalid")
    receipt = validate_receipt(manifest["offline_locked_build"])
    expected_manifest = manifest_value(lock_data, inventory_data, archive_data, receipt)
    if manifest != expected_manifest or manifest_data != json_bytes(manifest):
        raise GateError("Rust source evidence manifest does not bind exact components")
    work = Path(tempfile.mkdtemp(prefix="teslatlas-rust-source-verify-"))
    try:
        extracted = work / "source"
        extracted.mkdir()
        extract_archive(archive_data, extracted)
        config = regular_bytes(extracted / ".cargo" / "config.toml", "vendored Cargo config", MAX_MANIFEST_BYTES)
        if config != VENDOR_CONFIG:
            raise GateError("vendored Cargo config is not the reviewed offline replacement")
        vendor = real_directory(extracted / "vendor", "vendored Rust dependency source")
        crate_archives = real_directory(
            extracted / CRATE_ARCHIVE_DIRECTORY, "locked Rust crate archives"
        )
        expected_dirs = {item["vendor_path"] for item in inventory["packages"]}
        actual_dirs = {entry.name for entry in vendor.iterdir()}
        if actual_dirs != expected_dirs:
            raise GateError("vendored Rust dependency directory set is invalid")
        expected_crates = {
            f"{item['name']}-{item['version']}.crate" for item in registry
        }
        actual_crates = {entry.name for entry in crate_archives.iterdir()}
        if actual_crates != expected_crates:
            raise GateError("locked Rust crate archive file set is invalid")
        reconstructed = work / "locked-reconstruction"
        reconstructed_vendor = reconstructed / "vendor"
        reconstructed_vendor.mkdir(parents=True)
        files = 0
        expanded = 0
        if len(registry) != len(inventory["packages"]):
            raise GateError("Rust source inventory dependency count differs from Cargo.lock")
        for locked, inventoried in zip(registry, inventory["packages"]):
            crate_name = f"{locked['name']}-{locked['version']}.crate"
            crate_data = regular_bytes(
                crate_archives / crate_name,
                f"locked crate archive {crate_name}",
                MAX_CRATE_BYTES,
            )
            if sha256_bytes(crate_data) != locked["checksum"]:
                raise GateError(f"locked crate archive digest differs from Cargo.lock: {crate_name}")
            independently_derived = unpack_crate(crate_data, locked, reconstructed_vendor)
            if independently_derived != inventoried:
                raise GateError(f"Rust source inventory is not derived from Cargo.lock: {crate_name}")
            count, size = scan_vendor_package(vendor, independently_derived)
            files += count
            expanded += size
        if files > MAX_TOTAL_FILES or expanded > MAX_TOTAL_EXPANDED:
            raise GateError("vendored Rust source exceeds evidence limits")
        compare_trees(vendor, reconstructed_vendor, "vendored Rust source")
        canonical = work / "canonical.tar.gz"
        write_archive(extracted, canonical)
        canonical_data = regular_bytes(canonical, "canonical Rust vendor source archive", MAX_ARCHIVE_BYTES)
        if canonical_data != archive_data:
            raise GateError("Rust vendor source archive is not canonical and reproducible")
        if rebuild:
            cargo = Path(os.path.realpath(shutil.which("cargo") or "cargo"))
            regular_bytes(cargo, "Cargo executable", 128 * 1024 * 1024)
            rebuilt_receipt = build_receipt(
                repo,
                cargo,
                reconstructed,
                receipt["artifact_name"],
                work / "verification-build",
            )
            if rebuilt_receipt["artifact_name"] != receipt["artifact_name"]:
                raise GateError("Rust offline rebuild did not reproduce the requested binary")
    finally:
        shutil.rmtree(work, ignore_errors=True)
    return manifest


def generate(repo: Path, cargo: Path, cargo_home: Path, output: Path, binary: str) -> None:
    if os.path.lexists(output):
        raise GateError("Rust source evidence output already exists")
    parent = real_directory(output.parent, "Rust source evidence output parent")
    output = parent / output.name
    lock_data, registry, workspace = load_lock(repo)
    metadata = metadata_set(repo, cargo, cargo_home)
    expected_metadata = {(item["name"], item["version"], item["source"]) for item in registry}
    expected_metadata.update((name, version, None) for name, version in workspace)
    if metadata != expected_metadata:
        raise GateError("offline locked Cargo metadata package set differs from Cargo.lock")
    work = Path(tempfile.mkdtemp(prefix="teslatlas-rust-source-generate-", dir=parent))
    try:
        source = work / "source"
        vendor = source / "vendor"
        crate_archives = source / CRATE_ARCHIVE_DIRECTORY
        (source / ".cargo").mkdir(parents=True)
        vendor.mkdir()
        crate_archives.mkdir()
        write_new(source / ".cargo" / "config.toml", VENDOR_CONFIG)
        packages: list[dict[str, Any]] = []
        total_files = 0
        total_expanded = 0
        for item in registry:
            data = crate_archive(cargo_home, item)
            write_new(
                crate_archives / f"{item['name']}-{item['version']}.crate",
                data,
            )
            package = unpack_crate(data, item, vendor)
            packages.append(package)
            total_files += package["file_count"]
            total_expanded += package["expanded_size"]
            if total_files > MAX_TOTAL_FILES or total_expanded > MAX_TOTAL_EXPANDED:
                raise GateError("locked Rust dependencies exceed evidence limits")
        for item in packages:
            scan_vendor_package(vendor, item)
        receipt = build_receipt(repo, cargo, source, binary, work)
        for item in packages:
            scan_vendor_package(vendor, item)
        inventory = {
            "schema": SCHEMA,
            "cargo_lock_sha256": sha256_bytes(lock_data),
            "dependency_count": len(packages),
            "packages": packages,
        }
        inventory_data = json_bytes(inventory)
        stage = work / "evidence"
        stage.mkdir()
        archive_path = stage / ARCHIVE_NAME
        write_archive(source, archive_path)
        archive_data = regular_bytes(archive_path, "generated Rust vendor source archive", MAX_ARCHIVE_BYTES)
        write_new(stage / INVENTORY_NAME, inventory_data, 0o600)
        write_new(
            stage / MANIFEST_NAME,
            json_bytes(manifest_value(lock_data, inventory_data, archive_data, receipt)),
            0o600,
        )
        validate_directory(repo, stage)
        stage.rename(output)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def default_cargo_home() -> Path:
    value = os.environ.get("CARGO_HOME")
    return Path(os.path.abspath(value)) if value else Path.home() / ".cargo"


def main() -> int:
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--cargo", type=Path, default=Path(shutil.which("cargo") or "cargo"))
    parser.add_argument("--cargo-home", type=Path, default=default_cargo_home())
    parser.add_argument("--bin", default="teslatlas-hub")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--output-dir", type=Path)
    mode.add_argument("--verify-dir", type=Path)
    parser.add_argument(
        "--rebuild",
        action="store_true",
        help="also perform a native offline rebuild from independently reconstructed crate sources",
    )
    args = parser.parse_args()
    repo = real_directory(Path(os.path.abspath(args.repo)), "repository")
    if args.verify_dir is not None:
        if args.bin != "teslatlas-hub" or args.cargo_home != default_cargo_home():
            raise GateError("verification cannot be combined with generation overrides")
        validate_directory(repo, Path(os.path.abspath(args.verify_dir)), rebuild=args.rebuild)
        return 0
    if args.rebuild:
        raise GateError("--rebuild is only valid with --verify-dir")
    cargo = Path(os.path.realpath(os.path.abspath(args.cargo)))
    regular_bytes(cargo, "Cargo executable", 128 * 1024 * 1024)
    cargo_home = real_directory(Path(os.path.abspath(args.cargo_home)), "Cargo home")
    if NAME_RE.fullmatch(args.bin) is None:
        raise GateError("--bin is invalid")
    output_raw = Path(os.path.abspath(args.output_dir))
    parent = real_directory(output_raw.parent, "Rust source evidence output parent")
    output = parent / output_raw.name
    if output.name in ("", ".", ".."):
        raise GateError("Rust source evidence output path is unsafe")
    generate(repo, cargo, cargo_home, output, args.bin)
    print(output)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as exc:
        print(f"rust-source-evidence: {exc}", file=sys.stderr)
        raise SystemExit(1)
