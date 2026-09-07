#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Select and validate the Go executable used by packaging scripts."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys


SELECTION_ENV = "TESLATLAS_GO"


class GoToolchainError(RuntimeError):
    pass


def _validate_explicit_path(value: str) -> str:
    if not value or any(ord(character) < 32 or ord(character) == 127 for character in value):
        raise GoToolchainError(f"{SELECTION_ENV} is malformed")
    path = Path(value)
    if not path.is_absolute():
        raise GoToolchainError(f"{SELECTION_ENV} must be an absolute path")
    try:
        metadata = path.stat()
    except OSError as exc:
        raise GoToolchainError(f"{SELECTION_ENV} does not exist: {value}") from exc
    if not stat.S_ISREG(metadata.st_mode) or not os.access(path, os.X_OK):
        raise GoToolchainError(f"{SELECTION_ENV} must be an executable file: {value}")
    return value


def select_go(expected_version: str) -> str:
    if SELECTION_ENV in os.environ:
        selected = _validate_explicit_path(os.environ[SELECTION_ENV])
    else:
        discovered = shutil.which("go")
        if not discovered:
            raise GoToolchainError("go is required")
        selected = os.path.abspath(discovered)
        if not Path(selected).is_file() or not os.access(selected, os.X_OK):
            raise GoToolchainError("go is not an executable file")

    environment = os.environ.copy()
    environment.update({"GOENV": "off", "GOWORK": "off", "GOTOOLCHAIN": "local"})
    try:
        result = subprocess.run(
            [selected, "env", "GOVERSION"],
            env=environment,
            text=True,
            capture_output=True,
            check=True,
            timeout=15,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        raise GoToolchainError(f"cannot read the selected Go version: {selected}") from exc
    versions = result.stdout.splitlines()
    if versions != [expected_version]:
        actual = result.stdout.strip() or "<empty>"
        raise GoToolchainError(f"{expected_version} is required exactly: {actual}")
    return selected


def select_configured_go(expected_version: str) -> str | None:
    if SELECTION_ENV not in os.environ:
        return None
    return select_go(expected_version)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-version", required=True)
    args = parser.parse_args()
    try:
        print(select_go(args.expected_version))
        return 0
    except GoToolchainError as exc:
        print(f"go-toolchain: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
