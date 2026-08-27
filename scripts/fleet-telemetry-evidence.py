#!/usr/bin/env python3
"""Bind a built Fleet Telemetry receiver to the reviewed bridge lock."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import sys
import tempfile


SCHEMA = "teslatlas.fleet-telemetry-evidence/v1"
FILES = (
    "fleet-telemetry-bridge-lock.json",
    "fleet-telemetry-component-manifest.json",
    "fleet-telemetry.unsigned",
)
MAX_BINARY_BYTES = 128 * 1024 * 1024


class GateError(RuntimeError):
    pass


def regular_bytes(path: Path, label: str, maximum: int) -> bytes:
    try:
        before = os.lstat(path)
    except OSError as exc:
        raise GateError(f"{label} is missing: {path}") from exc
    if not stat.S_ISREG(before.st_mode) or before.st_size <= 0 or before.st_size > maximum:
        raise GateError(f"{label} must be a bounded regular non-symlink file: {path}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise GateError(f"cannot safely read {label}: {path}") from exc
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise GateError(f"{label} changed while opening: {path}")
        data = bytearray()
        while len(data) <= maximum:
            block = os.read(descriptor, min(1024 * 1024, maximum + 1 - len(data)))
            if not block:
                break
            data.extend(block)
        after = os.fstat(descriptor)
        if len(data) != opened.st_size or after.st_mtime_ns != opened.st_mtime_ns:
            raise GateError(f"{label} changed while reading: {path}")
        return bytes(data)
    finally:
        os.close(descriptor)


def read_json(path: Path, label: str) -> dict:
    try:
        value = json.loads(regular_bytes(path, label, 128 * 1024))
    except json.JSONDecodeError as exc:
        raise GateError(f"{label} is not valid JSON") from exc
    if not isinstance(value, dict):
        raise GateError(f"{label} must be an object")
    return value


def validate_lock(lock: dict) -> None:
    try:
        upstream, overlay, bridge, toolchain = (
            lock["upstream"], lock["overlay"], lock["bridge"], lock["toolchain"]
        )
    except KeyError as exc:
        raise GateError("Fleet Telemetry bridge lock is incomplete") from exc
    if lock.get("schema") != 1 or lock.get("targets") != [
        "darwin-arm64", "darwin-amd64", "linux-arm64", "linux-amd64"
    ]:
        raise GateError("Fleet Telemetry bridge lock is not the reviewed target set")
    if not isinstance(upstream, dict) or upstream.get("repository") != "https://github.com/teslamotors/fleet-telemetry" \
            or upstream.get("version") != "v0.9.4" or len(upstream.get("commit", "")) != 40:
        raise GateError("Fleet Telemetry bridge lock has an unreviewed upstream")
    if not isinstance(overlay, dict) or overlay.get("patch") != "0001-teslatlas-http-dispatcher.patch" \
            or not isinstance(overlay.get("patch_sha256"), str) or len(overlay["patch_sha256"]) != 64:
        raise GateError("Fleet Telemetry bridge lock has an unreviewed overlay")
    if not isinstance(bridge, dict) or bridge.get("endpoint") != "http://127.0.0.1:8080/v1/internal/fleet-telemetry":
        raise GateError("Fleet Telemetry bridge lock has an unsafe dispatcher")
    if not isinstance(toolchain, dict) or toolchain != {"go_version": "go1.27.0", "cgo_enabled": False}:
        raise GateError("Fleet Telemetry bridge lock has an unreviewed toolchain")


def manifest(lock_bytes: bytes, binary: bytes) -> dict:
    return {
        "schema": SCHEMA,
        "subject": {
            "name": "fleet-telemetry",
            "sha256": hashlib.sha256(binary).hexdigest(),
            "size": len(binary),
            "target": "darwin-arm64",
        },
        "bridge_lock_sha256": hashlib.sha256(lock_bytes).hexdigest(),
    }


def validate_directory(directory: Path, expected_lock_path: Path) -> None:
    if not directory.is_dir() or directory.is_symlink():
        raise GateError("Fleet Telemetry evidence must be a real directory")
    if {path.name for path in directory.iterdir()} != set(FILES):
        raise GateError("Fleet Telemetry evidence file set is invalid")
    lock_path = directory / "fleet-telemetry-bridge-lock.json"
    binary_path = directory / "fleet-telemetry.unsigned"
    manifest_path = directory / "fleet-telemetry-component-manifest.json"
    lock_bytes = regular_bytes(lock_path, "Fleet Telemetry bridge lock", 128 * 1024)
    validate_lock(read_json(lock_path, "Fleet Telemetry bridge lock"))
    expected_lock_bytes = regular_bytes(
        expected_lock_path, "reviewed Fleet Telemetry bridge lock", 128 * 1024
    )
    validate_lock(read_json(expected_lock_path, "reviewed Fleet Telemetry bridge lock"))
    if lock_bytes != expected_lock_bytes:
        raise GateError("Fleet Telemetry evidence does not match the reviewed repository lock")
    binary = regular_bytes(binary_path, "Fleet Telemetry receiver", MAX_BINARY_BYTES)
    value = read_json(manifest_path, "Fleet Telemetry component manifest")
    if value != manifest(lock_bytes, binary):
        raise GateError("Fleet Telemetry component manifest does not bind the receiver and lock")


def write_file(path: Path, data: bytes) -> None:
    with path.open("xb") as target:
        target.write(data)
        target.flush()
        os.fsync(target.fileno())
    os.chmod(path, 0o600)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--receiver-binary", type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--verify-dir", type=Path)
    args = parser.parse_args()
    repo = args.repo.resolve()
    lock_path = repo / "packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json"
    if args.verify_dir is not None:
        if args.receiver_binary is not None or args.output_dir is not None:
            raise GateError("--verify-dir cannot be combined with generation inputs")
        validate_directory(args.verify_dir.resolve(), lock_path)
        return 0
    if args.receiver_binary is None or args.output_dir is None:
        raise GateError("--receiver-binary and --output-dir are required")
    output = args.output_dir.resolve()
    if os.path.lexists(output):
        raise GateError("Fleet Telemetry evidence output already exists")
    if not output.parent.is_dir():
        raise GateError("Fleet Telemetry evidence output parent is missing")
    lock_bytes = regular_bytes(lock_path, "Fleet Telemetry bridge lock", 128 * 1024)
    validate_lock(read_json(lock_path, "Fleet Telemetry bridge lock"))
    binary = regular_bytes(args.receiver_binary.resolve(), "Fleet Telemetry receiver", MAX_BINARY_BYTES)
    stage = Path(tempfile.mkdtemp(prefix="teslatlas-fleet-telemetry-evidence-", dir=output.parent))
    try:
        write_file(stage / "fleet-telemetry-bridge-lock.json", lock_bytes)
        write_file(stage / "fleet-telemetry.unsigned", binary)
        write_file(
            stage / "fleet-telemetry-component-manifest.json",
            (json.dumps(manifest(lock_bytes, binary), sort_keys=True, indent=2) + "\n").encode(),
        )
        validate_directory(stage, lock_path)
        stage.rename(output)
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as error:
        print(f"fleet-telemetry-evidence: {error}", file=sys.stderr)
        raise SystemExit(1)
