#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Generate or verify the exact dependency legal payload embedded in packages."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
from types import ModuleType


SCHEMA = "teslatlas.dependency-legal-bundle/v1"
BASE_FILES = (
    "RUST_THIRD_PARTY_NOTICES.generated.md",
    "rust-dependency-inventory.json",
    "rust-sbom.spdx.json",
)
SIDECAR_FILES = (
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
MANIFEST = "legal-bundle-manifest.json"
MAX_FILE_BYTES = 16 * 1024 * 1024


class GateError(RuntimeError):
    pass


def json_bytes(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def regular_bytes(path: Path, label: str, maximum: int = MAX_FILE_BYTES) -> bytes:
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
        raise GateError(f"cannot safely open {label}: {path}") from exc
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
            or after.st_nlink != 1
        ):
            raise GateError(f"{label} changed while reading: {path}")
        return bytes(data)
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


def load_release_module(repo: Path) -> ModuleType:
    helper = repo / "scripts" / "release-evidence.py"
    regular_bytes(helper, "release evidence helper", 2 * 1024 * 1024)
    spec = importlib.util.spec_from_file_location("teslatlas_release_evidence", helper)
    if spec is None or spec.loader is None:
        raise GateError("cannot load release evidence helper")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    previous_bytecode_setting = sys.dont_write_bytecode
    sys.dont_write_bytecode = True
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        raise GateError("cannot load release evidence helper") from exc
    finally:
        sys.dont_write_bytecode = previous_bytecode_setting
    return module


def run_verifier(repo: Path, helper_name: str, directory: Path) -> None:
    helper = repo / "scripts" / helper_name
    regular_bytes(helper, f"{helper_name} verifier", 2 * 1024 * 1024)
    try:
        subprocess.run(
            [sys.executable, str(helper), "--repo", str(repo), "--verify-dir", str(directory)],
            cwd=repo,
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        detail = exc.stderr.strip() if isinstance(exc, subprocess.CalledProcessError) else ""
        raise GateError(
            f"{helper_name} rejected its evidence" + (f": {detail}" if detail else "")
        ) from exc


def rust_components(repo: Path) -> dict[str, bytes]:
    release = load_release_module(repo)
    try:
        metadata = release.cargo_metadata(repo)
        sbom, inventory, notices = release.sbom_and_notices(metadata, repo)
    except Exception as exc:
        raise GateError(f"cannot generate locked Rust legal material: {exc}") from exc
    return {
        "RUST_THIRD_PARTY_NOTICES.generated.md": notices.encode(),
        "rust-dependency-inventory.json": json_bytes(inventory),
        "rust-sbom.spdx.json": json_bytes(sbom),
    }


def sidecar_components(
    repo: Path, go_evidence: Path | None, fleet_evidence: Path | None
) -> dict[str, bytes]:
    if (go_evidence is None) != (fleet_evidence is None):
        raise GateError("Go and Fleet Telemetry evidence must be supplied together")
    if go_evidence is None or fleet_evidence is None:
        return {}
    go = real_directory(go_evidence, "Go proxy evidence")
    fleet = real_directory(fleet_evidence, "Fleet Telemetry evidence")
    run_verifier(repo, "go-proxy-evidence.py", go)
    run_verifier(repo, "fleet-telemetry-evidence.py", fleet)
    go_names = (
        "GO_THIRD_PARTY_NOTICES.generated.md",
        "go-dependency-inventory.json",
        "go-sbom.spdx.json",
    )
    values = {
        name: regular_bytes(go / name, f"Go evidence {name}") for name in go_names
    }
    fleet_names = (
        "FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md",
        "fleet-telemetry-bridge-lock.json",
        "fleet-telemetry-dependency-inventory.json",
        "fleet-telemetry-legal-lock.json",
        "fleet-telemetry-license-material.tar.gz",
        "fleet-telemetry-sbom.spdx.json",
    )
    values.update(
        {name: regular_bytes(fleet / name, f"Fleet Telemetry evidence {name}")
         for name in fleet_names}
    )
    return values


def expected_components(
    repo: Path, go_evidence: Path | None, fleet_evidence: Path | None
) -> dict[str, bytes]:
    values = rust_components(repo)
    values.update(sidecar_components(repo, go_evidence, fleet_evidence))
    return values


def manifest_bytes(repo: Path, components: dict[str, bytes]) -> bytes:
    lock = regular_bytes(repo / "Cargo.lock", "Cargo lockfile")
    records = [
        {"path": name, "sha256": sha256_bytes(data), "size": len(data)}
        for name, data in sorted(components.items())
    ]
    value = {
        "schema": SCHEMA,
        "cargo_lock_sha256": sha256_bytes(lock),
        "contains_sidecar_material": bool(set(components) & set(SIDECAR_FILES)),
        "components": records,
    }
    return json_bytes(value)


def validate_directory(
    repo: Path,
    directory: Path,
    go_evidence: Path | None,
    fleet_evidence: Path | None,
) -> None:
    directory = real_directory(directory, "dependency legal bundle")
    expected = expected_components(repo, go_evidence, fleet_evidence)
    expected_set = set(expected) | {MANIFEST}
    try:
        actual_set = {entry.name for entry in directory.iterdir()}
    except OSError as exc:
        raise GateError("dependency legal bundle cannot be listed") from exc
    if actual_set != expected_set:
        raise GateError("dependency legal bundle file set is incomplete or unexpected")
    for name, data in sorted(expected.items()):
        if regular_bytes(directory / name, f"dependency legal component {name}") != data:
            raise GateError(f"dependency legal component mismatch: {name}")
    if regular_bytes(directory / MANIFEST, "dependency legal manifest") != manifest_bytes(
        repo, expected
    ):
        raise GateError("dependency legal bundle manifest mismatch")


def write_new(path: Path, data: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, 0o600)
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


def generate(
    repo: Path,
    output: Path,
    go_evidence: Path | None,
    fleet_evidence: Path | None,
) -> None:
    if os.path.lexists(output):
        raise GateError(f"dependency legal bundle output already exists: {output}")
    parent = real_directory(output.parent, "dependency legal bundle output parent")
    output = parent / output.name
    components = expected_components(repo, go_evidence, fleet_evidence)
    stage = Path(tempfile.mkdtemp(prefix=".teslatlas-legal-bundle.", dir=parent))
    try:
        for name, data in sorted(components.items()):
            write_new(stage / name, data)
        write_new(stage / MANIFEST, manifest_bytes(repo, components))
        validate_directory(repo, stage, go_evidence, fleet_evidence)
        os.chmod(stage, 0o755)
        stage.rename(output)
    except BaseException:
        shutil.rmtree(stage, ignore_errors=True)
        raise


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--output-dir", type=Path)
    mode.add_argument("--verify-dir", type=Path)
    parser.add_argument("--go-proxy-evidence", type=Path)
    parser.add_argument("--fleet-telemetry-evidence", type=Path)
    return parser.parse_args()


def main() -> int:
    os.umask(0o077)
    args = parse_args()
    repo = real_directory(args.repo.resolve(), "repository")
    if args.output_dir is not None:
        generate(
            repo,
            args.output_dir.resolve(),
            args.go_proxy_evidence.resolve() if args.go_proxy_evidence else None,
            args.fleet_telemetry_evidence.resolve() if args.fleet_telemetry_evidence else None,
        )
        print(args.output_dir.resolve())
    else:
        validate_directory(
            repo,
            args.verify_dir.resolve(),
            args.go_proxy_evidence.resolve() if args.go_proxy_evidence else None,
            args.fleet_telemetry_evidence.resolve() if args.fleet_telemetry_evidence else None,
        )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as exc:
        print(f"legal-bundle: {exc}", file=sys.stderr)
        raise SystemExit(1)
