#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Generate fail-closed, reproducible evidence for Tesla's Go command proxy."""

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


SCHEMA = "teslatlas.go-proxy-evidence/v2"
LOCK_SCHEMA = "teslatlas.tesla-proxy-lock/v2"
PACKAGE = "github.com/teslamotors/vehicle-command/cmd/tesla-http-proxy"
MAX_LOCK_BYTES = 128 * 1024
MAX_BINARY_BYTES = 128 * 1024 * 1024
MAX_TOOL_BYTES = 256 * 1024 * 1024
MAX_ZIP_BYTES = 64 * 1024 * 1024
MAX_MOD_BYTES = 2 * 1024 * 1024
MAX_ZIP_FILES = 50_000
MAX_ZIP_FILE_BYTES = 32 * 1024 * 1024
MAX_ZIP_EXPANDED_BYTES = 256 * 1024 * 1024
MAX_LICENSE_BYTES = 1024 * 1024
MAX_OVERLAY_BYTES = 1024 * 1024
MAX_SOURCE_ARCHIVE_BYTES = 96 * 1024 * 1024
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SUM_RE = re.compile(r"^h1:[A-Za-z0-9+/]{43}=$")
SAFE_LICENSE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
MODULE_PATH_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._~/-]{0,511}$")
MODULE_VERSION_RE = re.compile(r"^v[0-9][A-Za-z0-9.+~-]{0,255}$")
ALLOWED_LICENSES = {
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "MIT",
    "MIT AND BSD-3-Clause",
}
TOOLCHAIN_POLICY = {
    "go_version": "go1.27.0",
    "trimpath": True,
    "buildvcs": False,
    "ldflags": "-s -w",
    "buildmode": "exe",
    "compiler": "gc",
    "godebug_default": "go1.27",
}
TARGET_POLICIES = {
    "darwin-arm64": {
        "goos": "darwin",
        "goarch": "arm64",
        "cgo_enabled": "1",
        "architecture_level_key": "GOARM64",
        "architecture_level_value": "v8.0",
        "macosx_deployment_target": "13.0",
        "runtime_module_paths": [
            "github.com/99designs/go-keychain",
            "github.com/99designs/keyring",
            "github.com/JuulLabs-OSS/cbgo",
            "github.com/cronokirby/saferith",
            "github.com/dvsekhvalnov/jose2go",
            "github.com/go-ble/ble",
            "github.com/golang-jwt/jwt/v5",
            "github.com/mattn/go-colorable",
            "github.com/mattn/go-isatty",
            "github.com/mgutz/ansi",
            "github.com/mgutz/logxi",
            "github.com/mtibben/percent",
            "github.com/pkg/errors",
            "github.com/raff/goble",
            "github.com/sirupsen/logrus",
            "golang.org/x/sys",
            "golang.org/x/term",
            "google.golang.org/protobuf",
        ],
    },
    "linux-amd64": {
        "goos": "linux",
        "goarch": "amd64",
        "cgo_enabled": "0",
        "architecture_level_key": "GOAMD64",
        "architecture_level_value": "v1",
        "macosx_deployment_target": None,
        "runtime_module_paths": [
            "github.com/99designs/keyring",
            "github.com/cronokirby/saferith",
            "github.com/dvsekhvalnov/jose2go",
            "github.com/go-ble/ble",
            "github.com/godbus/dbus",
            "github.com/golang-jwt/jwt/v5",
            "github.com/gsterjov/go-libsecret",
            "github.com/mattn/go-colorable",
            "github.com/mattn/go-isatty",
            "github.com/mgutz/ansi",
            "github.com/mgutz/logxi",
            "github.com/mtibben/percent",
            "github.com/pkg/errors",
            "golang.org/x/sys",
            "golang.org/x/term",
            "google.golang.org/protobuf",
        ],
    },
    "linux-arm64": {
        "goos": "linux",
        "goarch": "arm64",
        "cgo_enabled": "0",
        "architecture_level_key": "GOARM64",
        "architecture_level_value": "v8.0",
        "macosx_deployment_target": None,
        "runtime_module_paths": [
            "github.com/99designs/keyring",
            "github.com/cronokirby/saferith",
            "github.com/dvsekhvalnov/jose2go",
            "github.com/go-ble/ble",
            "github.com/godbus/dbus",
            "github.com/golang-jwt/jwt/v5",
            "github.com/gsterjov/go-libsecret",
            "github.com/mattn/go-colorable",
            "github.com/mattn/go-isatty",
            "github.com/mgutz/ansi",
            "github.com/mgutz/logxi",
            "github.com/mtibben/percent",
            "github.com/pkg/errors",
            "golang.org/x/sys",
            "golang.org/x/term",
            "google.golang.org/protobuf",
        ],
    },
}
MAIN_POLICY = {
    "path": "github.com/teslamotors/vehicle-command",
    "version": "v0.4.1",
    "commit": "49977a18fd68567501d59e16a6c9e4a8b9348544",
    "sum": "h1:J4ne/TNGwgodJLYJDLm/hjoygXyQ/bpqO/EiCaeoobM=",
    "go_mod_sum": "h1:liN6VG6MCc7m02wFaBm2sQT6MYGm/dJua6bG00QSpnA=",
}
OVERLAY_POLICY = {
    "path": "packaging/tesla-command-proxy/0001-go-1.27-runtime-defaults.patch",
    "sha256": "0eb6a95f175ebdde51b18485a7ccd19c5e23aeb009a6f989b4512eb12b843a16",
    "modified_go_mod_sha256": "7459a52ecd7758154ae58d6ec85ac621293aad7d942055f239206ea082e00c3e",
}


class GateError(RuntimeError):
    pass


def json_bytes(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise GateError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def parse_json(data: bytes, label: str) -> Any:
    try:
        return json.loads(data, object_pairs_hook=unique_object)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GateError(f"{label} is not valid UTF-8 JSON") from exc


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def regular_bytes(path: Path, label: str, limit: int, *, allow_empty: bool = False) -> bytes:
    try:
        before = os.lstat(path)
    except OSError as exc:
        raise GateError(f"{label} is missing: {path}") from exc
    if not stat.S_ISREG(before.st_mode):
        raise GateError(f"{label} must be a regular, non-symlink file: {path}")
    if before.st_size > limit:
        raise GateError(f"{label} exceeds {limit} bytes: {path}")
    if not allow_empty and before.st_size == 0:
        raise GateError(f"{label} is empty: {path}")
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise GateError(f"cannot safely open {label}: {path}") from exc
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise GateError(f"{label} changed while opening: {path}")
        chunks: list[bytes] = []
        total = 0
        while True:
            chunk = os.read(descriptor, min(1024 * 1024, limit + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > limit:
                raise GateError(f"{label} exceeds {limit} bytes: {path}")
        after = os.fstat(descriptor)
        if (
            after.st_size != opened.st_size
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
        ):
            raise GateError(f"{label} changed while reading: {path}")
        data = b"".join(chunks)
        if len(data) != opened.st_size:
            raise GateError(f"short read from {label}: {path}")
        return data
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


def run(
    command: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout: int = 180,
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            command,
            cwd=cwd,
            env=env,
            text=True,
            capture_output=True,
            check=True,
            timeout=timeout,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        detail = ""
        if isinstance(exc, subprocess.CalledProcessError):
            detail = (exc.stderr or exc.stdout or "").strip()
        raise GateError(
            f"command failed: {' '.join(command)}{': ' + detail if detail else ''}"
        ) from exc


def require_keys(value: object, expected: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise GateError(f"{label} must be an object")
    keys = set(value)
    if keys != expected:
        missing = sorted(expected - keys)
        extra = sorted(keys - expected)
        raise GateError(f"{label} keys mismatch; missing={missing}, extra={extra}")
    return value


def require_string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise GateError(f"{label} must be a non-empty string")
    return value


def validate_sum(value: object, label: str) -> str:
    text = require_string(value, label)
    if not SUM_RE.fullmatch(text):
        raise GateError(f"{label} is not an h1 module sum")
    return text


def validate_sha(value: object, label: str) -> str:
    text = require_string(value, label)
    if not SHA256_RE.fullmatch(text):
        raise GateError(f"{label} is not a lowercase SHA-256 digest")
    return text


def validate_module_identity(path: object, version: object, label: str) -> tuple[str, str]:
    module_path = require_string(path, f"{label}.path")
    module_version = require_string(version, f"{label}.version")
    parts = PurePosixPath(module_path).parts
    if (
        not MODULE_PATH_RE.fullmatch(module_path)
        or "//" in module_path
        or "\\" in module_path
        or "@" in module_path
        or any(part in ("", ".", "..") for part in parts)
    ):
        raise GateError(f"{label}.path is unsafe")
    if not MODULE_VERSION_RE.fullmatch(module_version) or "/" in module_version:
        raise GateError(f"{label}.version is unsafe")
    return module_path, module_version


def validate_license_fields(item: dict[str, Any], label: str) -> None:
    expression = require_string(item["license_expression"], f"{label}.license_expression")
    if expression not in ALLOWED_LICENSES:
        raise GateError(f"{label} has an unreviewed SPDX license expression: {expression}")
    files = item["license_files"]
    if not isinstance(files, list) or not files or len(files) > 4:
        raise GateError(f"{label}.license_files must contain 1-4 names")
    if len(files) != len(set(files)):
        raise GateError(f"{label}.license_files contains duplicates")
    for name in files:
        if not isinstance(name, str) or not SAFE_LICENSE_RE.fullmatch(name):
            raise GateError(f"{label} has an unsafe license filename")


def validate_lock(lock: object) -> dict[str, Any]:
    root = require_keys(
        lock,
        {
            "schema",
            "package",
            "subjects",
            "build_host",
            "toolchain",
            "targets",
            "overlay",
            "main",
            "modules",
        },
        "lock",
    )
    if root["schema"] != LOCK_SCHEMA or root["package"] != PACKAGE:
        raise GateError("lock schema or package does not match the supported proxy")
    subjects = require_keys(root["subjects"], set(TARGET_POLICIES), "lock.subjects")
    for target, raw_subject in subjects.items():
        subject = require_keys(
            raw_subject, {"name", "sha256", "size"}, f"lock.subjects.{target}"
        )
        if subject["name"] != "tesla-http-proxy":
            raise GateError(f"lock subject name does not match the supported proxy: {target}")
        validate_sha(subject["sha256"], f"lock.subjects.{target}.sha256")
        if not isinstance(subject["size"], int) or subject["size"] <= 0:
            raise GateError(f"lock.subjects.{target}.size must be a positive integer")
    validate_build_host(root["build_host"])
    toolchain = require_keys(root["toolchain"], set(TOOLCHAIN_POLICY), "lock.toolchain")
    if toolchain != TOOLCHAIN_POLICY:
        raise GateError("lock toolchain does not match the reviewed build policy")
    targets = require_keys(root["targets"], set(TARGET_POLICIES), "lock.targets")
    if targets != TARGET_POLICIES:
        raise GateError("lock targets do not match the reviewed build policy")
    overlay = require_keys(root["overlay"], set(OVERLAY_POLICY), "lock.overlay")
    if overlay != OVERLAY_POLICY:
        raise GateError("lock overlay does not match the reviewed source modification")
    validate_sha(overlay["sha256"], "lock.overlay.sha256")
    validate_sha(
        overlay["modified_go_mod_sha256"], "lock.overlay.modified_go_mod_sha256"
    )
    main_keys = {
        "path", "version", "commit", "sum", "go_mod_sum", "zip_sha256",
        "go_mod_sha256", "license_expression", "license_files",
    }
    main = require_keys(root["main"], main_keys, "lock.main")
    for key, expected in MAIN_POLICY.items():
        if main[key] != expected:
            raise GateError(f"lock.main.{key} does not match the reviewed Tesla source")
    validate_module_identity(main["path"], main["version"], "lock.main")
    validate_sum(main["sum"], "lock.main.sum")
    validate_sum(main["go_mod_sum"], "lock.main.go_mod_sum")
    validate_sha(main["zip_sha256"], "lock.main.zip_sha256")
    validate_sha(main["go_mod_sha256"], "lock.main.go_mod_sha256")
    validate_license_fields(main, "lock.main")

    modules = root["modules"]
    if not isinstance(modules, list) or len(modules) != 20:
        raise GateError("lock.modules must contain the 20 reviewed cross-platform dependencies")
    normal_keys = {
        "path", "version", "sum", "effective_path", "effective_version",
        "effective_sum", "go_mod_sum", "zip_sha256", "go_mod_sha256",
        "license_expression", "license_files",
    }
    replacement_keys = normal_keys | {"replacement"}
    paths: list[str] = []
    effective: set[tuple[str, str]] = set()
    for index, candidate in enumerate(modules):
        label = f"lock.modules[{index}]"
        if not isinstance(candidate, dict):
            raise GateError(f"{label} must be an object")
        has_replacement = "replacement" in candidate
        item = require_keys(candidate, replacement_keys if has_replacement else normal_keys, label)
        path, version = validate_module_identity(item["path"], item["version"], label)
        effective_path, effective_version = validate_module_identity(
            item["effective_path"], item["effective_version"], f"{label}.effective"
        )
        paths.append(path)
        if (effective_path, effective_version) in effective:
            raise GateError(f"duplicate effective runtime module: {effective_path}@{effective_version}")
        effective.add((effective_path, effective_version))
        validate_sum(item["effective_sum"], f"{label}.effective_sum")
        validate_sum(item["go_mod_sum"], f"{label}.go_mod_sum")
        validate_sha(item["zip_sha256"], f"{label}.zip_sha256")
        validate_sha(item["go_mod_sha256"], f"{label}.go_mod_sha256")
        validate_license_fields(item, label)
        if has_replacement:
            if item["sum"] is not None:
                raise GateError(f"{label}.sum must be null for a replaced module")
            replacement = require_keys(
                item["replacement"], {"path", "version", "sum", "go_mod_sum"},
                f"{label}.replacement",
            )
            if replacement != {
                "path": effective_path,
                "version": effective_version,
                "sum": item["effective_sum"],
                "go_mod_sum": item["go_mod_sum"],
            }:
                raise GateError(f"{label} replacement and effective source disagree")
        else:
            validate_sum(item["sum"], f"{label}.sum")
            if (
                effective_path != path
                or effective_version != version
                or item["effective_sum"] != item["sum"]
            ):
                raise GateError(f"{label} has an undeclared replacement")
    if paths != sorted(paths) or len(paths) != len(set(paths)):
        raise GateError("lock.modules must be uniquely sorted by declared path")
    return root


def target_policy(lock: dict[str, Any], target: str) -> dict[str, Any]:
    if target not in TARGET_POLICIES:
        raise GateError(f"unsupported command-proxy evidence target: {target}")
    policy = lock["targets"].get(target)
    if policy != TARGET_POLICIES[target]:
        raise GateError(f"command-proxy target policy does not match the lock: {target}")
    return policy


def locked_subject(repo: Path, lock: dict[str, Any], target: str) -> dict[str, Any]:
    target_policy(lock, target)
    subject = lock["subjects"][target]
    if target.startswith("linux-"):
        architecture = target.removeprefix("linux-")
        lock_data = regular_bytes(
            repo / "packaging" / "linux" / "sidecar-sha256.lock",
            "Linux sidecar lock",
            MAX_LOCK_BYTES,
        )
        selected: str | None = None
        seen: set[str] = set()
        try:
            lines = lock_data.decode("ascii").splitlines()
        except UnicodeDecodeError as exc:
            raise GateError("Linux sidecar lock is not ASCII") from exc
        for line in lines:
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            fields = stripped.split()
            if len(fields) != 3 or fields[0] not in {"amd64", "arm64"}:
                raise GateError("Linux sidecar lock has an invalid row")
            if fields[0] in seen:
                raise GateError("Linux sidecar lock has a duplicate architecture")
            seen.add(fields[0])
            validate_sha(fields[1], f"Linux {fields[0]} command-proxy digest")
            validate_sha(fields[2], f"Linux {fields[0]} Fleet digest")
            if fields[0] == architecture:
                selected = fields[1]
        if seen != {"amd64", "arm64"} or selected is None:
            raise GateError("Linux sidecar lock is incomplete")
        if selected != subject["sha256"]:
            raise GateError(f"Linux command-proxy subject disagrees with sidecar lock: {target}")
    return subject


def strict_go_environment() -> tuple[str, dict[str, str], dict[str, str]]:
    go = shutil.which("go")
    if not go or not Path(go).is_file():
        raise GateError("go is required")
    environment = os.environ.copy()
    for key in (
        "CC", "CXX", "CGO_CFLAGS", "CGO_CPPFLAGS", "CGO_CXXFLAGS", "CGO_LDFLAGS",
        "GOEXPERIMENT", "GODEBUG", "GOAMD64", "GOARM64",
    ):
        environment.pop(key, None)
    environment.update({
        "GOENV": "off",
        "GOWORK": "off",
        "GOTOOLCHAIN": "local",
        "GOFLAGS": "",
        "GOPROXY": "off",
        "GOSUMDB": "off",
        "GONOSUMDB": "",
        "GOPRIVATE": "",
    })
    result = run(
        [go, "env", "-json", "GOVERSION", "GOENV", "GOWORK", "GOTOOLCHAIN",
         "GOFLAGS", "GOHOSTOS", "GOHOSTARCH"],
        cwd=Path.cwd(), env=environment,
    )
    values = parse_json(result.stdout.encode(), "go env")
    expected = {
        "GOVERSION": TOOLCHAIN_POLICY["go_version"],
        "GOENV": "",
        "GOWORK": "off",
        "GOTOOLCHAIN": "local",
        "GOFLAGS": "",
        "GOHOSTOS": "darwin",
        "GOHOSTARCH": "arm64",
    }
    if values != expected:
        raise GateError(f"local Go environment does not match the lock: {values}")
    return go, environment, expected


def toolchain_identity(
    go: str, environment: dict[str, str], cwd: Path
) -> dict[str, Any]:
    go_path = Path(go).resolve()
    go_data = regular_bytes(go_path, "Go executable", MAX_TOOL_BYTES)
    goroot = run([go, "env", "GOROOT"], cwd=cwd, env=environment).stdout.strip()
    if not goroot:
        raise GateError("Go reported an empty GOROOT")

    clang_path_text = run(
        ["/usr/bin/xcrun", "--find", "clang"], cwd=cwd, env=environment
    ).stdout.strip()
    clang_path = Path(clang_path_text).resolve()
    clang_data = regular_bytes(clang_path, "Apple clang executable", MAX_TOOL_BYTES)
    clang_version = run(
        [str(clang_path), "--version"], cwd=cwd, env=environment
    ).stdout.splitlines()
    if not clang_version:
        raise GateError("Apple clang reported no version")

    xcode_lines = run(
        ["/usr/bin/xcodebuild", "-version"], cwd=cwd, env=environment
    ).stdout.splitlines()
    if len(xcode_lines) != 2:
        raise GateError("xcodebuild reported an unexpected version identity")
    sdk_path = run(
        ["/usr/bin/xcrun", "--show-sdk-path"], cwd=cwd, env=environment
    ).stdout.strip()
    sdk_version = run(
        ["/usr/bin/xcrun", "--show-sdk-version"], cwd=cwd, env=environment
    ).stdout.strip()
    sdk_build = run(
        ["/usr/bin/xcrun", "--show-sdk-build-version"], cwd=cwd, env=environment
    ).stdout.strip()
    if not sdk_path or not sdk_version or not sdk_build:
        raise GateError("Xcode reported an incomplete SDK identity")

    return {
        "go": {
            "path": str(go_path),
            "sha256": sha256_bytes(go_data),
            "goroot": goroot,
        },
        "compiler": {
            "path": str(clang_path),
            "sha256": sha256_bytes(clang_data),
            "version": clang_version[0],
        },
        "xcode": {
            "version": xcode_lines[0],
            "build": xcode_lines[1],
        },
        "sdk": {
            "path": sdk_path,
            "version": sdk_version,
            "build": sdk_build,
        },
    }


def go_hash(entries: dict[str, bytes]) -> str:
    digest = hashlib.sha256()
    for name in sorted(entries):
        if "\n" in name:
            raise GateError("module zip contains a newline in a filename")
        digest.update(f"{sha256_bytes(entries[name])}  {name}\n".encode())
    return "h1:" + base64.b64encode(digest.digest()).decode()


def go_mod_hash(data: bytes) -> str:
    return go_hash({"go.mod": data})


def inspect_module_zip(
    data: bytes, path: str, version: str, license_files: list[str]
) -> tuple[dict[str, bytes], dict[str, bytes]]:
    prefix = f"{path}@{version}/"
    entries: dict[str, bytes] = {}
    relative_casefold: set[str] = set()
    total = 0
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            infos = archive.infolist()
            if not infos or len(infos) > MAX_ZIP_FILES:
                raise GateError(f"invalid module zip entry count for {path}@{version}")
            for info in infos:
                name = info.filename
                unix_mode = (info.external_attr >> 16) & 0xFFFF
                file_type = stat.S_IFMT(unix_mode)
                if info.is_dir() or file_type not in (0, stat.S_IFREG):
                    raise GateError(f"module zip contains a non-regular entry: {name}")
                if info.flag_bits & 0x1:
                    raise GateError(f"module zip contains an encrypted entry: {name}")
                if info.file_size > MAX_ZIP_FILE_BYTES:
                    raise GateError(f"module zip entry is oversized: {name}")
                total += info.file_size
                if total > MAX_ZIP_EXPANDED_BYTES:
                    raise GateError(f"expanded module zip is oversized: {path}@{version}")
                if not name.startswith(prefix):
                    raise GateError(f"module zip entry escapes its module prefix: {name}")
                relative = name[len(prefix):]
                pure = PurePosixPath(relative)
                if (
                    not relative
                    or relative.startswith("/")
                    or "\\" in relative
                    or "\x00" in relative
                    or any(part in ("", ".", "..") for part in pure.parts)
                ):
                    raise GateError(f"module zip contains an unsafe path: {name}")
                folded = relative.casefold()
                if folded in relative_casefold:
                    raise GateError(f"module zip contains duplicate/case-colliding paths: {relative}")
                relative_casefold.add(folded)
                if name in entries:
                    raise GateError(f"module zip contains a duplicate entry: {name}")
                try:
                    entries[name] = archive.read(info)
                except (OSError, RuntimeError, zipfile.BadZipFile) as exc:
                    raise GateError(f"cannot read module zip entry: {name}") from exc
    except zipfile.BadZipFile as exc:
        raise GateError(f"invalid module zip for {path}@{version}") from exc
    licenses: dict[str, bytes] = {}
    for name in license_files:
        member = prefix + name
        if member not in entries:
            raise GateError(f"locked license file is absent: {path}@{version}/{name}")
        content = entries[member]
        if not content or len(content) > MAX_LICENSE_BYTES:
            raise GateError(f"locked license file is empty or oversized: {path}@{version}/{name}")
        try:
            content.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise GateError(f"locked license is not UTF-8: {path}@{version}/{name}") from exc
        licenses[name] = content
    return entries, licenses


def download_source(
    go: str,
    environment: dict[str, str],
    cwd: Path,
    item: dict[str, Any],
    *,
    main: bool,
) -> dict[str, Any]:
    path = item["path"] if main else item["effective_path"]
    version = item["version"] if main else item["effective_version"]
    expected_sum = item["sum"] if main else item["effective_sum"]
    result = run(
        [go, "mod", "download", "-json", f"{path}@{version}"],
        cwd=cwd, env=environment,
    )
    metadata = parse_json(result.stdout.encode(), f"go mod download {path}@{version}")
    if not isinstance(metadata, dict):
        raise GateError(f"go mod download returned no object for {path}@{version}")
    if metadata.get("Path") != path or metadata.get("Version") != version:
        raise GateError(f"module cache identity mismatch for {path}@{version}")
    if metadata.get("Sum") != expected_sum or metadata.get("GoModSum") != item["go_mod_sum"]:
        raise GateError(f"module cache sums mismatch for {path}@{version}")
    if main:
        origin = metadata.get("Origin")
        if not isinstance(origin, dict) or origin.get("VCS") != "git" or origin.get("Hash") != item["commit"]:
            raise GateError("Tesla module cache origin does not match the locked commit")
    try:
        zip_path = Path(metadata["Zip"])
        mod_path = Path(metadata["GoMod"])
    except (KeyError, TypeError) as exc:
        raise GateError(f"module cache paths are missing for {path}@{version}") from exc
    zip_data = regular_bytes(zip_path, f"module zip {path}@{version}", MAX_ZIP_BYTES)
    mod_data = regular_bytes(mod_path, f"module go.mod {path}@{version}", MAX_MOD_BYTES)
    if sha256_bytes(zip_data) != item["zip_sha256"]:
        raise GateError(f"module zip byte digest mismatch for {path}@{version}")
    if sha256_bytes(mod_data) != item["go_mod_sha256"]:
        raise GateError(f"module go.mod byte digest mismatch for {path}@{version}")
    entries, licenses = inspect_module_zip(zip_data, path, version, item["license_files"])
    if go_hash(entries) != expected_sum:
        raise GateError(f"module zip h1 sum mismatch for {path}@{version}")
    if go_mod_hash(mod_data) != item["go_mod_sum"]:
        raise GateError(f"module go.mod h1 sum mismatch for {path}@{version}")
    return {
        "path": path,
        "version": version,
        "sum": expected_sum,
        "go_mod_sum": item["go_mod_sum"],
        "zip_sha256": item["zip_sha256"],
        "go_mod_sha256": item["go_mod_sha256"],
        "license_expression": item["license_expression"],
        "license_files": list(item["license_files"]),
        "zip": zip_data,
        "mod": mod_data,
        "entries": entries,
        "licenses": licenses,
    }


def escape_module_component(value: str) -> str:
    result: list[str] = []
    for character in value:
        if "A" <= character <= "Z":
            result.extend(("!", character.lower()))
        else:
            result.append(character)
    return "".join(result)


def write_file(path: Path, data: bytes, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    descriptor = os.open(path, flags, mode)
    try:
        view = memoryview(data)
        while view:
            count = os.write(descriptor, view)
            view = view[count:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def remove_work_tree(path: Path) -> None:
    if not path.exists():
        return
    for root, directories, files in os.walk(path, topdown=False, followlinks=False):
        for name in files:
            try:
                os.chmod(Path(root) / name, 0o600, follow_symlinks=False)
            except OSError:
                pass
        for name in directories:
            try:
                os.chmod(Path(root) / name, 0o700, follow_symlinks=False)
            except OSError:
                pass
    try:
        os.chmod(path, 0o700, follow_symlinks=False)
    except OSError:
        pass
    shutil.rmtree(path)


def populate_file_proxy(proxy: Path, sources: list[dict[str, Any]]) -> None:
    for source in sources:
        directory = proxy / escape_module_component(source["path"]) / "@v"
        version = escape_module_component(source["version"])
        write_file(directory / f"{version}.zip", source["zip"])
        write_file(directory / f"{version}.mod", source["mod"])
        info = json.dumps({
            "Version": source["version"],
            "Time": "1970-01-01T00:00:00Z",
        }, separators=(",", ":"), sort_keys=True).encode() + b"\n"
        write_file(directory / f"{version}.info", info)


def extract_main(source: dict[str, Any], destination: Path) -> None:
    prefix = f"{source['path']}@{source['version']}/"
    for name, data in sorted(source["entries"].items()):
        relative = name[len(prefix):]
        write_file(destination / Path(*PurePosixPath(relative).parts), data)


def parse_json_stream(text: str, label: str) -> list[dict[str, Any]]:
    decoder = json.JSONDecoder(object_pairs_hook=unique_object)
    offset = 0
    values: list[dict[str, Any]] = []
    while offset < len(text):
        while offset < len(text) and text[offset].isspace():
            offset += 1
        if offset == len(text):
            break
        try:
            value, offset = decoder.raw_decode(text, offset)
        except json.JSONDecodeError as exc:
            raise GateError(f"{label} returned invalid JSON") from exc
        if not isinstance(value, dict):
            raise GateError(f"{label} returned a non-object")
        values.append(value)
    return values


def normalized_module(module: dict[str, Any]) -> dict[str, Any]:
    result = {
        "path": module.get("Path"),
        "version": module.get("Version"),
        "sum": module.get("Sum"),
    }
    replacement = module.get("Replace")
    if replacement is not None:
        if not isinstance(replacement, dict):
            raise GateError("Go reported an invalid module replacement")
        result["replacement"] = {
            "path": replacement.get("Path"),
            "version": replacement.get("Version"),
            "sum": replacement.get("Sum"),
        }
    return result


def runtime_lock_items(lock: dict[str, Any], target: str) -> list[dict[str, Any]]:
    paths = target_policy(lock, target)["runtime_module_paths"]
    if not isinstance(paths, list) or paths != sorted(paths) or len(paths) != len(set(paths)):
        raise GateError(f"runtime module paths are invalid for target: {target}")
    by_path = {item["path"]: item for item in lock["modules"]}
    if any(path not in by_path for path in paths):
        raise GateError(f"runtime module path is absent from the source lock: {target}")
    return [by_path[path] for path in paths]


def expected_runtime_modules(
    lock: dict[str, Any], target: str
) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    for item in runtime_lock_items(lock, target):
        module: dict[str, Any] = {
            "path": item["path"],
            "version": item["version"],
            "sum": item["sum"],
        }
        if "replacement" in item:
            module["replacement"] = {
                "path": item["effective_path"],
                "version": item["effective_version"],
                "sum": item["effective_sum"],
            }
        result.append(module)
    return result


def expected_build_dependencies(
    lock: dict[str, Any], target: str
) -> list[dict[str, Any]]:
    dependencies: list[dict[str, Any]] = []
    for item in runtime_lock_items(lock, target):
        dependency: dict[str, Any] = {
            "Path": item["path"],
            "Version": item["version"],
        }
        if "replacement" in item:
            dependency["Replace"] = {
                "Path": item["effective_path"],
                "Version": item["effective_version"],
                "Sum": item["effective_sum"],
            }
        else:
            dependency["Sum"] = item["sum"]
        dependencies.append(dependency)
    return dependencies


def expected_build_settings(
    lock: dict[str, Any], target: str
) -> list[dict[str, str]]:
    policy = target_policy(lock, target)
    return [
        {"Key": "-buildmode", "Value": lock["toolchain"]["buildmode"]},
        {"Key": "-compiler", "Value": lock["toolchain"]["compiler"]},
        {"Key": "-trimpath", "Value": "true"},
        {"Key": "CGO_ENABLED", "Value": policy["cgo_enabled"]},
        {"Key": "GOARCH", "Value": policy["goarch"]},
        {"Key": "GOOS", "Value": policy["goos"]},
        {
            "Key": policy["architecture_level_key"],
            "Value": policy["architecture_level_value"],
        },
    ]


def validate_build_info_value(
    info: object, lock: dict[str, Any], target: str
) -> dict[str, Any]:
    value = require_keys(
        info,
        {"GoVersion", "Path", "Main", "Deps", "Settings"},
        "proxy Go build information",
    )
    expected = {
        "GoVersion": lock["toolchain"]["go_version"],
        "Path": PACKAGE,
        "Main": {"Path": lock["main"]["path"], "Version": "(devel)"},
        "Deps": expected_build_dependencies(lock, target),
        "Settings": expected_build_settings(lock, target),
    }
    if value != expected:
        raise GateError("proxy Go build information does not match the exact lock")
    return value


def modules_from_go_list(objects: list[dict[str, Any]], main_path: str) -> list[dict[str, Any]]:
    modules: dict[str, dict[str, Any]] = {}
    for package in objects:
        module = package.get("Module")
        if module is None:
            continue
        if not isinstance(module, dict):
            raise GateError("go list reported an invalid Module object")
        if module.get("Main"):
            if module.get("Path") != main_path:
                raise GateError("go list reported an unexpected main module")
            continue
        normalized = normalized_module(module)
        path = normalized["path"]
        if not isinstance(path, str):
            raise GateError("go list reported a module without a path")
        previous = modules.setdefault(path, normalized)
        if previous != normalized:
            raise GateError(f"go list reported inconsistent module data: {path}")
    return [modules[path] for path in sorted(modules)]


def verify_build_info(
    go: str,
    environment: dict[str, str],
    cwd: Path,
    binary: Path,
    lock: dict[str, Any],
    target: str,
) -> dict[str, Any]:
    result = run([go, "version", "-m", "-json", str(binary)], cwd=cwd, env=environment)
    info = parse_json(result.stdout.encode(), "go version -m")
    return validate_build_info_value(info, lock, target)


def verify_executable(
    binary: Path,
    cwd: Path,
    environment: dict[str, str],
    lock: dict[str, Any],
    target: str,
) -> None:
    policy = target_policy(lock, target)
    file_result = run(["/usr/bin/file", str(binary)], cwd=cwd, env=environment)
    if target == "linux-amd64":
        if "ELF 64-bit LSB executable, x86-64" not in file_result.stdout or "statically linked" not in file_result.stdout:
            raise GateError("proxy is not a static Linux amd64 executable")
        return
    if target == "linux-arm64":
        if "ELF 64-bit LSB executable, ARM aarch64" not in file_result.stdout or "statically linked" not in file_result.stdout:
            raise GateError("proxy is not a static Linux ARM64 executable")
        return
    if "Mach-O 64-bit executable arm64" not in file_result.stdout:
        raise GateError("proxy is not a thin arm64 Mach-O executable")
    codesign = run(["/usr/bin/codesign", "-dv", "--verbose=4", str(binary)], cwd=cwd, env=environment)
    signature = f"{codesign.stdout}\n{codesign.stderr}"
    if "Signature=adhoc" not in signature or "TeamIdentifier=not set" not in signature:
        raise GateError("proxy must be the unsigned linker-signed build, not a release-signed binary")
    otool = run(["/usr/bin/otool", "-l", str(binary)], cwd=cwd, env=environment)
    command = ""
    minimum = None
    for line in otool.stdout.splitlines():
        fields = line.split()
        if len(fields) == 2 and fields[0] == "cmd":
            command = fields[1]
        elif command == "LC_BUILD_VERSION" and len(fields) == 2 and fields[0] == "minos":
            minimum = fields[1]
            break
        elif command == "LC_VERSION_MIN_MACOSX" and len(fields) == 2 and fields[0] == "version":
            minimum = fields[1]
            break
    expected = policy["macosx_deployment_target"]
    if minimum != expected:
        raise GateError(f"proxy minimum macOS version is {minimum}, expected {expected}")


def receipt_toolchain(lock: dict[str, Any], target: str) -> dict[str, Any]:
    target_values = {
        key: value
        for key, value in target_policy(lock, target).items()
        if key != "runtime_module_paths"
    }
    return {
        **lock["toolchain"],
        **target_values,
        "target": target,
    }


def proxy_receipt(subject: dict[str, Any], lock: dict[str, Any], target: str) -> dict[str, Any]:
    policy = target_policy(lock, target)
    formats = {
        "darwin-arm64": "Mach-O 64-bit arm64",
        "linux-amd64": "ELF 64-bit x86-64 static",
        "linux-arm64": "ELF 64-bit ARM aarch64 static",
    }
    return {
        "sha256": subject["sha256"],
        "size": subject["size"],
        "target": target,
        "format": formats[target],
        "signature": (
            "Mach-O linker-signed ad hoc; no TeamIdentifier"
            if target == "darwin-arm64"
            else "not applicable to static ELF"
        ),
        "minimum_macos": policy["macosx_deployment_target"],
    }


def clean_build_environment(
    base: dict[str, str], lock: dict[str, Any], target: str, proxy: Path, work: Path
) -> dict[str, str]:
    policy = target_policy(lock, target)
    environment = base.copy()
    for key in ("GOAMD64", "GOARM64", "MACOSX_DEPLOYMENT_TARGET", "COPYFILE_DISABLE"):
        environment.pop(key, None)
    environment.update({
        "GOENV": "off",
        "GOWORK": "off",
        "GOTOOLCHAIN": "local",
        "GOFLAGS": "",
        "GOPROXY": proxy.as_uri(),
        "GOSUMDB": "off",
        "GONOSUMDB": "",
        "GOPRIVATE": "",
        "GOMODCACHE": str(work / "module-cache"),
        "GOCACHE": str(work / "build-cache"),
        "CGO_ENABLED": policy["cgo_enabled"],
        "GOOS": policy["goos"],
        "GOARCH": policy["goarch"],
        policy["architecture_level_key"]: policy["architecture_level_value"],
    })
    if policy["macosx_deployment_target"] is not None:
        environment["MACOSX_DEPLOYMENT_TARGET"] = policy["macosx_deployment_target"]
        environment["COPYFILE_DISABLE"] = "1"
    return environment


def load_overlay(repo: Path, lock: dict[str, Any]) -> bytes:
    relative = PurePosixPath(lock["overlay"]["path"])
    if relative.is_absolute() or any(part in ("", ".", "..") for part in relative.parts):
        raise GateError("lock overlay path is unsafe")
    overlay = regular_bytes(
        repo.joinpath(*relative.parts), "Tesla command-proxy overlay", MAX_OVERLAY_BYTES
    )
    if sha256_bytes(overlay) != lock["overlay"]["sha256"]:
        raise GateError("Tesla command-proxy overlay does not match the exact lock")
    return overlay


def apply_overlay(
    source_root: Path,
    work: Path,
    overlay: bytes,
    lock: dict[str, Any],
    environment: dict[str, str],
) -> None:
    patch_tool = shutil.which("patch")
    if not patch_tool or not Path(patch_tool).is_file():
        raise GateError("patch is required")
    patch_path = work / "tesla-command-proxy-overlay.patch"
    write_file(patch_path, overlay, mode=0o600)
    run(
        [patch_tool, "--batch", "--forward", "--fuzz=0", "-p1", "-i", str(patch_path)],
        cwd=source_root,
        env=environment,
    )
    modified = regular_bytes(source_root / "go.mod", "modified Tesla go.mod", MAX_MOD_BYTES)
    if sha256_bytes(modified) != lock["overlay"]["modified_go_mod_sha256"]:
        raise GateError("modified Tesla go.mod does not match the exact overlay lock")


def clean_rebuild(
    go: str,
    base_environment: dict[str, str],
    lock: dict[str, Any],
    target: str,
    sources: list[dict[str, Any]],
    overlay: bytes,
    work: Path,
    supplied: bytes,
) -> tuple[str, list[dict[str, Any]]]:
    proxy = work / "file-proxy"
    source_root = work / "source"
    populate_file_proxy(proxy, sources)
    extract_main(sources[0], source_root)
    edit_environment = base_environment.copy()
    edit_environment["GOWORK"] = "off"
    apply_overlay(source_root, work, overlay, lock, edit_environment)
    environment = clean_build_environment(base_environment, lock, target, proxy, work)
    listed = run(
        [go, "list", "-mod=readonly", "-deps", "-json", f"./cmd/tesla-http-proxy"],
        cwd=source_root, env=environment,
    )
    list_objects = parse_json_stream(listed.stdout, "go list -deps")
    modules = modules_from_go_list(list_objects, lock["main"]["path"])
    if modules != expected_runtime_modules(lock, target):
        raise GateError("source runtime module graph does not match the exact dependency lock")
    rebuilt = work / "rebuilt-tesla-http-proxy"
    run(
        [
            go, "build", "-mod=readonly", "-trimpath", "-buildvcs=false",
            f"-ldflags={lock['toolchain']['ldflags']}", "-o", str(rebuilt),
            "./cmd/tesla-http-proxy",
        ],
        cwd=source_root, env=environment,
    )
    rebuilt_bytes = regular_bytes(rebuilt, "rebuilt proxy", MAX_BINARY_BYTES)
    if rebuilt_bytes != supplied:
        raise GateError("deterministic clean rebuild does not match the supplied proxy bytes")
    return sha256_bytes(rebuilt_bytes), modules


def add_tar_member(archive: tarfile.TarFile, name: str, data: bytes) -> None:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = 0o644
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = 0
    archive.addfile(info, io.BytesIO(data))


def source_metadata(source: dict[str, Any]) -> dict[str, Any]:
    return {
        "path": source["path"],
        "version": source["version"],
        "sum": source["sum"],
        "go_mod_sum": source["go_mod_sum"],
        "zip_sha256": source["zip_sha256"],
        "go_mod_sha256": source["go_mod_sha256"],
        "license_expression": source["license_expression"],
        "license_files": source["license_files"],
    }


def source_archive_bytes(
    sources: list[dict[str, Any]], lock_data: bytes, overlay: bytes
) -> bytes:
    tar_buffer = io.BytesIO()
    root = "tesla-http-proxy-go-sources"
    with tarfile.open(fileobj=tar_buffer, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        add_tar_member(archive, f"{root}/tesla-proxy-lock.json", lock_data)
        add_tar_member(
            archive,
            f"{root}/{OVERLAY_POLICY['path']}",
            overlay,
        )
        for index, source in enumerate(sources):
            directory = f"{root}/modules/{index:02d}"
            add_tar_member(archive, f"{directory}/module.zip", source["zip"])
            add_tar_member(archive, f"{directory}/module.mod", source["mod"])
            add_tar_member(archive, f"{directory}/source.json", json_bytes(source_metadata(source)))
    compressed = io.BytesIO()
    with gzip.GzipFile(
        fileobj=compressed, mode="wb", filename="", mtime=0, compresslevel=9
    ) as zipped:
        zipped.write(tar_buffer.getvalue())
    return compressed.getvalue()


def create_source_archive(
    path: Path, sources: list[dict[str, Any]], lock_data: bytes, overlay: bytes
) -> None:
    write_file(path, source_archive_bytes(sources, lock_data, overlay), mode=0o644)


def archived_source_policy(lock: dict[str, Any], index: int) -> dict[str, Any]:
    if index == 0:
        item = lock["main"]
        return {
            "path": item["path"],
            "version": item["version"],
            "sum": item["sum"],
            "go_mod_sum": item["go_mod_sum"],
            "zip_sha256": item["zip_sha256"],
            "go_mod_sha256": item["go_mod_sha256"],
            "license_expression": item["license_expression"],
            "license_files": item["license_files"],
        }
    item = lock["modules"][index - 1]
    return {
        "path": item["effective_path"],
        "version": item["effective_version"],
        "sum": item["effective_sum"],
        "go_mod_sum": item["go_mod_sum"],
        "zip_sha256": item["zip_sha256"],
        "go_mod_sha256": item["go_mod_sha256"],
        "license_expression": item["license_expression"],
        "license_files": item["license_files"],
    }


def parse_source_archive(
    data: bytes, lock_data: bytes, lock: dict[str, Any], overlay: bytes
) -> list[dict[str, Any]]:
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(data), mode="rb") as compressed:
            tar_data = compressed.read(MAX_SOURCE_ARCHIVE_BYTES + 1)
    except (EOFError, OSError, gzip.BadGzipFile) as exc:
        raise GateError("Go source archive is not a valid gzip stream") from exc
    if len(tar_data) > MAX_SOURCE_ARCHIVE_BYTES:
        raise GateError("Go source archive expands beyond the safety limit")

    root = "tesla-http-proxy-go-sources"
    overlay_name = f"{root}/{lock['overlay']['path']}"
    expected_names = {f"{root}/tesla-proxy-lock.json", overlay_name}
    for index in range(1 + len(lock["modules"])):
        directory = f"{root}/modules/{index:02d}"
        expected_names.update({
            f"{directory}/module.zip",
            f"{directory}/module.mod",
            f"{directory}/source.json",
        })
    entries: dict[str, bytes] = {}
    try:
        with tarfile.open(fileobj=io.BytesIO(tar_data), mode="r:") as archive:
            for member in archive:
                if len(entries) >= len(expected_names):
                    raise GateError("Go source archive contains too many members")
                if not member.isfile() or member.issym() or member.islnk():
                    raise GateError(f"Go source archive contains a non-regular member: {member.name}")
                if member.name not in expected_names or member.name in entries:
                    raise GateError(f"Go source archive contains an unsafe or duplicate member: {member.name}")
                if member.size < 0 or member.size > MAX_ZIP_BYTES:
                    raise GateError(f"Go source archive member has an invalid size: {member.name}")
                extracted = archive.extractfile(member)
                if extracted is None:
                    raise GateError(f"Go source archive member cannot be read: {member.name}")
                content = extracted.read(member.size + 1)
                if len(content) != member.size:
                    raise GateError(f"Go source archive member has a short read: {member.name}")
                entries[member.name] = content
    except (OSError, tarfile.TarError) as exc:
        raise GateError("Go source archive is not a valid tar archive") from exc
    if set(entries) != expected_names:
        raise GateError("Go source archive member set does not match the exact lock")
    if entries[f"{root}/tesla-proxy-lock.json"] != lock_data:
        raise GateError("Go source archive embeds a different Tesla proxy lock")
    if entries[overlay_name] != overlay:
        raise GateError("Go source archive embeds a different source overlay")
    if sha256_bytes(entries[overlay_name]) != lock["overlay"]["sha256"]:
        raise GateError("Go source archive overlay does not match the exact lock")

    sources: list[dict[str, Any]] = []
    for index in range(1 + len(lock["modules"])):
        directory = f"{root}/modules/{index:02d}"
        policy = archived_source_policy(lock, index)
        zip_data = entries[f"{directory}/module.zip"]
        mod_data = entries[f"{directory}/module.mod"]
        if sha256_bytes(zip_data) != policy["zip_sha256"]:
            raise GateError(f"Go source archive module {index:02d} zip does not match the lock")
        if sha256_bytes(mod_data) != policy["go_mod_sha256"]:
            raise GateError(f"Go source archive module {index:02d} go.mod does not match the lock")
        module_entries, licenses = inspect_module_zip(
            zip_data, policy["path"], policy["version"], policy["license_files"]
        )
        if go_hash(module_entries) != policy["sum"]:
            raise GateError(f"Go source archive module {index:02d} sum does not match the lock")
        if go_mod_hash(mod_data) != policy["go_mod_sum"]:
            raise GateError(f"Go source archive module {index:02d} go.mod sum does not match the lock")
        source = {
            **policy,
            "zip": zip_data,
            "mod": mod_data,
            "entries": module_entries,
            "licenses": licenses,
        }
        metadata = entries[f"{directory}/source.json"]
        if metadata != json_bytes(source_metadata(source)):
            raise GateError(f"Go source archive module {index:02d} metadata does not match the lock")
        sources.append(source)
    if source_archive_bytes(sources, lock_data, overlay) != data:
        raise GateError("Go source archive is not in the canonical reproducible format")
    return sources


def dependency_inventory(lock: dict[str, Any], lock_sha: str) -> dict[str, Any]:
    dependencies: list[dict[str, Any]] = []
    for item in lock["modules"]:
        entry = {
            "path": item["path"],
            "version": item["version"],
            "sum": item["sum"],
            "source": {
                "path": item["effective_path"],
                "version": item["effective_version"],
                "sum": item["effective_sum"],
                "go_mod_sum": item["go_mod_sum"],
                "zip_sha256": item["zip_sha256"],
                "go_mod_sha256": item["go_mod_sha256"],
                "license_expression": item["license_expression"],
                "license_files": item["license_files"],
            },
        }
        if "replacement" in item:
            entry["replacement"] = item["replacement"]
        dependencies.append(entry)
    return {
        "schema": SCHEMA,
        "lock_sha256": lock_sha,
        "package": PACKAGE,
        "main": {key: value for key, value in lock["main"].items()},
        "runtime_dependency_count": len(dependencies),
        "runtime_dependencies": dependencies,
    }


def spdx_document(lock: dict[str, Any], sources: list[dict[str, Any]], lock_sha: str) -> dict[str, Any]:
    packages: list[dict[str, Any]] = []
    relationships: list[dict[str, str]] = []
    for index, source in enumerate(sources):
        spdx_id = f"SPDXRef-GoModule-{index:02d}"
        packages.append({
            "SPDXID": spdx_id,
            "name": source["path"],
            "versionInfo": source["version"],
            "downloadLocation": (
                "https://proxy.golang.org/"
                + escape_module_component(source["path"])
                + "/@v/"
                + escape_module_component(source["version"])
                + ".zip"
            ),
            "filesAnalyzed": False,
            "checksums": [{"algorithm": "SHA256", "checksumValue": source["zip_sha256"]}],
            "licenseConcluded": source["license_expression"],
            "licenseDeclared": source["license_expression"],
            "copyrightText": "NOASSERTION",
            "externalRefs": [{
                "referenceCategory": "PACKAGE-MANAGER",
                "referenceType": "purl",
                "referenceLocator": (
                    "pkg:golang/" + quote(source["path"], safe="/") + "@"
                    + quote(source["version"], safe="")
                ),
            }],
        })
        if index == 0:
            relationships.append({
                "spdxElementId": "SPDXRef-DOCUMENT",
                "relationshipType": "DESCRIBES",
                "relatedSpdxElement": spdx_id,
            })
        else:
            relationships.append({
                "spdxElementId": "SPDXRef-GoModule-00",
                "relationshipType": "DEPENDS_ON",
                "relatedSpdxElement": spdx_id,
            })
    return {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": "teslatlas-tesla-http-proxy-go-components",
        "documentNamespace": f"https://teslatlas.eu/spdx/go-proxy/{lock_sha}",
        "creationInfo": {
            "created": "1970-01-01T00:00:00Z",
            "creators": ["Tool: teslatlas-go-proxy-evidence"],
        },
        "documentDescribes": ["SPDXRef-GoModule-00"],
        "packages": packages,
        "relationships": relationships,
    }


def notices(sources: list[dict[str, Any]], lock: dict[str, Any]) -> bytes:
    declared_by_source: dict[tuple[str, str], list[str]] = {}
    for item in lock["modules"]:
        key = (item["effective_path"], item["effective_version"])
        declared_by_source.setdefault(key, []).append(f"{item['path']}@{item['version']}")
    lines = [
        "# Tesla HTTP proxy Go third-party notices",
        "",
        "Complete locked license texts for the exact runtime source modules follow.",
        "",
    ]
    for source in sources:
        identity = f"{source['path']}@{source['version']}"
        lines.extend((f"## {identity}", ""))
        if source is not sources[0]:
            declarations = declared_by_source[(source["path"], source["version"])]
            if declarations != [identity]:
                lines.extend(("Declared dependency: " + ", ".join(declarations), ""))
        lines.extend((f"SPDX license expression: {source['license_expression']}", ""))
        for filename in source["license_files"]:
            content = source["licenses"][filename].decode("utf-8")
            lines.extend((f"### {filename}", "", "----- BEGIN EXACT LICENSE TEXT -----"))
            lines.extend(content.rstrip("\n").splitlines())
            lines.extend(("----- END EXACT LICENSE TEXT -----", ""))
    return ("\n".join(lines).rstrip() + "\n").encode()


def write_output(path: Path, data: bytes) -> None:
    write_file(path, data, mode=0o644)


def validate_build_host(value: object) -> dict[str, Any]:
    host = require_keys(value, {"go", "compiler", "xcode", "sdk"}, "Go build host")
    go = require_keys(host["go"], {"path", "sha256", "goroot"}, "Go build host.go")
    compiler = require_keys(
        host["compiler"], {"path", "sha256", "version"}, "Go build host.compiler"
    )
    xcode = require_keys(host["xcode"], {"version", "build"}, "Go build host.xcode")
    sdk = require_keys(host["sdk"], {"path", "version", "build"}, "Go build host.sdk")
    for item, prefix, fields in (
        (go, "Go build host.go", ("path", "goroot")),
        (compiler, "Go build host.compiler", ("path", "version")),
        (xcode, "Go build host.xcode", ("version", "build")),
        (sdk, "Go build host.sdk", ("path", "version", "build")),
    ):
        for field in fields:
            require_string(item[field], f"{prefix}.{field}")
    validate_sha(go["sha256"], "Go build host.go.sha256")
    validate_sha(compiler["sha256"], "Go build host.compiler.sha256")
    return host


def validate_build_receipt(
    data: bytes,
    lock: dict[str, Any],
    lock_sha: str,
    subject: dict[str, Any],
    target: str,
) -> dict[str, Any]:
    receipt = require_keys(
        parse_json(data, "Go build receipt"),
        {
            "schema",
            "package",
            "target",
            "lock_sha256",
            "proxy",
            "toolchain",
            "build_host",
            "source_configuration",
            "strict_go_environment",
            "build_command",
            "build_info",
            "runtime_modules",
            "clean_rebuild_sha256",
            "clean_rebuild_byte_identical",
        },
        "Go build receipt",
    )
    if receipt["schema"] != SCHEMA or receipt["package"] != PACKAGE:
        raise GateError("Go build receipt schema or package is invalid")
    if receipt["target"] != target:
        raise GateError("Go build receipt target does not match its manifest")
    if receipt["lock_sha256"] != lock_sha:
        raise GateError("Go build receipt does not match the exact lock")
    expected_proxy = proxy_receipt(subject, lock, target)
    if receipt["proxy"] != expected_proxy:
        raise GateError("Go build receipt proxy does not match the locked subject")
    if receipt["toolchain"] != receipt_toolchain(lock, target):
        raise GateError("Go build receipt toolchain does not match the exact lock")
    validate_build_host(receipt["build_host"])
    if receipt["build_host"] != lock["build_host"]:
        raise GateError("Go build receipt host identity does not match the exact lock")
    expected_source_configuration = {
        "archived_upstream_source_unchanged": True,
        "overlay_path": lock["overlay"]["path"],
        "overlay_sha256": lock["overlay"]["sha256"],
        "modified_go_mod_sha256": lock["overlay"]["modified_go_mod_sha256"],
        "private_build_copy_go_mod_directive": (
            f"godebug default={lock['toolchain']['godebug_default']}"
        ),
    }
    if receipt["source_configuration"] != expected_source_configuration:
        raise GateError("Go build receipt source configuration is invalid")
    expected_environment = {
        "GOVERSION": lock["toolchain"]["go_version"],
        "GOENV": "",
        "GOWORK": "off",
        "GOTOOLCHAIN": "local",
        "GOFLAGS": "",
        "GOHOSTOS": "darwin",
        "GOHOSTARCH": "arm64",
    }
    if receipt["strict_go_environment"] != expected_environment:
        raise GateError("Go build receipt environment is invalid")
    expected_command = [
        "go",
        "build",
        "-mod=readonly",
        "-trimpath",
        "-buildvcs=false",
        f"-ldflags={lock['toolchain']['ldflags']}",
        "-o",
        "tesla-http-proxy",
        "./cmd/tesla-http-proxy",
    ]
    if receipt["build_command"] != expected_command:
        raise GateError("Go build receipt command is invalid")
    validate_build_info_value(receipt["build_info"], lock, target)
    if receipt["runtime_modules"] != expected_runtime_modules(lock, target):
        raise GateError("Go build receipt runtime modules do not match the exact lock")
    if (
        receipt["clean_rebuild_sha256"] != subject["sha256"]
        or receipt["clean_rebuild_byte_identical"] is not True
    ):
        raise GateError("Go build receipt does not prove the locked reproducible subject")
    return receipt


def verify_published_evidence(repo: Path, directory: Path) -> dict[str, Any]:
    expected_names = {
        "GO_THIRD_PARTY_NOTICES.generated.md",
        "go-build-receipt.json",
        "go-component-manifest.json",
        "go-dependency-inventory.json",
        "go-sbom.spdx.json",
        "tesla-http-proxy.unsigned",
        "tesla-http-proxy-go-sources.tar.gz",
    }
    actual_names = {path.name for path in directory.iterdir()}
    if actual_names != expected_names:
        raise GateError(
            "published evidence files mismatch; "
            f"missing={sorted(expected_names - actual_names)}, "
            f"extra={sorted(actual_names - expected_names)}"
        )
    lock_data = regular_bytes(
        repo / "scripts" / "tesla-proxy-lock.json", "Tesla proxy lock", MAX_LOCK_BYTES
    )
    lock = validate_lock(parse_json(lock_data, "Tesla proxy lock"))
    lock_sha = sha256_bytes(lock_data)
    overlay = load_overlay(repo, lock)
    manifest_data = regular_bytes(
        directory / "go-component-manifest.json",
        "Go component manifest",
        MAX_LOCK_BYTES,
    )
    manifest = require_keys(
        parse_json(manifest_data, "Go component manifest"),
        {
            "schema",
            "target",
            "subject",
            "lock_sha256",
            "source_module_count",
            "runtime_dependency_count",
            "clean_rebuild_byte_identical",
            "components",
            "component_set_sha256",
        },
        "Go component manifest",
    )
    if manifest["schema"] != SCHEMA or manifest["clean_rebuild_byte_identical"] is not True:
        raise GateError("Go component manifest policy is not satisfied")
    target = require_string(manifest["target"], "Go component target")
    target_policy(lock, target)
    subject = require_keys(
        manifest["subject"], {"name", "sha256", "size"}, "Go component subject"
    )
    if subject != locked_subject(repo, lock, target):
        raise GateError("Go component manifest subject does not match the locked proxy")
    if manifest["lock_sha256"] != lock_sha:
        raise GateError("Go component manifest does not match this source lock")
    if (
        manifest["source_module_count"] != 21
        or manifest["runtime_dependency_count"] != len(runtime_lock_items(lock, target))
    ):
        raise GateError("Go component manifest dependency counts are unexpected")

    components = manifest["components"]
    if not isinstance(components, list) or len(components) != 6:
        raise GateError("Go component manifest must contain six evidence components")
    component_names = expected_names - {"go-component-manifest.json"}
    seen: set[str] = set()
    normalized: list[dict[str, Any]] = []
    component_data: dict[str, bytes] = {}
    for index, raw in enumerate(components):
        item = require_keys(raw, {"path", "sha256", "size"}, f"Go component {index}")
        name = require_string(item["path"], f"Go component {index}.path")
        if name not in component_names or name in seen:
            raise GateError("Go component manifest contains an unsafe or duplicate path")
        digest = validate_sha(item["sha256"], f"Go component {index}.sha256")
        size = item["size"]
        if not isinstance(size, int) or size <= 0:
            raise GateError(f"Go component {index}.size is invalid")
        data = regular_bytes(
            directory / name, f"Go evidence component {name}", MAX_ZIP_EXPANDED_BYTES
        )
        if len(data) != size or sha256_bytes(data) != digest:
            raise GateError(f"Go evidence component does not match its manifest: {name}")
        seen.add(name)
        component_data[name] = data
        normalized.append({"path": name, "sha256": digest, "size": size})
    if seen != component_names:
        raise GateError("Go component manifest is incomplete")
    component_set = "".join(
        f"{item['sha256']}  {item['path']}\n"
        for item in sorted(normalized, key=lambda item: item["path"])
    ).encode()
    if manifest["component_set_sha256"] != sha256_bytes(component_set):
        raise GateError("Go component set digest is invalid")

    sources = parse_source_archive(
        component_data["tesla-http-proxy-go-sources.tar.gz"], lock_data, lock, overlay
    )
    expected_inventory = json_bytes(dependency_inventory(lock, lock_sha))
    if component_data["go-dependency-inventory.json"] != expected_inventory:
        raise GateError("Go dependency inventory does not match the exact lock")
    expected_sbom = json_bytes(spdx_document(lock, sources, lock_sha))
    if component_data["go-sbom.spdx.json"] != expected_sbom:
        raise GateError("Go SPDX SBOM does not match the locked source archive")
    expected_notices = notices(sources, lock)
    if component_data["GO_THIRD_PARTY_NOTICES.generated.md"] != expected_notices:
        raise GateError("Go third-party notices do not match the locked source licenses")
    validate_build_receipt(
        component_data["go-build-receipt.json"], lock, lock_sha, subject, target
    )
    unsigned_proxy = component_data["tesla-http-proxy.unsigned"]
    if len(unsigned_proxy) != subject["size"] or sha256_bytes(unsigned_proxy) != subject["sha256"]:
        raise GateError("unsigned Tesla proxy component does not match the locked subject")
    return manifest


def evidence(
    repo: Path,
    proxy_path: Path,
    output: Path,
    target: str,
) -> None:
    lock_path = repo / "scripts" / "tesla-proxy-lock.json"
    lock_data = regular_bytes(lock_path, "Tesla proxy lock", MAX_LOCK_BYTES)
    lock = validate_lock(parse_json(lock_data, "Tesla proxy lock"))
    lock_sha = sha256_bytes(lock_data)
    overlay = load_overlay(repo, lock)
    subject = locked_subject(repo, lock, target)
    proxy_data = regular_bytes(proxy_path, "unsigned Tesla proxy", MAX_BINARY_BYTES)
    proxy_sha = sha256_bytes(proxy_data)
    if {
        "name": "tesla-http-proxy",
        "sha256": proxy_sha,
        "size": len(proxy_data),
    } != subject:
        raise GateError("unsigned Tesla proxy does not match the reviewed locked subject")
    go, base_environment, go_environment = strict_go_environment()

    temporary = Path(tempfile.mkdtemp(prefix="teslatlas-go-evidence.", dir=str(output.parent)))
    stage = temporary / "published"
    stage.mkdir(mode=0o700)
    try:
        staged_proxy = temporary / "supplied-tesla-http-proxy"
        write_file(staged_proxy, proxy_data, mode=0o700)
        build_host = toolchain_identity(go, base_environment, temporary)
        if build_host != lock["build_host"]:
            raise GateError("local Go/Xcode build host does not match the reviewed lock")
        build_info = verify_build_info(
            go, base_environment, temporary, staged_proxy, lock, target
        )
        verify_executable(staged_proxy, temporary, base_environment, lock, target)

        sources = [download_source(go, base_environment, repo, lock["main"], main=True)]
        sources.extend(
            download_source(go, base_environment, repo, item, main=False)
            for item in lock["modules"]
        )
        rebuilt_sha, runtime_modules = clean_rebuild(
            go, base_environment, lock, target, sources, overlay, temporary, proxy_data
        )

        source_archive = stage / "tesla-http-proxy-go-sources.tar.gz"
        create_source_archive(source_archive, sources, lock_data, overlay)
        write_output(
            stage / "go-dependency-inventory.json",
            json_bytes(dependency_inventory(lock, lock_sha)),
        )
        write_output(
            stage / "go-sbom.spdx.json",
            json_bytes(spdx_document(lock, sources, lock_sha)),
        )
        write_output(stage / "GO_THIRD_PARTY_NOTICES.generated.md", notices(sources, lock))
        write_output(stage / "tesla-http-proxy.unsigned", proxy_data)
        receipt = {
            "schema": SCHEMA,
            "package": PACKAGE,
            "target": target,
            "lock_sha256": lock_sha,
            "proxy": proxy_receipt(subject, lock, target),
            "toolchain": receipt_toolchain(lock, target),
            "build_host": build_host,
            "source_configuration": {
                "archived_upstream_source_unchanged": True,
                "overlay_path": lock["overlay"]["path"],
                "overlay_sha256": lock["overlay"]["sha256"],
                "modified_go_mod_sha256": lock["overlay"]["modified_go_mod_sha256"],
                "private_build_copy_go_mod_directive": (
                    f"godebug default={lock['toolchain']['godebug_default']}"
                ),
            },
            "strict_go_environment": go_environment,
            "build_command": [
                "go", "build", "-mod=readonly", "-trimpath", "-buildvcs=false",
                f"-ldflags={lock['toolchain']['ldflags']}", "-o", "tesla-http-proxy",
                "./cmd/tesla-http-proxy",
            ],
            "build_info": build_info,
            "runtime_modules": runtime_modules,
            "clean_rebuild_sha256": rebuilt_sha,
            "clean_rebuild_byte_identical": True,
        }
        write_output(stage / "go-build-receipt.json", json_bytes(receipt))

        components: list[dict[str, Any]] = []
        for component in sorted(stage.iterdir(), key=lambda path: path.name):
            data = regular_bytes(component, "evidence component", MAX_ZIP_EXPANDED_BYTES)
            components.append({
                "path": component.name,
                "sha256": sha256_bytes(data),
                "size": len(data),
            })
        component_set = "".join(
            f"{item['sha256']}  {item['path']}\n" for item in components
        ).encode()
        manifest = {
            "schema": SCHEMA,
            "target": target,
            "subject": {
                "name": "tesla-http-proxy",
                "sha256": proxy_sha,
                "size": len(proxy_data),
            },
            "lock_sha256": lock_sha,
            "source_module_count": len(sources),
            "runtime_dependency_count": len(runtime_lock_items(lock, target)),
            "clean_rebuild_byte_identical": True,
            "components": components,
            "component_set_sha256": sha256_bytes(component_set),
        }
        write_output(stage / "go-component-manifest.json", json_bytes(manifest))
        stage.chmod(0o755)

        if os.path.lexists(output):
            raise GateError(f"output directory already exists: {output}")
        stage.rename(output)
        remove_work_tree(temporary)
    except BaseException:
        try:
            remove_work_tree(temporary)
        except OSError:
            pass
        raise


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate deterministic Tesla Go proxy source and dependency evidence."
    )
    parser.add_argument("--repo", required=True, help="Teslatlas Hub repository root")
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument("--proxy-binary", help="unsigned linker-built proxy")
    inputs.add_argument("--verify-dir", help="verify an existing evidence directory")
    parser.add_argument("--output-dir", help="new evidence directory")
    parser.add_argument(
        "--target",
        choices=tuple(TARGET_POLICIES),
        default="darwin-arm64",
        help="binary target for evidence generation (default: darwin-arm64)",
    )
    return parser.parse_args()


def main() -> int:
    os.umask(0o077)
    args = parse_args()
    try:
        repo = checked_directory(Path(args.repo), "repository")
        if args.verify_dir:
            if args.output_dir:
                raise GateError("--output-dir cannot be used with --verify-dir")
            if args.target != "darwin-arm64":
                raise GateError("--target is inferred from evidence when using --verify-dir")
            directory = checked_directory(Path(args.verify_dir), "Go evidence directory")
            verify_published_evidence(repo, directory)
            print(directory)
            return 0
        if not args.output_dir:
            raise GateError("--output-dir is required with --proxy-binary")
        proxy = Path(os.path.abspath(args.proxy_binary))
        output = Path(os.path.abspath(args.output_dir))
        parent = checked_directory(output.parent, "output parent")
        output = parent / output.name
        if os.path.lexists(output):
            raise GateError(f"output directory already exists: {output}")
        evidence(repo, proxy, output, args.target)
        print(output)
        return 0
    except GateError as exc:
        print(f"go-proxy-evidence: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
