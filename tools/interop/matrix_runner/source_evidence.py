#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Finite, external source and build evidence for compatibility cohorts.

The existing runner owns the historical Git identity algorithm.  Callers pass
that observer to :func:`capture_repository_snapshot`; this module never
normalizes or projects those identities.  Its additional manifests retain the
actual patch, changed/untracked bytes, admitted build inputs, exports, commands,
toolchain evidence, and output member inventories.
"""

from __future__ import annotations

import hashlib
import datetime
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
from typing import Any, Callable, Mapping, Sequence
import zipfile


MAX_JSON_BYTES = 32 * 1024 * 1024
MAX_MEMBER_BYTES = 2 * 1024 * 1024 * 1024
MAX_MEMBERS = 250_000
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
MODE_RE = re.compile(r"^0[0-7]{3}$")
HEAD_RE = re.compile(r"^[0-9a-f]{40,64}$")
HERE = Path(__file__).resolve().parent
WORKSPACE_ROOT = HERE.parents[3]
KNOWN_SOURCE_NAMES = (
    "hub", "teslatlas-protocol", "teslatlas-sdk-typescript",
    "teslatlas-sdk-swift",
    "teslatlas-home-assistant", "teslatlas-edge", "app",
)


class SourceEvidenceError(RuntimeError):
    """Evidence cannot be captured or validated without ambiguity."""


def _sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _identity(metadata: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        metadata.st_dev, metadata.st_ino, metadata.st_mode,
        metadata.st_size, metadata.st_mtime_ns,
    )


def _strict_json(payload: bytes, label: str) -> Any:
    def unique(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise SourceEvidenceError(f"duplicate JSON member in {label}: {key}")
            result[key] = value
        return result

    def reject(_value: str) -> None:
        raise SourceEvidenceError(f"non-finite JSON number in {label}")

    def finite(value: str) -> float:
        parsed = float(value)
        if not math.isfinite(parsed):
            raise SourceEvidenceError(f"non-finite JSON number in {label}")
        return parsed

    try:
        return json.loads(
            payload, object_pairs_hook=unique, parse_constant=reject,
            parse_float=finite,
        )
    except SourceEvidenceError:
        raise
    except (UnicodeError, json.JSONDecodeError) as error:
        raise SourceEvidenceError(f"invalid JSON in {label}") from error


def _canonical(path: Path | str, label: str, *, exists: bool = True) -> Path:
    raw = str(path)
    candidate = Path(raw)
    if not candidate.is_absolute() or os.path.normpath(raw) != raw:
        raise SourceEvidenceError(f"{label} path alias is invalid")
    if exists:
        try:
            resolved = candidate.resolve(strict=True)
        except OSError as error:
            raise SourceEvidenceError(f"{label} is missing") from error
        if str(resolved) != raw:
            raise SourceEvidenceError(f"{label} path alias is invalid")
        return resolved
    missing: list[str] = []
    cursor = candidate
    while not os.path.lexists(cursor):
        missing.append(cursor.name)
        if cursor.parent == cursor:
            raise SourceEvidenceError(f"{label} parent is missing")
        cursor = cursor.parent
    try:
        resolved = cursor.resolve(strict=True)
    except OSError as error:
        raise SourceEvidenceError(f"{label} parent is unsafe") from error
    rebuilt = resolved
    for member in reversed(missing):
        rebuilt /= member
    if str(rebuilt) != raw:
        raise SourceEvidenceError(f"{label} path alias is invalid")
    return rebuilt


def _within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _known_source_roots(extra: Sequence[Path] = ()) -> tuple[Path, ...]:
    roots: list[Path] = []
    for raw in [*(WORKSPACE_ROOT / name for name in KNOWN_SOURCE_NAMES), *extra]:
        if os.path.lexists(raw):
            try:
                canonical = _canonical(raw, "source root")
            except SourceEvidenceError:
                continue
            if canonical not in roots:
                roots.append(canonical)
    return tuple(roots)


def _outside_sources(path: Path | str, roots: Sequence[Path], label: str) -> Path:
    canonical = _canonical(path, label, exists=os.path.lexists(path))
    if any(_within(canonical, root) for root in roots):
        raise SourceEvidenceError(f"{label} must be outside source roots")
    return canonical


def _read_regular(path: Path | str, label: str, limit: int = MAX_MEMBER_BYTES) -> bytes:
    canonical = _canonical(path, label)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    try:
        descriptor = os.open(canonical, flags)
    except OSError as error:
        raise SourceEvidenceError(f"{label} is not a safe regular file") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise SourceEvidenceError(f"{label} is not a regular file")
        if before.st_size > limit:
            raise SourceEvidenceError(f"{label} exceeds byte limit")
        chunks: list[bytes] = []
        remaining = before.st_size + 1
        while remaining:
            chunk = os.read(descriptor, min(remaining, 1024 * 1024))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    try:
        current = os.lstat(canonical)
    except OSError as error:
        raise SourceEvidenceError(f"{label} changed while reading") from error
    payload = b"".join(chunks)
    if _identity(before) != _identity(after) or _identity(after) != _identity(current):
        raise SourceEvidenceError(f"{label} changed while reading")
    if len(payload) != before.st_size:
        raise SourceEvidenceError(f"{label} changed while reading")
    return payload


def _read_json(path: Path | str, label: str) -> Any:
    return _strict_json(_read_regular(path, label, MAX_JSON_BYTES), label)


def _binding(path: Path | str, label: str) -> tuple[Path, bytes]:
    payload = _read_regular(path, label)
    return _canonical(path, label), payload


def _verify_binding(binding: Mapping[str, Any], label: str) -> tuple[Path, bytes]:
    if not isinstance(binding, dict) or set(binding) != {"path", "sha256"}:
        raise SourceEvidenceError(f"{label} binding fields are invalid")
    if not isinstance(binding["sha256"], str) or not SHA256_RE.fullmatch(binding["sha256"]):
        raise SourceEvidenceError(f"{label} digest is invalid")
    path, payload = _binding(binding["path"], label)
    if _sha256_bytes(payload) != binding["sha256"]:
        raise SourceEvidenceError(f"{label} digest does not match")
    return path, payload


def _validate_schema(value: Any, name: str, label: str) -> None:
    try:
        from jsonschema import Draft202012Validator
    except ImportError as error:
        raise SourceEvidenceError("jsonschema dependency is unavailable") from error
    schema = _read_json(HERE / name, name)
    errors = sorted(Draft202012Validator(schema).iter_errors(value), key=lambda item: list(item.path))
    if errors:
        path = "/" + "/".join(str(item) for item in errors[0].path)
        raise SourceEvidenceError(f"{label} violates schema at {path}")


def _validate_schema_definition(value: Any, name: str, definition: str, label: str) -> None:
    try:
        from jsonschema import Draft202012Validator
    except ImportError as error:
        raise SourceEvidenceError("jsonschema dependency is unavailable") from error
    schema = _read_json(HERE / name, name)
    wrapper = {
        "$schema": schema["$schema"], "$defs": schema["$defs"],
        "$ref": f"#/$defs/{definition}",
    }
    errors = sorted(Draft202012Validator(wrapper).iter_errors(value), key=lambda item: list(item.path))
    if errors:
        path = "/" + "/".join(str(item) for item in errors[0].path)
        raise SourceEvidenceError(f"{label} violates schema at {path}")


def _relative(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value or value.startswith("/") or "\\" in value:
        raise SourceEvidenceError(f"{label} path is not normalized")
    parsed = PurePosixPath(value)
    if parsed.as_posix() != value or any(part in {"", ".", "..", ".git"} for part in parsed.parts):
        raise SourceEvidenceError(f"{label} path is not normalized")
    return value


def _member_path(root: Path, relative: str, label: str) -> Path:
    """Resolve no component while rejecting symlinked/non-directory ancestors."""
    parts = PurePosixPath(_relative(relative, label)).parts
    current = root
    for part in parts[:-1]:
        current /= part
        try:
            metadata = os.lstat(current)
        except OSError as error:
            raise SourceEvidenceError(f"{label} ancestor is missing or unsafe") from error
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise SourceEvidenceError(f"{label} ancestor is a path alias or unsafe")
    return current / parts[-1]


def _possibly_deleted_member_path(root: Path, relative: str, label: str) -> Path:
    """Return a lexical member path while checking every ancestor that exists."""
    parts = PurePosixPath(_relative(relative, label)).parts
    current = root
    missing = False
    for part in parts[:-1]:
        current /= part
        if missing:
            continue
        try:
            metadata = os.lstat(current)
        except FileNotFoundError:
            missing = True
            continue
        except OSError as error:
            raise SourceEvidenceError(f"{label} ancestor is missing or unsafe") from error
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise SourceEvidenceError(f"{label} ancestor is a path alias or unsafe")
    return current / parts[-1]


def _lexical_existing(path: Path | str, label: str) -> Path:
    """Validate aliases through the parent without dereferencing the final member."""
    raw = str(path)
    candidate = Path(raw)
    if not candidate.is_absolute() or os.path.normpath(raw) != raw:
        raise SourceEvidenceError(f"{label} path alias is invalid")
    parent = _canonical(candidate.parent, f"{label} parent")
    rebuilt = parent / candidate.name
    if str(rebuilt) != raw:
        raise SourceEvidenceError(f"{label} path alias is invalid")
    if not os.path.lexists(rebuilt):
        raise SourceEvidenceError(f"{label} is missing")
    return rebuilt


def _validate_source_identity(value: Any, label: str) -> None:
    fields = {"role", "repo", "head", "dirty_patch_sha256", "untracked_source_manifest_sha256"}
    if not isinstance(value, dict) or set(value) != fields:
        raise SourceEvidenceError(f"{label} fields are invalid")
    if not isinstance(value["role"], str) or not value["role"]:
        raise SourceEvidenceError(f"{label} role is invalid")
    _canonical(value["repo"], f"{label} repo")
    if not isinstance(value["head"], str) or not HEAD_RE.fullmatch(value["head"]):
        raise SourceEvidenceError(f"{label} head is invalid")
    for name in ("dirty_patch_sha256", "untracked_source_manifest_sha256"):
        if not isinstance(value[name], str) or not SHA256_RE.fullmatch(value[name]):
            raise SourceEvidenceError(f"{label} {name} is invalid")


def _git(repo: Path, *args: str, binary: bool = False) -> bytes | str:
    try:
        result = subprocess.run(
            ["git", "-C", str(repo), *args], stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise SourceEvidenceError("source snapshot Git observation failed") from error
    if result.returncode:
        raise SourceEvidenceError("source snapshot Git observation failed")
    return result.stdout if binary else result.stdout.decode("utf-8").strip()


def _snapshot_member(repo: Path, relative: str, final_blob: Path, staging_blob: Path) -> dict[str, Any]:
    _relative(relative, "snapshot member")
    path = _possibly_deleted_member_path(repo, relative, "snapshot member")
    if not os.path.lexists(path):
        tree = _git(repo, "ls-tree", "-z", "HEAD", "--", relative, binary=True)
        mode = "0000"
        if tree:
            prefix = tree.split(b" ", 1)[0]
            if prefix in {b"100644", b"100755"}:
                mode = "0" + prefix[-3:].decode("ascii")
        return {
            "path": relative, "type": "deleted", "mode": mode, "bytes": 0,
            "sha256": None, "symlink_target": None, "blob": None,
        }
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode):
        try:
            target = os.readlink(path)
            payload = target.encode("utf-8")
        except (OSError, UnicodeError) as error:
            raise SourceEvidenceError(f"snapshot member is an unreadable symlink: {relative}") from error
        kind = "symlink"
    elif stat.S_ISREG(before.st_mode):
        payload = _read_regular(path, f"snapshot member {relative}")
        target = None
        kind = "file"
    else:
        raise SourceEvidenceError(f"snapshot member type is unsupported: {relative}")
    after = os.lstat(path)
    if _identity(before) != _identity(after):
        raise SourceEvidenceError(f"snapshot member changed while reading: {relative}")
    staging_blob.write_bytes(payload)
    os.chmod(staging_blob, 0o600)
    return {
        "path": relative, "type": kind,
        "mode": f"{stat.S_IMODE(before.st_mode):04o}", "bytes": len(payload),
        "sha256": _sha256_bytes(payload), "symlink_target": target,
        "blob": str(final_blob),
    }


def _snapshot_paths(repo: Path) -> set[bytes]:
    paths: set[bytes] = set()
    for args in (
        ("ls-files", "--cached", "-z"),
        ("ls-files", "--others", "--exclude-standard", "-z"),
        ("ls-tree", "-r", "--name-only", "-z", "HEAD"),
    ):
        raw = _git(repo, *args, binary=True)
        paths.update(item for item in raw.split(b"\0") if item)
    return paths


def _member_stability_state(repo: Path, member: Mapping[str, Any]) -> tuple[Any, ...]:
    relative = member["path"]
    path = _possibly_deleted_member_path(repo, relative, "snapshot stability member")
    if not os.path.lexists(path):
        return (relative, "deleted", member["mode"], 0, None, None)
    metadata = os.lstat(path)
    if stat.S_ISLNK(metadata.st_mode):
        target = os.readlink(path)
        payload = target.encode("utf-8")
        kind = "symlink"
    elif stat.S_ISREG(metadata.st_mode):
        payload = _read_regular(path, f"snapshot stability member {relative}")
        target = None
        kind = "file"
    else:
        raise SourceEvidenceError(f"snapshot member type is unsupported: {relative}")
    return (
        relative, kind, f"{stat.S_IMODE(metadata.st_mode):04o}", len(payload),
        _sha256_bytes(payload), target,
    )


def capture_repository_snapshot(
    repo: Path | str,
    output_directory: Path | str,
    *,
    role: str,
    observer: Callable[[Path, str], Mapping[str, Any]],
) -> dict[str, str]:
    """Capture one recoverable Git observation outside every known source root.

    ``observer(repo, role)`` must be the runner's existing
    ``observe_source_identity`` (or a test-equivalent).  The callback is invoked
    before and after capture; any difference rejects the capture and leaves no
    published directory.
    """
    repository = _canonical(repo, "source repo")
    if repository.is_symlink() or not repository.is_dir():
        raise SourceEvidenceError("source repo is unsafe")
    if _git(repository, "rev-parse", "--show-toplevel") != str(repository):
        raise SourceEvidenceError("source repo is not an exact Git root")
    if not isinstance(role, str) or not role or not callable(observer):
        raise SourceEvidenceError("snapshot observer contract is invalid")
    destination = _canonical(output_directory, "snapshot output", exists=False)
    roots = _known_source_roots((repository,))
    if any(_within(destination, root) for root in roots):
        raise SourceEvidenceError("snapshot output must be outside source roots")
    if os.path.lexists(destination):
        raise SourceEvidenceError("snapshot output already exists")
    parent = _canonical(destination.parent, "snapshot output parent")
    parent_metadata = parent.stat()
    if parent_metadata.st_uid != os.getuid() or parent_metadata.st_mode & 0o077:
        raise SourceEvidenceError("snapshot output parent must be owner-only")

    before = dict(observer(repository, role))
    _validate_source_identity(before, "source identity")
    if before["repo"] != str(repository) or before["role"] != role:
        raise SourceEvidenceError("source observer returned a foreign identity")
    staging = Path(tempfile.mkdtemp(prefix=f".{destination.name}.tmp-", dir=parent))
    os.chmod(staging, 0o700)
    try:
        final_patch = destination / "patch.bin"
        patch = _git(repository, "diff", "--binary", "HEAD", "--", binary=True)
        staging_patch = staging / "patch.bin"
        staging_patch.write_bytes(patch)
        os.chmod(staging_patch, 0o600)
        # The patch preserves Git's exact dirty-state bytes.  The member set is
        # independently complete for every tracked and nonignored untracked
        # source path, so an observation remains recoverable and B/C comparison
        # can detect changes to an otherwise clean file.
        encoded_paths = _snapshot_paths(repository)
        blobs = staging / "members"
        blobs.mkdir(mode=0o700)
        members = []
        for index, encoded in enumerate(sorted(encoded_paths)):
            try:
                relative = encoded.decode("utf-8")
            except UnicodeError as error:
                raise SourceEvidenceError("snapshot member path is not UTF-8") from error
            blob_name = f"{index:06d}.bin"
            members.append(_snapshot_member(
                repository, relative, destination / "members" / blob_name, blobs / blob_name
            ))
        snapshot = {
            "schema_version": 1,
            "repository": str(repository),
            "source_identity": before,
            "patch": {"path": str(final_patch), "sha256": _sha256_bytes(patch), "bytes": len(patch)},
            "members": members,
        }
        _validate_schema(snapshot, "repository-snapshot.schema.json", "repository snapshot")
        snapshot_path = staging / "snapshot.json"
        snapshot_path.write_text(
            json.dumps(snapshot, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        os.chmod(snapshot_path, 0o600)
        after = dict(observer(repository, role))
        if after != before:
            raise SourceEvidenceError("source changed during snapshot capture")
        if _snapshot_paths(repository) != encoded_paths:
            raise SourceEvidenceError("source inventory changed during snapshot capture")
        for member in members:
            expected = (
                member["path"], member["type"], member["mode"], member["bytes"],
                member["sha256"], member["symlink_target"],
            )
            if _member_stability_state(repository, member) != expected:
                raise SourceEvidenceError("source inventory changed during snapshot capture")
        os.rename(staging, destination)
        return {"path": str(destination / "snapshot.json"), "sha256": _sha256_bytes((destination / "snapshot.json").read_bytes())}
    except BaseException:
        if staging.exists():
            shutil.rmtree(staging)
        raise


def _member_payload(path: Path, kind: str, label: str) -> tuple[os.stat_result, bytes, str | None]:
    metadata = os.lstat(path)
    if kind == "file" and stat.S_ISREG(metadata.st_mode):
        return metadata, _read_regular(path, label), None
    if kind == "symlink" and stat.S_ISLNK(metadata.st_mode):
        target = os.readlink(path)
        return metadata, target.encode("utf-8"), target
    if kind == "directory" and stat.S_ISDIR(metadata.st_mode) and not path.is_symlink():
        return metadata, b"", None
    raise SourceEvidenceError(f"{label} type does not match")


def _record(path: str, kind: str, mode: int, payload: bytes, target: str | None) -> dict[str, Any]:
    return {
        "path": path, "type": kind, "mode": f"{mode & 0o7777:04o}",
        "bytes": len(payload), "sha256": None if kind == "directory" else _sha256_bytes(payload),
        "symlink_target": target,
    }


def inventory_artifact(path: Path | str, artifact_type: str) -> list[dict[str, Any]]:
    """Return the exact sorted member inventory for a file, directory, tar, or zip."""
    artifact = _canonical(path, "artifact")
    if artifact_type == "file":
        metadata, payload, target = _member_payload(artifact, "file", "artifact")
        return [_record(artifact.name, "file", stat.S_IMODE(metadata.st_mode), payload, target)]
    if artifact_type == "directory":
        if artifact.is_symlink() or not artifact.is_dir():
            raise SourceEvidenceError("artifact directory is unsafe")
        records = []
        for current in sorted(artifact.rglob("*"), key=lambda item: item.relative_to(artifact).as_posix()):
            relative = current.relative_to(artifact).as_posix()
            metadata = os.lstat(current)
            kind = "symlink" if stat.S_ISLNK(metadata.st_mode) else "directory" if stat.S_ISDIR(metadata.st_mode) else "file" if stat.S_ISREG(metadata.st_mode) else "unsupported"
            if kind == "unsupported":
                raise SourceEvidenceError(f"artifact member type is unsupported: {relative}")
            metadata, payload, target = _member_payload(current, kind, f"artifact member {relative}")
            records.append(_record(relative, kind, stat.S_IMODE(metadata.st_mode), payload, target))
        return records
    if artifact_type == "tar":
        records = []
        seen: set[str] = set()
        try:
            with tarfile.open(artifact, "r:*") as archive:
                for member in archive.getmembers():
                    relative = member.name.rstrip("/")
                    _relative(relative, "archive member")
                    if relative in seen:
                        raise SourceEvidenceError("artifact contains duplicate members")
                    seen.add(relative)
                    if member.isfile():
                        stream = archive.extractfile(member)
                        if stream is None:
                            raise SourceEvidenceError("artifact member is unreadable")
                        payload, kind, target = stream.read(MAX_MEMBER_BYTES + 1), "file", None
                    elif member.issym():
                        payload, kind, target = member.linkname.encode("utf-8"), "symlink", member.linkname
                    elif member.isdir():
                        payload, kind, target = b"", "directory", None
                    else:
                        raise SourceEvidenceError("artifact contains unsupported member type")
                    if len(payload) > MAX_MEMBER_BYTES:
                        raise SourceEvidenceError("artifact member exceeds byte limit")
                    records.append(_record(relative, kind, member.mode, payload, target))
        except (OSError, tarfile.TarError) as error:
            raise SourceEvidenceError("artifact tar is invalid") from error
        return sorted(records, key=lambda item: item["path"])
    if artifact_type == "zip":
        records = []
        seen: set[str] = set()
        try:
            with zipfile.ZipFile(artifact) as archive:
                for member in archive.infolist():
                    relative = member.filename.rstrip("/")
                    _relative(relative, "archive member")
                    if relative in seen:
                        raise SourceEvidenceError("artifact contains duplicate members")
                    seen.add(relative)
                    unix_bits = (member.external_attr >> 16) & 0o177777
                    file_type = stat.S_IFMT(unix_bits)
                    if member.create_system == 3 and file_type not in {
                        0, stat.S_IFREG, stat.S_IFDIR, stat.S_IFLNK,
                    }:
                        raise SourceEvidenceError("artifact contains unsupported member type")
                    if member.create_system == 3 and file_type == stat.S_IFDIR and not member.is_dir():
                        raise SourceEvidenceError("artifact Unix directory type requires a directory name")
                    if member.is_dir():
                        if member.create_system == 3 and file_type not in {0, stat.S_IFDIR}:
                            raise SourceEvidenceError("artifact contains unsupported member type")
                        payload, kind, target = b"", "directory", None
                    elif member.create_system == 3 and file_type == stat.S_IFLNK:
                        payload = archive.read(member)
                        target, kind = payload.decode("utf-8"), "symlink"
                    else:
                        payload, kind, target = archive.read(member), "file", None
                    if len(payload) > MAX_MEMBER_BYTES:
                        raise SourceEvidenceError("artifact member exceeds byte limit")
                    mode = stat.S_IMODE(unix_bits) if member.create_system == 3 else (
                        0o755 if kind == "directory" else 0o644
                    )
                    records.append(_record(relative, kind, mode, payload, target))
        except (OSError, UnicodeError, zipfile.BadZipFile) as error:
            raise SourceEvidenceError("artifact zip is invalid") from error
        return sorted(records, key=lambda item: item["path"])
    raise SourceEvidenceError("artifact type is unsupported")


def _validate_snapshot(binding: Mapping[str, Any], expected_identity: Mapping[str, Any] | None = None) -> dict[str, Any]:
    path, payload = _verify_binding(binding, "repository snapshot")
    snapshot = _strict_json(payload, "repository snapshot")
    _validate_schema(snapshot, "repository-snapshot.schema.json", "repository snapshot")
    _validate_source_identity(snapshot["source_identity"], "snapshot source identity")
    if snapshot["repository"] != snapshot["source_identity"]["repo"]:
        raise SourceEvidenceError("snapshot repository does not match source identity")
    if expected_identity is not None and snapshot["source_identity"] != expected_identity:
        raise SourceEvidenceError("snapshot source identity does not match cohort observation")
    roots = _known_source_roots((_canonical(snapshot["repository"], "snapshot repository"),))
    if any(_within(path, root) for root in roots):
        raise SourceEvidenceError("repository snapshot must be outside source roots")
    patch_path, patch_payload = _verify_binding(
        {"path": snapshot["patch"]["path"], "sha256": snapshot["patch"]["sha256"]},
        "snapshot patch",
    )
    if snapshot["patch"]["bytes"] != len(patch_payload) or any(_within(patch_path, root) for root in roots):
        raise SourceEvidenceError("snapshot patch binding is invalid")
    if snapshot["source_identity"]["dirty_patch_sha256"] != snapshot["patch"]["sha256"]:
        raise SourceEvidenceError("snapshot patch changes historical digest meaning")
    seen: set[str] = set()
    for member in snapshot["members"]:
        relative = _relative(member["path"], "snapshot member")
        if relative in seen:
            raise SourceEvidenceError("snapshot member paths are duplicated")
        seen.add(relative)
        if member["type"] == "deleted":
            if any(member[key] is not None for key in ("sha256", "symlink_target", "blob")) or member["bytes"] != 0:
                raise SourceEvidenceError("deleted snapshot member is malformed")
            continue
        blob_path, blob = _binding(member["blob"], f"snapshot blob {relative}")
        if any(_within(blob_path, root) for root in roots):
            raise SourceEvidenceError("snapshot blob must be outside source roots")
        if len(blob) != member["bytes"] or _sha256_bytes(blob) != member["sha256"]:
            raise SourceEvidenceError(f"snapshot member bytes do not match: {relative}")
        if member["type"] == "symlink" and blob != member["symlink_target"].encode("utf-8"):
            raise SourceEvidenceError(f"snapshot symlink target does not match: {relative}")
    if [item["path"] for item in snapshot["members"]] != sorted(seen):
        raise SourceEvidenceError("snapshot member inventory is not sorted")
    return snapshot


def _retained_member_payload(
    member: Mapping[str, Any], snapshot: Mapping[str, Any], validation_mode: str,
    protected_roots: Sequence[Path],
) -> bytes:
    role = member["role"]
    if snapshot["source_identity"]["role"] != role:
        raise SourceEvidenceError("input member role does not match its snapshot")
    root = _canonical(member["root"], f"input member {role} root")
    if str(root) != snapshot["repository"]:
        raise SourceEvidenceError("input member root does not match its role snapshot")
    relative = _relative(member["path"], "input member")
    retained = member["retained_input"]
    if member["git_ignored"]:
        retained_path, payload = _verify_binding(retained, f"retained input {relative}")
        if any(_within(retained_path, protected) for protected in protected_roots):
            raise SourceEvidenceError("retained input must be outside source/export/output roots")
        if len(payload) != member["bytes"] or _sha256_bytes(payload) != member["sha256"]:
            raise SourceEvidenceError(f"retained input bytes do not match: {relative}")
        if member["type"] == "symlink" and payload != member["symlink_target"].encode("utf-8"):
            raise SourceEvidenceError(f"retained input symlink target does not match: {relative}")
    else:
        snapshot_members = {item["path"]: item for item in snapshot["members"]}
        source = snapshot_members.get(relative)
        if source is None or source["type"] == "deleted":
            raise SourceEvidenceError(f"input member is absent from role snapshot: {relative}")
        for key in ("type", "mode", "bytes", "sha256", "symlink_target"):
            if source[key] != member[key]:
                raise SourceEvidenceError(f"input member does not match snapshot {key}: {relative}")
        _blob_path, payload = _binding(source["blob"], f"snapshot input member {relative}")
        if len(payload) != member["bytes"] or _sha256_bytes(payload) != member["sha256"]:
            raise SourceEvidenceError(f"snapshot input member bytes do not match: {relative}")

    if validation_mode == "live":
        path = _member_path(root, relative, "input member")
        try:
            metadata, live_payload, target = _member_payload(
                path, member["type"], f"input member {relative}"
            )
        except (OSError, UnicodeError) as error:
            raise SourceEvidenceError(f"input member is missing or unsafe: {relative}") from error
        actual = _record(relative, member["type"], stat.S_IMODE(metadata.st_mode), live_payload, target)
        for key in ("type", "mode", "bytes", "sha256", "symlink_target"):
            if actual[key] != member[key]:
                raise SourceEvidenceError(f"input member {key} does not match: {relative}")
        observed_ignored = subprocess.run(
            ["git", "-C", str(root), "check-ignore", "-q", "--", relative],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        ).returncode == 0
        if member["git_ignored"] is not observed_ignored:
            raise SourceEvidenceError(f"input member ignored status does not match: {relative}")
    return payload


def _validate_input_manifest(
    binding: Mapping[str, Any], build_id: str, source_roots: Sequence[Path],
    source_snapshots: Mapping[str, Mapping[str, Any]], validation_mode: str,
    output_roots: Sequence[Path],
) -> dict[str, Any]:
    path, payload = _verify_binding(binding, "build input manifest")
    if any(_within(path, root) for root in source_roots):
        raise SourceEvidenceError("build input manifest must be outside source roots")
    value = _strict_json(payload, "build input manifest")
    _validate_schema(value, "build-input-manifest.schema.json", "build input manifest")
    if value["build_id"] != build_id:
        raise SourceEvidenceError("build input manifest id does not match")
    export_roots: dict[str, tuple[Path, Mapping[str, Any]]] = {}
    for export_root in value["export_roots"]:
        role = export_root["role"]
        if role in export_roots:
            raise SourceEvidenceError("build export root roles are duplicated")
        root = _outside_sources(export_root["path"], source_roots, "build export root")
        if root.is_symlink() or not root.is_dir():
            raise SourceEvidenceError("build export root is unsafe")
        if f"{stat.S_IMODE(root.stat().st_mode):04o}" != export_root["mode"]:
            raise SourceEvidenceError("build export root mode does not match")
        if any(_within(root, output) or _within(output, root) for output in output_roots):
            raise SourceEvidenceError("build export root overlaps a build output root")
        if any(_within(root, existing[0]) or _within(existing[0], root) for existing in export_roots.values()):
            raise SourceEvidenceError("build export roots overlap")
        observed_inventory = inventory_artifact(root, "directory")
        if observed_inventory != export_root["members"]:
            raise SourceEvidenceError("build export root inventory does not match")
        export_roots[role] = (root, export_root)

    if any(_within(path, root) for root, _entry in export_roots.values()) or any(
        _within(path, output) for output in output_roots
    ):
        raise SourceEvidenceError("build input manifest must be outside export/output roots")

    keys: set[tuple[str, str]] = set()
    members_by_source: dict[tuple[str, str], Mapping[str, Any]] = {}
    retained_payloads: dict[tuple[str, str], bytes] = {}
    for member in value["members"]:
        key = (member["role"], member["path"])
        if key in keys:
            raise SourceEvidenceError("build input role/path is duplicated or ambiguous")
        keys.add(key)
        snapshot = source_snapshots.get(member["role"])
        if snapshot is None:
            raise SourceEvidenceError("build input member has no role snapshot")
        members_by_source[key] = member
        retained_payloads[key] = _retained_member_payload(
            member, snapshot, validation_mode,
            [*source_roots, *(entry[0] for entry in export_roots.values()), *output_roots],
        )
    export_paths: set[str] = set()
    exported_members: dict[str, set[str]] = {role: set() for role in export_roots}
    for export in value["exports"]:
        root_entry = export_roots.get(export["export_root"])
        if root_entry is None:
            raise SourceEvidenceError("build export references an unknown export root")
        root = root_entry[0]
        raw_path = export["path"]
        if not isinstance(raw_path, str) or not raw_path.startswith("/") or os.path.normpath(raw_path) != raw_path:
            raise SourceEvidenceError("build export path alias is invalid")
        try:
            relative = Path(raw_path).relative_to(root).as_posix()
        except ValueError as error:
            raise SourceEvidenceError("build export is outside its declared export root") from error
        _relative(relative, "build export member")
        path = _member_path(root, relative, "build export")
        if str(path) != raw_path:
            raise SourceEvidenceError("build export path alias is invalid")
        if str(path) in export_paths:
            raise SourceEvidenceError("build export paths are duplicated")
        export_paths.add(str(path))
        exported_members[export["export_root"]].add(relative)
        source = members_by_source.get((export["source_role"], export["source_path"]))
        if source is None:
            raise SourceEvidenceError("build export has no bound source member")
        try:
            metadata, payload, target = _member_payload(path, export["type"], "build export")
        except OSError as error:
            raise SourceEvidenceError("build export is missing or unsafe") from error
        actual = _record(path.name, export["type"], stat.S_IMODE(metadata.st_mode), payload, target)
        for key in ("type", "mode", "bytes", "sha256", "symlink_target"):
            if actual[key] != export[key]:
                noun = "digest" if key == "sha256" else key
                raise SourceEvidenceError(f"build export {noun} does not match")
            if export[key] != source[key]:
                raise SourceEvidenceError(f"build export does not match source {key}")
        if payload != retained_payloads[(export["source_role"], export["source_path"])]:
            raise SourceEvidenceError("build export does not match retained source bytes")
    for role, (_root, export_root) in export_roots.items():
        complete = {item["path"] for item in export_root["members"] if item["type"] != "directory"}
        if exported_members[role] != complete:
            raise SourceEvidenceError("build export root inventory lacks exact export lineage")
    return value


def _verify_external_binding(binding: Mapping[str, Any], roots: Sequence[Path], label: str) -> Path:
    path, _payload = _verify_binding(binding, label)
    if any(_within(path, root) for root in roots):
        raise SourceEvidenceError(f"{label} must be outside source roots")
    return path


def verify_build_outputs(
    build: Mapping[str, Any], *, source_roots: Sequence[Path | str],
    source_snapshots: Mapping[str, Mapping[str, Any]] | None = None,
    validation_mode: str = "live",
) -> dict[str, Any]:
    """Rehash one closed build record, including inputs, exports and outputs."""
    if not isinstance(build, dict):
        raise SourceEvidenceError("build record is invalid")
    schema = _read_json(HERE / "cohort-inputs.schema.json", "cohort-inputs.schema.json")
    try:
        from jsonschema import Draft202012Validator
    except ImportError as error:
        raise SourceEvidenceError("jsonschema dependency is unavailable") from error
    wrapper = {
        "$schema": schema["$schema"], "$defs": schema["$defs"],
        "$ref": "#/$defs/build",
    }
    errors = sorted(Draft202012Validator(wrapper).iter_errors(build), key=lambda item: list(item.path))
    if errors:
        path = "/" + "/".join(str(item) for item in errors[0].path)
        raise SourceEvidenceError(f"build record violates schema at {path}")
    if validation_mode not in {"live", "retained"} or type(validation_mode) is not str:
        raise SourceEvidenceError("build validation mode is invalid")
    if not isinstance(source_snapshots, dict):
        raise SourceEvidenceError("build source snapshot contract is required")
    roots = tuple(_canonical(item, "source root") for item in source_roots)
    outputs = tuple(_canonical(item["path"], "build output") for item in build["outputs"])
    inputs = _validate_input_manifest(
        build["input_manifest"], build["id"], roots, source_snapshots,
        validation_mode, outputs,
    )
    input_roles = {item["role"] for item in inputs["members"]}
    if set(build["source_roles"]) != input_roles:
        raise SourceEvidenceError("build source roles do not match admitted inputs")
    recipe = build["recipe"]
    _canonical(recipe["cwd"], "build recipe cwd")
    command_paths = []
    for command in recipe["command_files"]:
        command_path, _payload = _verify_binding(command, "build command file")
        command_paths.append(command_path)
    try:
        argv_executable = _canonical(recipe["argv"][0], "build recipe argv executable")
    except SourceEvidenceError as error:
        raise SourceEvidenceError("build recipe argv does not match its first command file") from error
    if not command_paths or argv_executable != command_paths[0]:
        raise SourceEvidenceError("build recipe argv does not match its first command file")
    export_boundaries = tuple(
        _canonical(item["path"], "build export root") for item in inputs["export_roots"]
    )
    evidence_roots = (*roots, *export_boundaries, *outputs)
    _verify_external_binding(recipe["log"], evidence_roots, "build log")
    toolchain = build["toolchain"]
    for tool in toolchain["tools"]:
        path, payload = _binding(tool["path"], f"toolchain tool {tool['name']}")
        if _sha256_bytes(payload) != tool["sha256"]:
            raise SourceEvidenceError("toolchain tool digest does not match")
        if not tool["version"]:
            raise SourceEvidenceError("toolchain tool version is empty")
    for dependency in toolchain["dependencies"]:
        _verify_binding(dependency, "toolchain dependency")
    for evidence in toolchain["evidence"]:
        _verify_external_binding(evidence, evidence_roots, "toolchain evidence")
    for output in build["outputs"]:
        artifact = _outside_sources(output["path"], roots, "build output")
        raw = _read_regular(artifact, "build output") if output["type"] != "directory" else None
        if raw is not None and _sha256_bytes(raw) != output["sha256"]:
            raise SourceEvidenceError("build output digest does not match")
        if output["type"] == "directory":
            manifest_bytes = json.dumps(inventory_artifact(artifact, "directory"), sort_keys=True, separators=(",", ":")).encode()
            if _sha256_bytes(manifest_bytes) != output["sha256"]:
                raise SourceEvidenceError("build output directory digest does not match")
        observed_members = inventory_artifact(artifact, output["type"])
        if observed_members != output["members"]:
            raise SourceEvidenceError("artifact member inventory does not match")
    return dict(build)


def validate_cohort_inputs(path: Path | str, *, validation_mode: str = "live") -> dict[str, Any]:
    """Validate a cohort using explicit ``live`` admission or retained B bytes."""
    if validation_mode not in {"live", "retained"} or type(validation_mode) is not str:
        raise SourceEvidenceError("cohort validation mode is invalid")
    manifest_path, payload = _binding(path, "cohort input manifest")
    value = _strict_json(payload, "cohort input manifest")
    _validate_schema(value, "cohort-inputs.schema.json", "cohort input manifest")
    roles: set[str] = set()
    roots: list[Path] = []
    for observation in value["repository_observations"]:
        identity = observation["source_identity"]
        _validate_source_identity(identity, "cohort source identity")
        if identity["role"] in roles:
            raise SourceEvidenceError("cohort source roles are duplicated")
        roles.add(identity["role"])
        roots.append(_canonical(identity["repo"], "cohort source repo"))
    if any(_within(manifest_path, root) for root in roots):
        raise SourceEvidenceError("cohort input manifest must be outside source roots")
    snapshots: dict[str, Mapping[str, Any]] = {}
    for observation in value["repository_observations"]:
        role = observation["source_identity"]["role"]
        snapshots[role] = _validate_snapshot(
            observation["snapshot"], observation["source_identity"]
        )
    build_ids: set[str] = set()
    input_manifests: list[Mapping[str, Any]] = []
    for build in value["builds"]:
        if build["id"] in build_ids:
            raise SourceEvidenceError("cohort build ids are duplicated")
        build_ids.add(build["id"])
        if not set(build["source_roles"]).issubset(roles):
            raise SourceEvidenceError("build references an unobserved source role")
        verify_build_outputs(
            build, source_roots=roots, source_snapshots=snapshots,
            validation_mode=validation_mode,
        )
        _input_path, input_raw = _verify_binding(build["input_manifest"], "build input manifest")
        input_manifests.append(_strict_json(input_raw, "build input manifest"))

    export_roots = [
        _canonical(item["path"], "build export root")
        for manifest in input_manifests for item in manifest["export_roots"]
    ]
    output_roots = [
        _canonical(output["path"], "build output")
        for build in value["builds"] for output in build["outputs"]
    ]
    protected_roots = [*roots, *export_roots, *output_roots]
    evidence_paths: list[Path] = [manifest_path]
    for observation in value["repository_observations"]:
        snapshot_path = _canonical(observation["snapshot"]["path"], "repository snapshot")
        evidence_paths.append(snapshot_path)
        snapshot = snapshots[observation["source_identity"]["role"]]
        evidence_paths.append(_canonical(snapshot["patch"]["path"], "snapshot patch"))
        evidence_paths.extend(
            _canonical(member["blob"], "snapshot blob")
            for member in snapshot["members"] if member["blob"] is not None
        )
    for build in value["builds"]:
        evidence_paths.append(_canonical(build["input_manifest"]["path"], "build input manifest"))
        evidence_paths.append(_canonical(build["recipe"]["log"]["path"], "build log"))
        evidence_paths.extend(
            _canonical(item["path"], "toolchain evidence")
            for item in build["toolchain"]["evidence"]
        )
    evidence_paths.extend(
        _canonical(member["retained_input"]["path"], "retained input")
        for manifest in input_manifests for member in manifest["members"]
        if member["retained_input"] is not None
    )
    if any(
        _within(evidence, protected)
        for evidence in evidence_paths for protected in protected_roots
    ):
        raise SourceEvidenceError(
            "snapshot/evidence path must be outside protected source/export/output roots"
        )
    return value


def _loaded_input_manifests(cohort: Mapping[str, Any]) -> list[tuple[Mapping[str, Any], Mapping[str, Any]]]:
    loaded = []
    for build in cohort["builds"]:
        _path, raw = _verify_binding(build["input_manifest"], "build input manifest")
        loaded.append((build, _strict_json(raw, "build input manifest")))
    return loaded


def _cohort_protected_roots(
    cohort: Mapping[str, Any], manifests: Sequence[tuple[Mapping[str, Any], Mapping[str, Any]]],
) -> list[Path]:
    roots = [
        _canonical(item["source_identity"]["repo"], "cohort source root")
        for item in cohort["repository_observations"]
    ]
    roots.extend(
        _canonical(item["path"], "build export root")
        for _build, manifest in manifests for item in manifest["export_roots"]
    )
    roots.extend(
        _canonical(output["path"], "build output")
        for build in cohort["builds"] for output in build["outputs"]
    )
    return roots


def _ignored_specs(
    manifests: Sequence[tuple[Mapping[str, Any], Mapping[str, Any]]],
) -> list[tuple[str, Mapping[str, Any]]]:
    return sorted(
        (
            (build["id"], member)
            for build, manifest in manifests for member in manifest["members"]
            if member["git_ignored"]
        ),
        key=lambda item: (item[0], item[1]["role"], item[1]["path"]),
    )


def _observe_ignored_member(
    build_id: str, member: Mapping[str, Any], final_blob: Path | None,
    staging_blob: Path | None,
) -> dict[str, Any]:
    root = _canonical(member["root"], "ignored input root")
    relative = _relative(member["path"], "ignored input")
    path = _possibly_deleted_member_path(root, relative, "ignored input")
    base = {"build_id": build_id, "role": member["role"], "root": str(root), "path": relative}
    if not os.path.lexists(path):
        return {
            **base, "presence": "absent", "type": None, "mode": "0000",
            "bytes": 0, "sha256": None, "symlink_target": None, "blob": None,
        }
    before = os.lstat(path)
    if stat.S_ISREG(before.st_mode):
        payload = _read_regular(path, f"ignored input {relative}")
        kind, target = "file", None
    elif stat.S_ISLNK(before.st_mode):
        try:
            target = os.readlink(path)
            payload = target.encode("utf-8")
        except (OSError, UnicodeError) as error:
            raise SourceEvidenceError(f"ignored input symlink is unreadable: {relative}") from error
        kind = "symlink"
    elif stat.S_ISDIR(before.st_mode):
        payload, kind, target = b"", "directory", None
    else:
        raise SourceEvidenceError(f"ignored input type is unsupported: {relative}")
    after = os.lstat(path)
    if _identity(before) != _identity(after):
        raise SourceEvidenceError(f"ignored input changed while observing: {relative}")
    blob: str | None = None
    digest: str | None = None
    if kind != "directory":
        if final_blob is None or staging_blob is None:
            blob = None
        else:
            staging_blob.write_bytes(payload)
            os.chmod(staging_blob, 0o600)
            blob = str(final_blob)
        digest = _sha256_bytes(payload)
    return {
        **base, "presence": "present", "type": kind,
        "mode": f"{stat.S_IMODE(before.st_mode):04o}", "bytes": len(payload),
        "sha256": digest, "symlink_target": target, "blob": blob,
    }


def _ignored_state(value: Mapping[str, Any]) -> tuple[Any, ...]:
    return tuple(value[key] for key in (
        "build_id", "role", "root", "path", "presence", "type", "mode",
        "bytes", "sha256", "symlink_target",
    ))


def _utc_now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z")


def capture_ignored_input_observation(
    cohort_inputs: Mapping[str, str], repository_after: Sequence[Mapping[str, Any]],
    output_directory: Path | str,
) -> dict[str, str]:
    """Capture admitted ignored inputs after C snapshots and final annotations.

    The observation is bound to the exact retained B cohort and exact C
    repository snapshot bindings. It never revalidates mutable report files.
    """
    cohort_path, cohort_raw = _verify_binding(cohort_inputs, "cohort inputs")
    cohort = validate_cohort_inputs(cohort_path, validation_mode="retained")
    if _strict_json(cohort_raw, "cohort inputs") != cohort:
        raise SourceEvidenceError("cohort inputs changed during ignored-input capture")
    if not isinstance(repository_after, (list, tuple)) or not repository_after:
        raise SourceEvidenceError("C repository snapshot bindings are required")
    after_records = [dict(item) for item in repository_after]
    if any(set(item) != {"repo", "path", "sha256"} for item in after_records):
        raise SourceEvidenceError("C repository snapshot binding fields are invalid")
    if [item["repo"] for item in after_records] != sorted(item["repo"] for item in after_records):
        raise SourceEvidenceError("C repository snapshot bindings are not sorted")
    if len({item["repo"] for item in after_records}) != len(after_records):
        raise SourceEvidenceError("C repository snapshot bindings are duplicated")
    cohort_roles = {item["source_identity"]["role"] for item in cohort["repository_observations"]}
    if {item["repo"] for item in after_records} != cohort_roles:
        raise SourceEvidenceError("C repository snapshots do not cover the cohort")
    for item in after_records:
        snapshot = _validate_snapshot({"path": item["path"], "sha256": item["sha256"]})
        if snapshot["source_identity"]["role"] != item["repo"]:
            raise SourceEvidenceError("C repository snapshot role does not match")

    manifests = _loaded_input_manifests(cohort)
    protected_roots = _cohort_protected_roots(cohort, manifests)
    destination = _canonical(output_directory, "ignored-input observation output", exists=False)
    if os.path.lexists(destination):
        raise SourceEvidenceError("ignored-input observation output already exists")
    if any(_within(destination, root) for root in protected_roots):
        raise SourceEvidenceError("ignored-input observation must be outside protected roots")
    parent = _canonical(destination.parent, "ignored-input observation parent")
    parent_metadata = parent.stat()
    if parent_metadata.st_uid != os.getuid() or parent_metadata.st_mode & 0o077:
        raise SourceEvidenceError("ignored-input observation parent must be owner-only")

    staging = Path(tempfile.mkdtemp(prefix=f".{destination.name}.tmp-", dir=parent))
    os.chmod(staging, 0o700)
    started_at = _utc_now()
    try:
        blob_root = staging / "members"
        blob_root.mkdir(mode=0o700)
        members = []
        specs = _ignored_specs(manifests)
        for index, (build_id, member) in enumerate(specs):
            blob_name = f"{index:06d}.bin"
            members.append(_observe_ignored_member(
                build_id, member, destination / "members" / blob_name,
                blob_root / blob_name,
            ))
        observation = {
            "schema_version": 1,
            "capture_phase": "C-after-annotations-and-repository-snapshots",
            "started_at": started_at, "ended_at": _utc_now(),
            "cohort_inputs": dict(cohort_inputs),
            "repository_after": after_records,
            "members": members,
        }
        _validate_schema_definition(
            observation, "finalization.schema.json", "ignored_input_observation",
            "ignored-input observation",
        )
        path = staging / "observation.json"
        path.write_text(json.dumps(observation, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        os.chmod(path, 0o600)

        for expected, (build_id, member) in zip(members, specs):
            current = _observe_ignored_member(build_id, member, None, None)
            if _ignored_state(current) != _ignored_state(expected):
                raise SourceEvidenceError("ignored input changed during C observation")
        _verify_binding(cohort_inputs, "cohort inputs")
        for item in after_records:
            _verify_binding({"path": item["path"], "sha256": item["sha256"]}, "C repository snapshot")
        os.rename(staging, destination)
        observation_path = destination / "observation.json"
        return {"path": str(observation_path), "sha256": _sha256_bytes(observation_path.read_bytes())}
    except BaseException:
        if staging.exists():
            shutil.rmtree(staging)
        raise


def _validate_ignored_input_observation(
    binding: Mapping[str, Any], cohort_binding: Mapping[str, str],
    cohort: Mapping[str, Any], repository_after: Sequence[Mapping[str, Any]],
    protected_roots: Sequence[Path],
) -> dict[str, Any]:
    observation_path, raw = _verify_binding(binding, "C ignored-input observation")
    if any(_within(observation_path, root) for root in protected_roots):
        raise SourceEvidenceError("C ignored-input observation must be outside protected roots")
    value = _strict_json(raw, "C ignored-input observation")
    _validate_schema_definition(
        value, "finalization.schema.json", "ignored_input_observation",
        "C ignored-input observation",
    )
    if value["cohort_inputs"] != cohort_binding:
        raise SourceEvidenceError("C ignored-input observation has foreign cohort inputs")
    if value["repository_after"] != list(repository_after):
        raise SourceEvidenceError("C ignored-input observation has stale repository bindings")

    manifests = _loaded_input_manifests(cohort)
    expected = {
        (build_id, member["role"], member["root"], member["path"]): member
        for build_id, member in _ignored_specs(manifests)
    }
    observed: dict[tuple[str, str, str, str], Mapping[str, Any]] = {}
    observation_root = observation_path.parent
    for member in value["members"]:
        key = tuple(member[name] for name in ("build_id", "role", "root", "path"))
        if key in observed:
            raise SourceEvidenceError("C ignored-input observation members are duplicated")
        observed[key] = member
        if member["presence"] == "absent":
            if member["type"] is not None or member["mode"] != "0000" or member["bytes"] != 0 or any(
                member[name] is not None for name in ("sha256", "symlink_target", "blob")
            ):
                raise SourceEvidenceError("absent C ignored-input observation is malformed")
            continue
        if member["type"] == "directory":
            if member["bytes"] != 0 or member["sha256"] is not None or member["symlink_target"] is not None or member["blob"] is not None:
                raise SourceEvidenceError("directory C ignored-input observation is malformed")
            continue
        blob_path, payload = _binding(member["blob"], "C ignored-input blob")
        if not _within(blob_path, observation_root) or any(
            _within(blob_path, root) for root in protected_roots
        ):
            raise SourceEvidenceError("C ignored-input blob must be observation-owned and outside protected roots")
        if len(payload) != member["bytes"] or _sha256_bytes(payload) != member["sha256"]:
            raise SourceEvidenceError("C ignored-input blob bytes do not match")
        if member["type"] == "symlink":
            if member["symlink_target"] is None or payload != member["symlink_target"].encode("utf-8"):
                raise SourceEvidenceError("C ignored-input symlink target does not match")
        elif member["symlink_target"] is not None:
            raise SourceEvidenceError("C ignored-input file has a symlink target")
    if set(observed) != set(expected):
        raise SourceEvidenceError("C ignored-input observation does not cover admitted inputs")
    for key, before in expected.items():
        after = observed[key]
        if after["presence"] != "present" or any(
            after[field] != before[field]
            for field in ("type", "mode", "bytes", "sha256", "symlink_target")
        ):
            raise SourceEvidenceError("C ignored input changed after B")
    return value


__all__ = [
    "SourceEvidenceError", "capture_repository_snapshot", "capture_ignored_input_observation",
    "inventory_artifact",
    "validate_cohort_inputs", "verify_build_outputs",
]
