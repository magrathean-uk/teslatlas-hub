#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("normalize-tree-mtimes.py")
EPOCH = 1_700_000_000


class NormalizeTreeMtimesTests(unittest.TestCase):
    def run_tool(self, root: Path):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(root), "--epoch", str(EPOCH)],
            text=True,
            capture_output=True,
            check=False,
        )

    def test_normalizes_files_directories_and_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "source"
            child = root / "directory"
            child.mkdir(parents=True)
            regular = child / "file"
            regular.write_text("content", encoding="utf-8")
            link = root / "link"
            link.symlink_to("directory/file")
            result = self.run_tool(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            expected = EPOCH * 1_000_000_000
            self.assertEqual(root.lstat().st_mtime_ns, expected)
            self.assertEqual(child.lstat().st_mtime_ns, expected)
            self.assertEqual(regular.lstat().st_mtime_ns, expected)
            self.assertEqual(link.lstat().st_mtime_ns, expected)

    def test_rejects_special_member_without_changing_it(self) -> None:
        if not hasattr(os, "mkfifo"):
            self.skipTest("FIFO creation is unavailable")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "source"
            root.mkdir()
            fifo = root / "fifo"
            os.mkfifo(fifo)
            before = fifo.lstat().st_mtime_ns
            result = self.run_tool(root)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(fifo.lstat().st_mtime_ns, before)

    def test_rejects_symlink_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            real = parent / "real"
            real.mkdir()
            linked = parent / "linked"
            linked.symlink_to(real.name)
            result = self.run_tool(linked)
            self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
