#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Create and verify exact offline legal evidence for Fleet Telemetry."""

from __future__ import annotations

import argparse
import base64
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
from urllib.parse import quote
import zipfile


SCHEMA = "teslatlas.fleet-telemetry-evidence/v1"
LEGAL_LOCK_SCHEMA = "teslatlas.fleet-telemetry-legal-lock/v1"
BRIDGE_LOCK_NAME = "fleet-telemetry-bridge-lock.json"
LEGAL_LOCK_NAME = "fleet-telemetry-legal-lock.json"
MANIFEST_NAME = "fleet-telemetry-component-manifest.json"
BINARY_NAME = "fleet-telemetry.unsigned"
LICENSE_ARCHIVE_NAME = "fleet-telemetry-license-material.tar.gz"
MODULE_SOURCE_ARCHIVE_NAME = "fleet-telemetry-go-module-sources.tar.gz"
INVENTORY_NAME = "fleet-telemetry-dependency-inventory.json"
SBOM_NAME = "fleet-telemetry-sbom.spdx.json"
NOTICES_NAME = "FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md"
UPSTREAM_SOURCE_NAME = "fleet-telemetry-upstream-source.tar.gz"
FILES = (
    NOTICES_NAME,
    BRIDGE_LOCK_NAME,
    MANIFEST_NAME,
    INVENTORY_NAME,
    LEGAL_LOCK_NAME,
    LICENSE_ARCHIVE_NAME,
    MODULE_SOURCE_ARCHIVE_NAME,
    SBOM_NAME,
    UPSTREAM_SOURCE_NAME,
    BINARY_NAME,
)
COMPONENT_FILES = tuple(name for name in FILES if name != MANIFEST_NAME)

MAX_BINARY_BYTES = 128 * 1024 * 1024
MAX_LOCK_BYTES = 1024 * 1024
MAX_SOURCE_ARCHIVE_BYTES = 128 * 1024 * 1024
MAX_SOURCE_MEMBERS = 100_000
MAX_SOURCE_EXPANDED_BYTES = 1024 * 1024 * 1024
MAX_MODULE_ZIP_BYTES = 256 * 1024 * 1024
MAX_MODULE_FILES = 100_000
MAX_MODULE_FILE_BYTES = 64 * 1024 * 1024
MAX_MODULE_EXPANDED_BYTES = 512 * 1024 * 1024
MAX_GO_MOD_BYTES = 4 * 1024 * 1024
MAX_LICENSE_FILE_BYTES = 2 * 1024 * 1024
MAX_LICENSE_ARCHIVE_BYTES = 16 * 1024 * 1024
MAX_LICENSE_ARCHIVE_MEMBERS = 512
MAX_MODULE_SOURCE_ARCHIVE_BYTES = 192 * 1024 * 1024
MAX_MODULE_SOURCE_EXPANDED_BYTES = 256 * 1024 * 1024
MAX_MODULE_SOURCE_MEMBERS = 512
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SUM_RE = re.compile(r"^h1:[A-Za-z0-9+/]{43}=$")
MODULE_PATH_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._~/-]{0,511}$")
MODULE_VERSION_RE = re.compile(r"^v[0-9][A-Za-z0-9.+~-]{0,255}$")
ALLOWED_LICENSES = {
    "Apache-2.0",
    "Apache-2.0 AND BSD-3-Clause",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "EPL-2.0",
    "MIT",
}


class GateError(RuntimeError):
    pass


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise GateError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def json_bytes(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()


def parse_json(data: bytes, label: str) -> Any:
    try:
        return json.loads(data, object_pairs_hook=unique_object)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GateError(f"{label} is not valid UTF-8 JSON") from exc


def require_keys(value: object, keys: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise GateError(f"{label} has an invalid schema")
    return value


def require_string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise GateError(f"{label} must be a non-empty string")
    return value


def validate_sha(value: object, label: str) -> str:
    text = require_string(value, label)
    if SHA256_RE.fullmatch(text) is None:
        raise GateError(f"{label} must be a lowercase SHA-256 digest")
    return text


def validate_sum(value: object, label: str) -> str:
    text = require_string(value, label)
    if SUM_RE.fullmatch(text) is None:
        raise GateError(f"{label} must be a Go h1 checksum")
    return text


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def regular_bytes(path: Path, label: str, maximum: int) -> bytes:
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
        raise GateError(f"{label} must be a bounded regular non-symlink file: {path}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise GateError(f"cannot safely read {label}: {path}") from exc
    try:
        opened = os.fstat(descriptor)
        if (
            (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)
            or opened.st_nlink != 1
        ):
            raise GateError(f"{label} changed while opening: {path}")
        data = bytearray()
        while len(data) <= maximum:
            block = os.read(descriptor, min(1024 * 1024, maximum + 1 - len(data)))
            if not block:
                break
            data.extend(block)
        after = os.fstat(descriptor)
        if (
            len(data) != opened.st_size
            or after.st_size != opened.st_size
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
        ):
            raise GateError(f"{label} changed while reading: {path}")
        if len(data) > maximum:
            raise GateError(f"{label} is oversized: {path}")
        return bytes(data)
    finally:
        os.close(descriptor)


def checked_directory(path: Path, label: str) -> Path:
    try:
        metadata = os.lstat(path)
    except OSError as exc:
        raise GateError(f"{label} is missing: {path}") from exc
    if not stat.S_ISDIR(metadata.st_mode):
        raise GateError(f"{label} must be a real directory, not a symlink: {path}")
    return path.resolve()


def safe_relative_path(value: object, label: str) -> str:
    text = require_string(value, label)
    pure = PurePosixPath(text)
    if (
        len(text) > 4096
        or pure.is_absolute()
        or "\\" in text
        or "\x00" in text
        or any(part in ("", ".", "..") for part in pure.parts)
    ):
        raise GateError(f"{label} is unsafe")
    return text


def validate_module_identity(path: object, version: object, label: str) -> tuple[str, str]:
    module_path = require_string(path, f"{label}.path")
    module_version = require_string(version, f"{label}.version")
    parts = module_path.split("/")
    if (
        MODULE_PATH_RE.fullmatch(module_path) is None
        or "//" in module_path
        or "\\" in module_path
        or "@" in module_path
        or any(part in ("", ".", "..") for part in parts)
    ):
        raise GateError(f"{label}.path is unsafe")
    if MODULE_VERSION_RE.fullmatch(module_version) is None or "/" in module_version:
        raise GateError(f"{label}.version is unsafe")
    return module_path, module_version


def validate_license_files(value: object, label: str) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not value or len(value) > 16:
        raise GateError(f"{label} must contain 1-16 files")
    files: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, candidate in enumerate(value):
        item = require_keys(candidate, {"path", "sha256", "size"}, f"{label}[{index}]")
        path = safe_relative_path(item["path"], f"{label}[{index}].path")
        folded = path.casefold()
        if folded in seen:
            raise GateError(f"{label} contains a duplicate or case-colliding path")
        seen.add(folded)
        digest = validate_sha(item["sha256"], f"{label}[{index}].sha256")
        size = item["size"]
        if not isinstance(size, int) or size <= 0 or size > MAX_LICENSE_FILE_BYTES:
            raise GateError(f"{label}[{index}].size is invalid")
        files.append({"path": path, "sha256": digest, "size": size})
    if [item["path"] for item in files] != sorted(item["path"] for item in files):
        raise GateError(f"{label} is not sorted")
    return files


def validate_bridge_lock(value: object) -> dict[str, Any]:
    lock = require_keys(
        value,
        {"schema", "upstream", "overlay", "bridge", "toolchain", "targets"},
        "Fleet Telemetry bridge lock",
    )
    upstream = require_keys(
        lock["upstream"],
        {"repository", "version", "commit", "archive_url", "archive_sha256"},
        "Fleet Telemetry bridge lock.upstream",
    )
    overlay = require_keys(
        lock["overlay"], {"patch", "patch_sha256"}, "Fleet Telemetry bridge lock.overlay"
    )
    bridge = require_keys(
        lock["bridge"],
        {
            "endpoint",
            "bearer_file_env",
            "envelope_version",
            "max_envelope_bytes",
            "default_timeout_ms",
            "maximum_timeout_ms",
        },
        "Fleet Telemetry bridge lock.bridge",
    )
    toolchain = require_keys(
        lock["toolchain"], {"go_version", "cgo_enabled"}, "Fleet Telemetry bridge lock.toolchain"
    )
    if (
        lock["schema"] != 1
        or upstream["repository"] != "https://github.com/teslamotors/fleet-telemetry"
        or upstream["version"] != "v0.9.4"
        or not isinstance(upstream["commit"], str)
        or re.fullmatch(r"[0-9a-f]{40}", upstream["commit"]) is None
        or not isinstance(upstream["archive_url"], str)
        or not upstream["archive_url"].startswith("https://codeload.github.com/teslamotors/fleet-telemetry/")
        or overlay["patch"] != "0001-teslatlas-http-dispatcher.patch"
        or bridge["endpoint"] != "http://127.0.0.1:8080/v1/internal/fleet-telemetry"
        or toolchain != {"go_version": "go1.27.0", "cgo_enabled": False}
        or lock["targets"]
        != ["darwin-arm64", "darwin-amd64", "linux-arm64", "linux-amd64"]
    ):
        raise GateError("Fleet Telemetry bridge lock is not the reviewed build policy")
    validate_sha(upstream["archive_sha256"], "Fleet Telemetry bridge archive SHA-256")
    validate_sha(overlay["patch_sha256"], "Fleet Telemetry bridge patch SHA-256")
    for key in ("bearer_file_env",):
        require_string(bridge[key], f"Fleet Telemetry bridge lock.bridge.{key}")
    for key in (
        "envelope_version",
        "max_envelope_bytes",
        "default_timeout_ms",
        "maximum_timeout_ms",
    ):
        if not isinstance(bridge[key], int) or bridge[key] <= 0:
            raise GateError(f"Fleet Telemetry bridge lock.bridge.{key} is invalid")
    return lock


def validate_legal_lock(value: object, bridge: dict[str, Any], bridge_bytes: bytes) -> dict[str, Any]:
    lock = require_keys(value, {"schema", "bridge_lock_sha256", "main", "modules"}, "legal lock")
    if lock["schema"] != LEGAL_LOCK_SCHEMA:
        raise GateError("Fleet Telemetry legal lock schema is unsupported")
    if validate_sha(lock["bridge_lock_sha256"], "legal lock.bridge_lock_sha256") != sha256_bytes(
        bridge_bytes
    ):
        raise GateError("Fleet Telemetry legal lock does not bind the bridge lock")
    main = require_keys(
        lock["main"],
        {
            "path",
            "version",
            "commit",
            "archive_sha256",
            "go_mod_sha256",
            "go_sum_sha256",
            "license_expression",
            "license_files",
        },
        "legal lock.main",
    )
    if (
        main["path"] != "github.com/teslamotors/fleet-telemetry"
        or main["version"] != bridge["upstream"]["version"]
        or main["commit"] != bridge["upstream"]["commit"]
        or main["archive_sha256"] != bridge["upstream"]["archive_sha256"]
        or main["license_expression"] != "Apache-2.0"
    ):
        raise GateError("Fleet Telemetry legal lock main source is invalid")
    for field in ("archive_sha256", "go_mod_sha256", "go_sum_sha256"):
        validate_sha(main[field], f"legal lock.main.{field}")
    main["license_files"] = validate_license_files(
        main["license_files"], "legal lock.main.license_files"
    )
    modules = lock["modules"]
    if not isinstance(modules, list) or not modules or len(modules) > 256:
        raise GateError("Fleet Telemetry legal lock runtime module list is invalid")
    identities: list[tuple[str, str]] = []
    normalized: list[dict[str, Any]] = []
    module_keys = {
        "path",
        "version",
        "sum",
        "go_mod_sum",
        "zip_sha256",
        "go_mod_sha256",
        "license_expression",
        "license_files",
    }
    for index, candidate in enumerate(modules):
        label = f"legal lock.modules[{index}]"
        item = require_keys(candidate, module_keys, label)
        path, version = validate_module_identity(item["path"], item["version"], label)
        identity = (path, version)
        if identity in identities:
            raise GateError("Fleet Telemetry legal lock contains duplicate modules")
        identities.append(identity)
        expression = require_string(item["license_expression"], f"{label}.license_expression")
        if expression not in ALLOWED_LICENSES:
            raise GateError(f"{label} has an unreviewed license expression")
        normalized.append(
            {
                "path": path,
                "version": version,
                "sum": validate_sum(item["sum"], f"{label}.sum"),
                "go_mod_sum": validate_sum(item["go_mod_sum"], f"{label}.go_mod_sum"),
                "zip_sha256": validate_sha(item["zip_sha256"], f"{label}.zip_sha256"),
                "go_mod_sha256": validate_sha(
                    item["go_mod_sha256"], f"{label}.go_mod_sha256"
                ),
                "license_expression": expression,
                "license_files": validate_license_files(
                    item["license_files"], f"{label}.license_files"
                ),
            }
        )
    if identities != sorted(identities):
        raise GateError("Fleet Telemetry legal lock runtime modules are not sorted")
    lock["modules"] = normalized
    return lock


def go_hash(entries: dict[str, bytes]) -> str:
    digest = hashlib.sha256()
    for name in sorted(entries):
        if "\n" in name:
            raise GateError("Go module contains a newline in a filename")
        digest.update(f"{sha256_bytes(entries[name])}  {name}\n".encode())
    return "h1:" + base64.b64encode(digest.digest()).decode()


def go_escape(value: str) -> str:
    result: list[str] = []
    for character in value:
        if "A" <= character <= "Z":
            result.extend(("!", character.lower()))
        elif character == "!":
            result.extend(("!", "!"))
        else:
            result.append(character)
    return "".join(result)


def module_cache_paths(cache: Path, path: str, version: str) -> tuple[Path, Path]:
    base = cache / "cache" / "download" / Path(go_escape(path)) / "@v" / go_escape(version)
    return Path(str(base) + ".zip"), Path(str(base) + ".mod")


def inspect_module_zip(
    data: bytes, item: dict[str, Any]
) -> tuple[dict[str, bytes], str]:
    path = item["path"]
    version = item["version"]
    prefix = f"{path}@{version}/"
    wanted = {spec["path"]: spec for spec in item["license_files"]}
    licenses: dict[str, bytes] = {}
    hashes: dict[str, bytes] = {}
    seen: set[str] = set()
    expanded = 0
    try:
        archive = zipfile.ZipFile(io.BytesIO(data))
    except zipfile.BadZipFile as exc:
        raise GateError(f"module zip is invalid: {path}@{version}") from exc
    with archive:
        infos = archive.infolist()
        if not infos or len(infos) > MAX_MODULE_FILES:
            raise GateError(f"module zip member count is invalid: {path}@{version}")
        for info in infos:
            name = info.filename
            unix_mode = (info.external_attr >> 16) & 0xFFFF
            file_type = stat.S_IFMT(unix_mode)
            if info.is_dir() or file_type not in (0, stat.S_IFREG):
                raise GateError(f"module zip contains a non-regular member: {name}")
            if info.flag_bits & 0x1 or info.file_size > MAX_MODULE_FILE_BYTES:
                raise GateError(f"module zip member is invalid: {name}")
            expanded += info.file_size
            if expanded > MAX_MODULE_EXPANDED_BYTES:
                raise GateError(f"module zip expands beyond the safety limit: {path}@{version}")
            if not name.startswith(prefix):
                raise GateError(f"module zip member escapes its module prefix: {name}")
            relative = name[len(prefix) :]
            safe_relative_path(relative, "module zip member")
            folded = relative.casefold()
            if folded in seen:
                raise GateError(f"module zip contains a duplicate or case-colliding path: {name}")
            seen.add(folded)
            try:
                content = archive.read(info)
            except (OSError, RuntimeError, zipfile.BadZipFile) as exc:
                raise GateError(f"cannot read module zip member: {name}") from exc
            if len(content) != info.file_size:
                raise GateError(f"module zip member has a short read: {name}")
            hashes[name] = content
            if relative in wanted:
                licenses[relative] = content
    if set(licenses) != set(wanted):
        raise GateError(f"module zip has incomplete reviewed legal material: {path}@{version}")
    for relative, content in licenses.items():
        spec = wanted[relative]
        if len(content) != spec["size"] or sha256_bytes(content) != spec["sha256"]:
            raise GateError(f"module legal material does not match the reviewed lock: {path}/{relative}")
        try:
            content.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise GateError(f"module legal material is not UTF-8: {path}/{relative}") from exc
    return licenses, go_hash(hashes)


def source_material(source_data: bytes, legal: dict[str, Any]) -> dict[str, bytes]:
    main = legal["main"]
    root = f"fleet-telemetry-{main['commit']}"
    wanted = {spec["path"]: spec for spec in main["license_files"]}
    wanted.update({
        "go.mod": {"sha256": main["go_mod_sha256"]},
        "go.sum": {"sha256": main["go_sum_sha256"]},
    })
    captured: dict[str, bytes] = {}
    seen: set[str] = set()
    expanded = 0
    try:
        archive = tarfile.open(fileobj=io.BytesIO(source_data), mode="r:gz")
    except (tarfile.TarError, OSError) as exc:
        raise GateError("Fleet Telemetry source archive is invalid") from exc
    with archive:
        members = archive.getmembers()
        if not members or len(members) > MAX_SOURCE_MEMBERS:
            raise GateError("Fleet Telemetry source archive member count is invalid")
        for member in members:
            name = member.name
            safe_relative_path(name, "Fleet Telemetry source archive member")
            folded = name.casefold()
            if folded in seen:
                raise GateError("Fleet Telemetry source archive has duplicate paths")
            seen.add(folded)
            if not name.startswith(root + "/") and name != root:
                raise GateError("Fleet Telemetry source archive has the wrong root")
            if member.isdir():
                continue
            if not member.isreg() or member.size > MAX_MODULE_FILE_BYTES:
                raise GateError("Fleet Telemetry source archive contains an unsafe member")
            expanded += member.size
            if expanded > MAX_SOURCE_EXPANDED_BYTES:
                raise GateError("Fleet Telemetry source archive expands beyond the safety limit")
            relative = name[len(root) + 1 :]
            if relative not in wanted:
                continue
            extracted = archive.extractfile(member)
            if extracted is None:
                raise GateError("cannot read Fleet Telemetry source archive member")
            content = extracted.read(member.size + 1)
            if len(content) != member.size:
                raise GateError("Fleet Telemetry source archive member has a short read")
            captured[relative] = content
    if set(captured) != set(wanted):
        raise GateError("Fleet Telemetry source archive is missing reviewed source material")
    for relative, spec in wanted.items():
        content = captured[relative]
        if sha256_bytes(content) != spec["sha256"]:
            raise GateError(f"Fleet Telemetry source material does not match the legal lock: {relative}")
        if "size" in spec and len(content) != spec["size"]:
            raise GateError(f"Fleet Telemetry source legal material has the wrong size: {relative}")
        if relative not in ("go.mod", "go.sum"):
            try:
                content.decode("utf-8")
            except UnicodeDecodeError as exc:
                raise GateError(f"Fleet Telemetry source legal material is not UTF-8: {relative}") from exc
    return captured


def collect_legal_material(
    source_data: bytes, module_cache: Path, legal: dict[str, Any]
) -> tuple[dict[str, bytes], dict[str, bytes]]:
    main_material = source_material(source_data, legal)
    materials = {
        f"main/{spec['path']}": main_material[spec["path"]]
        for spec in legal["main"]["license_files"]
    }
    module_sources: dict[str, bytes] = {}
    for index, item in enumerate(legal["modules"]):
        zip_path, mod_path = module_cache_paths(module_cache, item["path"], item["version"])
        zip_data = regular_bytes(
            zip_path, f"module zip {item['path']}@{item['version']}", MAX_MODULE_ZIP_BYTES
        )
        mod_data = regular_bytes(
            mod_path, f"module go.mod {item['path']}@{item['version']}", MAX_GO_MOD_BYTES
        )
        if sha256_bytes(zip_data) != item["zip_sha256"]:
            raise GateError(f"module zip digest does not match the legal lock: {item['path']}")
        if sha256_bytes(mod_data) != item["go_mod_sha256"]:
            raise GateError(f"module go.mod digest does not match the legal lock: {item['path']}")
        if go_hash({"go.mod": mod_data}) != item["go_mod_sum"]:
            raise GateError(f"module go.mod h1 does not match the legal lock: {item['path']}")
        licenses, module_sum = inspect_module_zip(zip_data, item)
        if module_sum != item["sum"]:
            raise GateError(f"module zip h1 does not match the legal lock: {item['path']}")
        module_sources[f"modules/{index:03d}/source.zip"] = zip_data
        module_sources[f"modules/{index:03d}/go.mod"] = mod_data
        for spec in item["license_files"]:
            materials[f"modules/{index:03d}/{spec['path']}"] = licenses[spec["path"]]
    return materials, module_sources


def add_tar_member(archive: tarfile.TarFile, name: str, data: bytes) -> None:
    member = tarfile.TarInfo(name)
    member.size = len(data)
    member.mode = 0o644
    member.uid = member.gid = 0
    member.uname = member.gname = ""
    member.mtime = 0
    archive.addfile(member, io.BytesIO(data))


def license_archive_bytes(materials: dict[str, bytes]) -> bytes:
    compressed = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=compressed, mtime=0, compresslevel=9) as stream:
        with tarfile.open(fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for name in sorted(materials):
                add_tar_member(archive, name, materials[name])
    return compressed.getvalue()


def module_source_archive_bytes(sources: dict[str, bytes]) -> bytes:
    compressed = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=compressed, mtime=0, compresslevel=9) as stream:
        with tarfile.open(fileobj=stream, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for name in sorted(sources):
                add_tar_member(archive, name, sources[name])
    value = compressed.getvalue()
    if len(value) > MAX_MODULE_SOURCE_ARCHIVE_BYTES:
        raise GateError("Fleet Telemetry module source archive exceeds the release limit")
    return value


def expected_module_source_paths(legal: dict[str, Any]) -> set[str]:
    return {
        f"modules/{index:03d}/{name}"
        for index, _item in enumerate(legal["modules"])
        for name in ("source.zip", "go.mod")
    }


def expected_material_paths(legal: dict[str, Any]) -> set[str]:
    paths = {f"main/{spec['path']}" for spec in legal["main"]["license_files"]}
    for index, item in enumerate(legal["modules"]):
        paths.update(f"modules/{index:03d}/{spec['path']}" for spec in item["license_files"])
    return paths


def parse_license_archive(data: bytes, legal: dict[str, Any]) -> dict[str, bytes]:
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(data), mode="rb") as stream:
            expanded = stream.read(MAX_LICENSE_ARCHIVE_BYTES + 1)
    except (OSError, EOFError, gzip.BadGzipFile) as exc:
        raise GateError("Fleet Telemetry license material is not a valid gzip stream") from exc
    if len(expanded) > MAX_LICENSE_ARCHIVE_BYTES:
        raise GateError("Fleet Telemetry license material expands beyond the safety limit")
    values: dict[str, bytes] = {}
    try:
        with tarfile.open(fileobj=io.BytesIO(expanded), mode="r:") as archive:
            members = archive.getmembers()
            if not members or len(members) > MAX_LICENSE_ARCHIVE_MEMBERS:
                raise GateError("Fleet Telemetry license archive member count is invalid")
            for member in members:
                name = safe_relative_path(member.name, "Fleet Telemetry license archive member")
                if not member.isreg() or member.size <= 0 or member.size > MAX_LICENSE_FILE_BYTES:
                    raise GateError("Fleet Telemetry license archive contains a non-regular member")
                if name.casefold() in {item.casefold() for item in values}:
                    raise GateError("Fleet Telemetry license archive has duplicate paths")
                source = archive.extractfile(member)
                if source is None:
                    raise GateError("cannot read Fleet Telemetry license archive member")
                content = source.read(member.size + 1)
                if len(content) != member.size:
                    raise GateError("Fleet Telemetry license archive member has a short read")
                values[name] = content
    except (tarfile.TarError, OSError) as exc:
        raise GateError("Fleet Telemetry license material is not a valid tar archive") from exc
    if set(values) != expected_material_paths(legal):
        raise GateError("Fleet Telemetry license archive member set does not match the legal lock")
    for spec, material_path in iter_legal_specs(legal):
        content = values[material_path]
        if len(content) != spec["size"] or sha256_bytes(content) != spec["sha256"]:
            raise GateError(f"Fleet Telemetry license archive content mismatch: {material_path}")
        try:
            content.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise GateError(f"Fleet Telemetry license archive is not UTF-8: {material_path}") from exc
    if data != license_archive_bytes(values):
        raise GateError("Fleet Telemetry license archive is not in canonical reproducible form")
    return values


def parse_module_source_archive(data: bytes, legal: dict[str, Any]) -> dict[str, bytes]:
    values: dict[str, bytes] = {}
    names: list[str] = []
    expanded = 0
    try:
        archive = tarfile.open(fileobj=io.BytesIO(data), mode="r|gz")
    except (tarfile.TarError, OSError) as exc:
        raise GateError("Fleet Telemetry module sources are not a valid gzip tar archive") from exc
    with archive:
        for member in archive:
            if len(names) >= MAX_MODULE_SOURCE_MEMBERS:
                raise GateError("Fleet Telemetry module source archive has too many members")
            name = safe_relative_path(member.name, "Fleet Telemetry module source member")
            if name.casefold() in {item.casefold() for item in values}:
                raise GateError("Fleet Telemetry module source archive has duplicate paths")
            if (
                not member.isreg()
                or member.mode != 0o644
                or member.uid != 0
                or member.gid != 0
                or member.uname not in ("", None)
                or member.gname not in ("", None)
                or member.mtime != 0
            ):
                raise GateError("Fleet Telemetry module source archive has non-canonical metadata")
            maximum = MAX_MODULE_ZIP_BYTES if name.endswith("/source.zip") else MAX_GO_MOD_BYTES
            if member.size <= 0 or member.size > maximum:
                raise GateError("Fleet Telemetry module source archive has an invalid member size")
            expanded += member.size
            if expanded > MAX_MODULE_SOURCE_EXPANDED_BYTES:
                raise GateError("Fleet Telemetry module source archive expands beyond the safety limit")
            source = archive.extractfile(member)
            if source is None:
                raise GateError("cannot read Fleet Telemetry module source member")
            content = source.read(member.size + 1)
            if len(content) != member.size:
                raise GateError("Fleet Telemetry module source archive has a short member")
            values[name] = content
            names.append(name)
    expected = expected_module_source_paths(legal)
    if set(values) != expected or names != sorted(names):
        raise GateError("Fleet Telemetry module source archive member set is invalid")
    for index, item in enumerate(legal["modules"]):
        zip_data = values[f"modules/{index:03d}/source.zip"]
        mod_data = values[f"modules/{index:03d}/go.mod"]
        if sha256_bytes(zip_data) != item["zip_sha256"]:
            raise GateError(f"module source zip does not match the legal lock: {item['path']}")
        if sha256_bytes(mod_data) != item["go_mod_sha256"]:
            raise GateError(f"module source go.mod does not match the legal lock: {item['path']}")
        if go_hash({"go.mod": mod_data}) != item["go_mod_sum"]:
            raise GateError(f"module source go.mod h1 does not match the legal lock: {item['path']}")
        _licenses, module_sum = inspect_module_zip(zip_data, item)
        if module_sum != item["sum"]:
            raise GateError(f"module source zip h1 does not match the legal lock: {item['path']}")
    if data != module_source_archive_bytes(values):
        raise GateError("Fleet Telemetry module source archive is not canonical and reproducible")
    return values


def iter_legal_specs(legal: dict[str, Any]):
    for spec in legal["main"]["license_files"]:
        yield spec, f"main/{spec['path']}"
    for index, item in enumerate(legal["modules"]):
        for spec in item["license_files"]:
            yield spec, f"modules/{index:03d}/{spec['path']}"


def dependency_inventory(
    bridge_bytes: bytes, legal_bytes: bytes, legal: dict[str, Any]
) -> dict[str, Any]:
    return {
        "schema": SCHEMA,
        "bridge_lock_sha256": sha256_bytes(bridge_bytes),
        "legal_lock_sha256": sha256_bytes(legal_bytes),
        "main": legal["main"],
        "runtime_dependency_count": len(legal["modules"]),
        "runtime_dependencies": legal["modules"],
    }


def spdx_document(legal_bytes: bytes, legal: dict[str, Any]) -> dict[str, Any]:
    packages: list[dict[str, Any]] = []
    relationships: list[dict[str, str]] = []
    main = legal["main"]
    packages.append(
        {
            "SPDXID": "SPDXRef-FleetTelemetry",
            "name": main["path"],
            "versionInfo": main["version"],
            "downloadLocation": (
                "https://github.com/teslamotors/fleet-telemetry/archive/" + main["commit"] + ".tar.gz"
            ),
            "filesAnalyzed": False,
            "checksums": [{"algorithm": "SHA256", "checksumValue": main["archive_sha256"]}],
            "licenseConcluded": main["license_expression"],
            "licenseDeclared": main["license_expression"],
            "copyrightText": "NOASSERTION",
        }
    )
    relationships.append(
        {
            "spdxElementId": "SPDXRef-DOCUMENT",
            "relationshipType": "DESCRIBES",
            "relatedSpdxElement": "SPDXRef-FleetTelemetry",
        }
    )
    for index, item in enumerate(legal["modules"]):
        package_id = f"SPDXRef-GoModule-{index:03d}"
        packages.append(
            {
                "SPDXID": package_id,
                "name": item["path"],
                "versionInfo": item["version"],
                "downloadLocation": (
                    "https://proxy.golang.org/"
                    + quote(item["path"], safe="/")
                    + "/@v/"
                    + quote(item["version"], safe="")
                    + ".zip"
                ),
                "filesAnalyzed": False,
                "checksums": [{"algorithm": "SHA256", "checksumValue": item["zip_sha256"]}],
                "licenseConcluded": item["license_expression"],
                "licenseDeclared": item["license_expression"],
                "copyrightText": "NOASSERTION",
                "externalRefs": [
                    {
                        "referenceCategory": "PACKAGE-MANAGER",
                        "referenceType": "purl",
                        "referenceLocator": (
                            "pkg:golang/"
                            + quote(item["path"], safe="/")
                            + "@"
                            + quote(item["version"], safe="")
                        ),
                    }
                ],
            }
        )
        relationships.append(
            {
                "spdxElementId": "SPDXRef-FleetTelemetry",
                "relationshipType": "DEPENDS_ON",
                "relatedSpdxElement": package_id,
            }
        )
    return {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": "teslatlas-fleet-telemetry-go-components",
        "documentNamespace": (
            "https://teslatlas.eu/spdx/fleet-telemetry/" + sha256_bytes(legal_bytes)
        ),
        "creationInfo": {
            "created": "1970-01-01T00:00:00Z",
            "creators": ["Tool: teslatlas-fleet-telemetry-evidence"],
        },
        "documentDescribes": ["SPDXRef-FleetTelemetry"],
        "packages": packages,
        "relationships": relationships,
    }


def notices(legal: dict[str, Any], materials: dict[str, bytes]) -> bytes:
    lines = [
        "# Fleet Telemetry Go third-party notices",
        "",
        "Complete reviewed license and NOTICE texts for the exact CGO-disabled runtime module graph follow.",
        "The exact source zip and go.mod for every runtime module accompany this notice in `fleet-telemetry-go-module-sources.tar.gz`.",
        "",
    ]
    entries = [
        ("main", legal["main"], "main", "fleet-telemetry-upstream-source.tar.gz")
    ]
    entries.extend(
        (
            "runtime dependency",
            item,
            f"modules/{index:03d}",
            f"fleet-telemetry-go-module-sources.tar.gz:modules/{index:03d}/source.zip",
        )
        for index, item in enumerate(legal["modules"])
    )
    for kind, item, prefix, source_location in entries:
        lines.extend(
            [
                f"## {item['path']} {item['version']}",
                "",
                f"Component: {kind}",
                "",
                f"SPDX license expression: {item['license_expression']}",
                "",
                f"Exact source: `{source_location}`",
                "",
            ]
        )
        if item["license_expression"] == "EPL-2.0":
            lines.extend(
                [
                    "Source availability: this Program's exact Source Code is supplied under EPL-2.0 at the source location above. Download and extract the Fleet Telemetry evidence archive, then extract that module source zip.",
                    "",
                ]
            )
        for spec in item["license_files"]:
            material_path = f"{prefix}/{spec['path']}"
            content = materials[material_path].decode("utf-8")
            lines.extend(
                [
                    f"### {spec['path']}",
                    "",
                    f"SHA-256: `{spec['sha256']}`",
                    "",
                    "----- BEGIN EXACT LEGAL TEXT -----",
                    *content.rstrip("\n").splitlines(),
                    "----- END EXACT LEGAL TEXT -----",
                    "",
                ]
            )
    return ("\n".join(lines).rstrip() + "\n").encode()


def component_records(components: dict[str, bytes]) -> list[dict[str, Any]]:
    return [
        {"path": name, "sha256": sha256_bytes(components[name]), "size": len(components[name])}
        for name in sorted(components)
    ]


def manifest(
    bridge_bytes: bytes,
    legal_bytes: bytes,
    legal: dict[str, Any],
    binary: bytes,
    target: str,
    components: dict[str, bytes],
) -> dict[str, Any]:
    records = component_records(components)
    component_set = "".join(
        f"{item['sha256']}  {item['path']}\n" for item in records
    ).encode()
    return {
        "schema": SCHEMA,
        "subject": {
            "name": "fleet-telemetry",
            "sha256": sha256_bytes(binary),
            "size": len(binary),
            "target": target,
        },
        "bridge_lock_sha256": sha256_bytes(bridge_bytes),
        "legal_lock_sha256": sha256_bytes(legal_bytes),
        "source_archive_sha256": legal["main"]["archive_sha256"],
        "module_source_archive_sha256": sha256_bytes(components[MODULE_SOURCE_ARCHIVE_NAME]),
        "runtime_dependency_count": len(legal["modules"]),
        "legal_material_complete": True,
        "source_material_complete": True,
        "components": records,
        "component_set_sha256": sha256_bytes(component_set),
    }


def evidence_components(
    bridge_bytes: bytes,
    legal_bytes: bytes,
    legal: dict[str, Any],
    binary: bytes,
    source_data: bytes,
    module_source_archive: bytes,
    license_archive: bytes,
    materials: dict[str, bytes],
) -> dict[str, bytes]:
    return {
        BRIDGE_LOCK_NAME: bridge_bytes,
        LEGAL_LOCK_NAME: legal_bytes,
        BINARY_NAME: binary,
        UPSTREAM_SOURCE_NAME: source_data,
        MODULE_SOURCE_ARCHIVE_NAME: module_source_archive,
        LICENSE_ARCHIVE_NAME: license_archive,
        INVENTORY_NAME: json_bytes(dependency_inventory(bridge_bytes, legal_bytes, legal)),
        SBOM_NAME: json_bytes(spdx_document(legal_bytes, legal)),
        NOTICES_NAME: notices(legal, materials),
    }


def validate_directory(directory: Path, repo: Path) -> dict[str, Any]:
    directory = checked_directory(directory, "Fleet Telemetry evidence")
    actual = {path.name for path in directory.iterdir()}
    if actual != set(FILES):
        raise GateError("Fleet Telemetry evidence file set is invalid")
    bridge_path = repo / "packaging" / "fleet-telemetry-bridge" / BRIDGE_LOCK_NAME
    legal_path = repo / "packaging" / "fleet-telemetry-bridge" / LEGAL_LOCK_NAME
    expected_bridge = regular_bytes(bridge_path, "reviewed Fleet Telemetry bridge lock", MAX_LOCK_BYTES)
    expected_legal = regular_bytes(legal_path, "reviewed Fleet Telemetry legal lock", MAX_LOCK_BYTES)
    bridge_bytes = regular_bytes(directory / BRIDGE_LOCK_NAME, "Fleet Telemetry bridge lock", MAX_LOCK_BYTES)
    legal_bytes = regular_bytes(directory / LEGAL_LOCK_NAME, "Fleet Telemetry legal lock", MAX_LOCK_BYTES)
    if bridge_bytes != expected_bridge:
        raise GateError("Fleet Telemetry evidence does not match the reviewed repository bridge lock")
    if legal_bytes != expected_legal:
        raise GateError("Fleet Telemetry evidence does not match the reviewed repository legal lock")
    bridge = validate_bridge_lock(parse_json(bridge_bytes, "Fleet Telemetry bridge lock"))
    legal = validate_legal_lock(parse_json(legal_bytes, "Fleet Telemetry legal lock"), bridge, bridge_bytes)
    binary = regular_bytes(directory / BINARY_NAME, "Fleet Telemetry receiver", MAX_BINARY_BYTES)
    source_data = regular_bytes(
        directory / UPSTREAM_SOURCE_NAME,
        "Fleet Telemetry upstream source archive",
        MAX_SOURCE_ARCHIVE_BYTES,
    )
    if sha256_bytes(source_data) != legal["main"]["archive_sha256"]:
        raise GateError("Fleet Telemetry upstream source archive does not match the legal lock")
    source_material(source_data, legal)
    module_source_archive = regular_bytes(
        directory / MODULE_SOURCE_ARCHIVE_NAME,
        "Fleet Telemetry Go module source archive",
        MAX_MODULE_SOURCE_ARCHIVE_BYTES,
    )
    parse_module_source_archive(module_source_archive, legal)
    license_archive = regular_bytes(
        directory / LICENSE_ARCHIVE_NAME,
        "Fleet Telemetry license material",
        MAX_LICENSE_ARCHIVE_BYTES,
    )
    materials = parse_license_archive(license_archive, legal)
    expected_components = evidence_components(
        bridge_bytes,
        legal_bytes,
        legal,
        binary,
        source_data,
        module_source_archive,
        license_archive,
        materials,
    )
    for name, expected in expected_components.items():
        actual_data = regular_bytes(
            directory / name,
            f"Fleet Telemetry evidence component {name}",
            MAX_BINARY_BYTES,
        )
        if actual_data != expected:
            raise GateError(f"Fleet Telemetry evidence component does not match exact inputs: {name}")
    manifest_value = parse_json(
        regular_bytes(directory / MANIFEST_NAME, "Fleet Telemetry component manifest", MAX_LOCK_BYTES),
        "Fleet Telemetry component manifest",
    )
    if not isinstance(manifest_value, dict):
        raise GateError("Fleet Telemetry component manifest must be an object")
    subject = manifest_value.get("subject")
    if not isinstance(subject, dict) or subject.get("target") not in bridge["targets"]:
        raise GateError("Fleet Telemetry component manifest target is invalid")
    expected_manifest = manifest(
        bridge_bytes,
        legal_bytes,
        legal,
        binary,
        subject["target"],
        expected_components,
    )
    if manifest_value != expected_manifest:
        raise GateError("Fleet Telemetry component manifest does not bind the exact evidence set")
    return expected_manifest


def write_file(path: Path, data: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, 0o600)
    try:
        written = 0
        while written < len(data):
            written += os.write(descriptor, data[written:])
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def default_module_cache() -> Path:
    configured = os.environ.get("GOMODCACHE")
    if configured:
        return Path(os.path.abspath(configured))
    go = shutil.which("go")
    if go is None:
        raise GateError("--module-cache is required when Go is unavailable")
    environment = {
        "PATH": os.environ.get("PATH", ""),
        "HOME": os.environ.get("HOME", ""),
        "GOENV": "off",
        "GOWORK": "off",
        "GOTOOLCHAIN": "local",
    }
    try:
        result = subprocess.run(
            [go, "env", "GOMODCACHE"],
            env=environment,
            text=True,
            capture_output=True,
            check=True,
            timeout=15,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        raise GateError("cannot locate the offline Go module cache") from exc
    value = result.stdout.strip()
    if not value or "\n" in value:
        raise GateError("Go reported an invalid module cache path")
    return Path(os.path.abspath(value))


def generate(
    repo: Path,
    receiver_path: Path,
    source_path: Path,
    module_cache: Path,
    output: Path,
    target: str,
) -> None:
    bridge_path = repo / "packaging" / "fleet-telemetry-bridge" / BRIDGE_LOCK_NAME
    legal_path = repo / "packaging" / "fleet-telemetry-bridge" / LEGAL_LOCK_NAME
    patch_path = repo / "packaging" / "fleet-telemetry-bridge" / "0001-teslatlas-http-dispatcher.patch"
    bridge_bytes = regular_bytes(bridge_path, "Fleet Telemetry bridge lock", MAX_LOCK_BYTES)
    legal_bytes = regular_bytes(legal_path, "Fleet Telemetry legal lock", MAX_LOCK_BYTES)
    bridge = validate_bridge_lock(parse_json(bridge_bytes, "Fleet Telemetry bridge lock"))
    legal = validate_legal_lock(parse_json(legal_bytes, "Fleet Telemetry legal lock"), bridge, bridge_bytes)
    if target not in bridge["targets"]:
        raise GateError("Fleet Telemetry evidence target is not reviewed")
    patch_bytes = regular_bytes(patch_path, "Fleet Telemetry bridge patch", MAX_LOCK_BYTES)
    if sha256_bytes(patch_bytes) != bridge["overlay"]["patch_sha256"]:
        raise GateError("Fleet Telemetry bridge patch does not match the bridge lock")
    binary = regular_bytes(receiver_path, "Fleet Telemetry receiver", MAX_BINARY_BYTES)
    source_data = regular_bytes(source_path, "Fleet Telemetry source archive", MAX_SOURCE_ARCHIVE_BYTES)
    if sha256_bytes(source_data) != legal["main"]["archive_sha256"]:
        raise GateError("Fleet Telemetry source archive does not match the legal lock")
    materials, module_sources = collect_legal_material(source_data, module_cache, legal)
    module_source_archive = module_source_archive_bytes(module_sources)
    license_archive = license_archive_bytes(materials)
    components = evidence_components(
        bridge_bytes,
        legal_bytes,
        legal,
        binary,
        source_data,
        module_source_archive,
        license_archive,
        materials,
    )
    manifest_data = json_bytes(
        manifest(bridge_bytes, legal_bytes, legal, binary, target, components)
    )
    stage = Path(tempfile.mkdtemp(prefix="teslatlas-fleet-telemetry-evidence-", dir=output.parent))
    try:
        for name, data in components.items():
            write_file(stage / name, data)
        write_file(stage / MANIFEST_NAME, manifest_data)
        validate_directory(stage, repo)
        stage.rename(output)
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--receiver-binary", type=Path)
    parser.add_argument("--source-archive", type=Path)
    parser.add_argument("--module-cache", type=Path)
    parser.add_argument("--target", default="darwin-arm64")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--verify-dir", type=Path)
    args = parser.parse_args()

    repo = checked_directory(Path(os.path.abspath(args.repo)), "repository")
    if args.verify_dir is not None:
        if any(
            value is not None
            for value in (args.receiver_binary, args.source_archive, args.module_cache, args.output_dir)
        ) or args.target != "darwin-arm64":
            raise GateError("--verify-dir cannot be combined with generation inputs")
        validate_directory(Path(os.path.abspath(args.verify_dir)), repo)
        return 0
    if args.receiver_binary is None or args.output_dir is None:
        raise GateError("--receiver-binary and --output-dir are required")

    bridge_path = repo / "packaging" / "fleet-telemetry-bridge" / BRIDGE_LOCK_NAME
    bridge_bytes = regular_bytes(bridge_path, "Fleet Telemetry bridge lock", MAX_LOCK_BYTES)
    bridge = validate_bridge_lock(parse_json(bridge_bytes, "Fleet Telemetry bridge lock"))
    source_path = (
        Path(os.path.abspath(args.source_archive))
        if args.source_archive is not None
        else repo
        / "target"
        / "upstream-cache"
        / (
            "fleet-telemetry-"
            + bridge["upstream"]["commit"]
            + "-"
            + bridge["upstream"]["archive_sha256"]
            + ".tar.gz"
        )
    )
    module_cache = checked_directory(
        Path(os.path.abspath(args.module_cache))
        if args.module_cache is not None
        else default_module_cache(),
        "offline Go module cache",
    )
    output_raw = Path(os.path.abspath(args.output_dir))
    output_parent = checked_directory(output_raw.parent, "output parent")
    output = output_parent / output_raw.name
    if output.name in ("", ".", "..") or os.path.lexists(output):
        raise GateError("Fleet Telemetry evidence output already exists or is unsafe")
    generate(
        repo,
        Path(os.path.abspath(args.receiver_binary)),
        source_path,
        module_cache,
        output,
        args.target,
    )
    print(output)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as error:
        print(f"fleet-telemetry-evidence: {error}", file=sys.stderr)
        raise SystemExit(1)
