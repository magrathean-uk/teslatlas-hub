#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Load and verify an immutable candidate source file set."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
from typing import Any


SCHEMA = "teslatlas.candidate-source-manifest/v1"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
MODE_RE = re.compile(r"^0[0-7]{3}$")


class CandidateManifestError(RuntimeError):
    pass


def _read_bound_regular_file(
    root: Path, relative: str, expected_mode: str, expected_size: int
) -> bytes:
    directory_flags = os.O_RDONLY
    if hasattr(os, "O_DIRECTORY"):
        directory_flags |= os.O_DIRECTORY
    if hasattr(os, "O_NOFOLLOW"):
        directory_flags |= os.O_NOFOLLOW
    try:
        directory_fd = os.open(root, directory_flags)
    except OSError as error:
        raise CandidateManifestError(
            f"candidate root is missing or unsafe: {error}"
        ) from error
    try:
        parts = PurePosixPath(relative).parts
        for part in parts[:-1]:
            try:
                child_fd = os.open(part, directory_flags, dir_fd=directory_fd)
            except OSError as error:
                raise CandidateManifestError(
                    f"candidate path has a missing or unsafe ancestor: {relative}"
                ) from error
            os.close(directory_fd)
            directory_fd = child_fd

        file_flags = os.O_RDONLY
        if hasattr(os, "O_NOFOLLOW"):
            file_flags |= os.O_NOFOLLOW
        if hasattr(os, "O_NONBLOCK"):
            file_flags |= os.O_NONBLOCK
        try:
            descriptor = os.open(parts[-1], file_flags, dir_fd=directory_fd)
        except OSError as error:
            raise CandidateManifestError(
                f"candidate path is not a regular file or is unsafe: {relative}"
            ) from error
        try:
            before = os.fstat(descriptor)
            if not stat.S_ISREG(before.st_mode):
                raise CandidateManifestError(
                    f"candidate path is not a regular file: {relative}"
                )
            if before.st_size != expected_size:
                raise CandidateManifestError(
                    f"candidate size does not match: {relative}"
                )
            if f"{stat.S_IMODE(before.st_mode):04o}" != expected_mode:
                raise CandidateManifestError(
                    f"candidate mode does not match: {relative}"
                )
            chunks: list[bytes] = []
            remaining = expected_size + 1
            while remaining:
                chunk = os.read(descriptor, min(remaining, 1024 * 1024))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
            payload = b"".join(chunks)
            after = os.fstat(descriptor)
            try:
                current = os.stat(
                    parts[-1], dir_fd=directory_fd, follow_symlinks=False
                )
            except OSError as error:
                raise CandidateManifestError(
                    f"candidate file changed while reading: {relative}"
                ) from error
            identity = lambda value: (
                value.st_dev,
                value.st_ino,
                value.st_mode,
                value.st_size,
                value.st_mtime_ns,
            )
            if identity(before) != identity(after) or (
                current.st_dev,
                current.st_ino,
            ) != (before.st_dev, before.st_ino):
                raise CandidateManifestError(
                    f"candidate file changed while reading: {relative}"
                )
            if len(payload) != expected_size:
                raise CandidateManifestError(
                    f"candidate size does not match: {relative}"
                )
            return payload
        finally:
            os.close(descriptor)
    finally:
        os.close(directory_fd)


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise CandidateManifestError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def _normalized_path(value: Any) -> str:
    if not isinstance(value, str) or not value:
        raise CandidateManifestError("candidate path must be non-empty text")
    path = PurePosixPath(value)
    if (
        value.startswith("/")
        or "\\" in value
        or path.as_posix() != value
        or any(ord(character) < 32 for character in value)
        or any(part in {"", ".", "..", ".git"} for part in path.parts)
    ):
        raise CandidateManifestError(f"candidate path is not normalized: {value}")
    return value


def load_candidate_paths(repo: Path, manifest_path: Path) -> set[str]:
    try:
        manifest = json.loads(
            manifest_path.read_text(encoding="utf-8"),
            object_pairs_hook=_reject_duplicate_keys,
        )
    except CandidateManifestError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise CandidateManifestError(f"cannot read candidate manifest: {error}") from error
    if not isinstance(manifest, dict) or set(manifest) != {"schema", "files"}:
        raise CandidateManifestError("candidate manifest must contain exactly schema and files")
    if manifest["schema"] != SCHEMA:
        raise CandidateManifestError(f"candidate manifest schema must be {SCHEMA}")
    records = manifest["files"]
    if not isinstance(records, list) or not records:
        raise CandidateManifestError("candidate manifest files must be a non-empty array")

    paths: set[str] = set()
    for position, record in enumerate(records, 1):
        if not isinstance(record, dict) or set(record) != {
            "path",
            "sha256",
            "mode",
            "bytes",
        }:
            raise CandidateManifestError(
                f"candidate file {position} must contain exactly path, sha256, mode and bytes"
            )
        relative = _normalized_path(record["path"])
        if relative in paths:
            raise CandidateManifestError(f"duplicate candidate path: {relative}")
        digest = record["sha256"]
        mode = record["mode"]
        size = record["bytes"]
        if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
            raise CandidateManifestError(f"candidate digest is invalid: {relative}")
        if not isinstance(mode, str) or not MODE_RE.fullmatch(mode):
            raise CandidateManifestError(f"candidate mode is invalid: {relative}")
        if not isinstance(size, int) or isinstance(size, bool) or size < 0:
            raise CandidateManifestError(f"candidate size is invalid: {relative}")
        payload = _read_bound_regular_file(repo, relative, mode, size)
        if hashlib.sha256(payload).hexdigest() != digest:
            raise CandidateManifestError(f"candidate digest does not match: {relative}")
        paths.add(relative)
    return paths
