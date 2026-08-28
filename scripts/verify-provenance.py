#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Fail closed unless every tracked file has one provenance classification."""

from __future__ import annotations

import argparse
import fnmatch
import json
from pathlib import Path, PurePosixPath
import subprocess
import sys
from typing import Any


SCHEMA = "teslatlas.provenance-classification/v1"
CLASSES = {
    "MAGRATHEAN-ORIGINAL",
    "COMPANY-ASSIGNED-CONTRIBUTION",
    "TESLAMATE-COMPATIBILITY",
    "THIRD-PARTY",
    "GENERATED",
    "DATA-OR-FACTS",
    "UNKNOWN",
}
EXCEPTION_CLASSES = {"THIRD-PARTY", "GENERATED", "DATA-OR-FACTS"}
EXCEPTION_FIELDS = {"origin", "licensing", "release_treatment"}


class ManifestError(RuntimeError):
    pass


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ManifestError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_manifest(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=reject_duplicate_keys)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ManifestError(f"cannot read provenance manifest: {exc}") from exc
    if not isinstance(value, dict):
        raise ManifestError("provenance manifest root must be an object")
    return value


def tracked_files(repo: Path) -> set[str]:
    try:
        completed = subprocess.run(
            ["git", "ls-files", "-z", "--cached"],
            cwd=repo,
            check=True,
            capture_output=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        raise ManifestError("cannot enumerate tracked files with git ls-files") from exc
    try:
        unmerged = subprocess.run(
            ["git", "ls-files", "-z", "--unmerged"],
            cwd=repo,
            check=True,
            capture_output=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        raise ManifestError("cannot inspect the index for unmerged paths") from exc
    if unmerged.stdout:
        raise ManifestError("repository index has unmerged paths")
    try:
        files = {
            item.decode("utf-8")
            for item in completed.stdout.split(b"\0")
            if item
        }
    except UnicodeDecodeError as exc:
        raise ManifestError("tracked paths must be UTF-8") from exc
    if not files:
        raise ManifestError("repository has no tracked files")
    return files


def validate_pattern(pattern: Any, *, glob: bool) -> str:
    if not isinstance(pattern, str) or not pattern:
        raise ManifestError("classification paths and globs must be non-empty strings")
    pure = PurePosixPath(pattern)
    if pattern.startswith("/") or "\\" in pattern or pure.as_posix() != pattern \
            or any(ord(character) < 32 for character in pattern) \
            or any(part in {"", ".", ".."} for part in pure.parts):
        raise ManifestError(f"classification pattern is not a normalized repository path: {pattern}")
    wild = any(character in pattern for character in "*?[")
    if glob and not wild:
        raise ManifestError(f"glob has no wildcard: {pattern}")
    if not glob and wild:
        raise ManifestError(f"explicit path contains a wildcard: {pattern}")
    if glob and pattern in {"*", "**", "**/*"}:
        raise ManifestError(f"catch-all provenance glob is forbidden: {pattern}")
    return pattern


def matches_glob(path: str, pattern: str) -> bool:
    # fnmatch's '*' crosses '/', so compare path segments explicitly.
    path_parts = path.split("/")
    pattern_parts = pattern.split("/")
    if len(path_parts) != len(pattern_parts):
        return False
    return all(fnmatch.fnmatchcase(part, wanted) for part, wanted in zip(path_parts, pattern_parts))


def require_nonempty_text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ManifestError(f"{label} must be non-empty text")
    return value


def verify(repo: Path, manifest_path: Path) -> int:
    manifest = load_manifest(manifest_path)
    if manifest.get("schema") != SCHEMA:
        raise ManifestError(f"manifest schema must be {SCHEMA}")
    declared = manifest.get("declared_classes")
    if not isinstance(declared, list) or not all(isinstance(item, str) for item in declared) \
            or set(declared) != CLASSES or len(declared) != len(CLASSES):
        raise ManifestError("declared_classes must contain each repository class exactly once")
    require_nonempty_text(manifest.get("scope_note"), "scope_note")

    rules = manifest.get("rules")
    if not isinstance(rules, list) or not rules:
        raise ManifestError("manifest rules must be a non-empty array")

    tracked = tracked_files(repo)
    coverage: dict[str, list[tuple[str, str]]] = {path: [] for path in tracked}
    rule_ids: set[str] = set()

    for position, rule in enumerate(rules, 1):
        if not isinstance(rule, dict):
            raise ManifestError(f"rule {position} must be an object")
        rule_id = require_nonempty_text(rule.get("id"), f"rule {position} id")
        if rule_id in rule_ids:
            raise ManifestError(f"duplicate rule id: {rule_id}")
        rule_ids.add(rule_id)

        classification = rule.get("class")
        if not isinstance(classification, str) or classification not in CLASSES:
            raise ManifestError(f"rule {rule_id} has an undeclared class")
        if classification == "UNKNOWN":
            raise ManifestError(f"rule {rule_id} uses UNKNOWN, which blocks release")
        require_nonempty_text(rule.get("rationale"), f"rule {rule_id} rationale")

        exception = rule.get("exception")
        if classification in EXCEPTION_CLASSES:
            if not isinstance(exception, dict) or set(exception) != EXCEPTION_FIELDS:
                raise ManifestError(
                    f"rule {rule_id} must document exactly: {', '.join(sorted(EXCEPTION_FIELDS))}"
                )
            for field in sorted(EXCEPTION_FIELDS):
                require_nonempty_text(exception[field], f"rule {rule_id} exception.{field}")
        elif exception is not None:
            raise ManifestError(f"rule {rule_id} has exception metadata for a non-exception class")

        paths = rule.get("paths", [])
        globs = rule.get("globs", [])
        if not isinstance(paths, list) or not isinstance(globs, list) or not (paths or globs):
            raise ManifestError(f"rule {rule_id} must have paths or globs arrays")

        for raw_path in paths:
            path = validate_pattern(raw_path, glob=False)
            if path not in tracked:
                raise ManifestError(f"rule {rule_id} explicit path is not tracked: {path}")
            coverage[path].append((rule_id, classification))
        for raw_glob in globs:
            pattern = validate_pattern(raw_glob, glob=True)
            matched = sorted(path for path in tracked if matches_glob(path, pattern))
            if not matched:
                raise ManifestError(f"rule {rule_id} glob matches no tracked file: {pattern}")
            for path in matched:
                coverage[path].append((rule_id, classification))

    missing = sorted(path for path, owners in coverage.items() if not owners)
    duplicate = sorted((path, owners) for path, owners in coverage.items() if len(owners) > 1)
    if missing:
        raise ManifestError("unclassified tracked files:\n  " + "\n  ".join(missing))
    if duplicate:
        detail = "\n  ".join(
            f"{path}: " + ", ".join(f"{rule_id} ({classification})" for rule_id, classification in owners)
            for path, owners in duplicate
        )
        raise ManifestError("tracked files with multiple classifications:\n  " + detail)
    return len(tracked)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--manifest", type=Path)
    args = parser.parse_args()
    repo = args.repo.resolve()
    manifest = (args.manifest or repo / "provenance-manifest.json").resolve()
    try:
        count = verify(repo, manifest)
    except ManifestError as exc:
        print(f"verify-provenance: {exc}", file=sys.stderr)
        return 1
    print(f"provenance classification passed: {count} tracked files, exactly one class each")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
