#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Set every safe member of a materialized source tree to one exact mtime."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import stat
import sys


def fail(message: str) -> None:
    raise ValueError(message)


def members(root: Path) -> list[Path]:
    try:
        root_info = root.lstat()
    except FileNotFoundError:
        fail("source root does not exist")
    if not stat.S_ISDIR(root_info.st_mode) or root.is_symlink():
        fail("source root must be a real directory")
    result = [root]
    pending = [root]
    while pending:
        directory = pending.pop()
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except OSError as error:
            fail(f"cannot scan source tree: {error}")
        for entry in entries:
            path = Path(entry.path)
            info = entry.stat(follow_symlinks=False)
            if stat.S_ISDIR(info.st_mode):
                pending.append(path)
            elif not (stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode)):
                fail(f"source tree contains a special member: {path.relative_to(root)}")
            result.append(path)
    return result


def normalize(root: Path, epoch: int) -> int:
    selected = members(root)
    nanoseconds = epoch * 1_000_000_000
    for path in selected:
        os.utime(path, ns=(nanoseconds, nanoseconds), follow_symlinks=False)
    for path in selected:
        if path.lstat().st_mtime_ns != nanoseconds:
            fail(f"source tree mtime did not normalize: {path.relative_to(root)}")
    return len(selected)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True)
    parser.add_argument("--epoch", required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9]{1,10}", args.epoch):
        print("normalize-tree-mtimes: epoch must be explicit Unix seconds", file=sys.stderr)
        return 65
    try:
        count = normalize(Path(args.root), int(args.epoch))
    except (OSError, ValueError) as error:
        print(f"normalize-tree-mtimes: {error}", file=sys.stderr)
        return 65
    print(count)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
