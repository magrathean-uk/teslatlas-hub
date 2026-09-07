#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
import sys
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
from candidate_source_manifest import CandidateManifestError, load_candidate_paths


class CandidateSourceManifestTests(unittest.TestCase):
    def write_manifest(self, root: Path, paths: list[str]) -> Path:
        files = []
        for relative in paths:
            source = root / relative
            payload = source.read_bytes()
            files.append(
                {
                    "path": relative,
                    "sha256": hashlib.sha256(payload).hexdigest(),
                    "mode": f"{source.stat().st_mode & 0o7777:04o}",
                    "bytes": len(payload),
                }
            )
        manifest = root.parent / "candidate.json"
        manifest.write_text(
            json.dumps(
                {
                    "schema": "teslatlas.candidate-source-manifest/v1",
                    "files": files,
                }
            )
            + "\n"
        )
        return manifest

    def test_untracked_regular_file_is_bound_by_path_digest_mode_and_size(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "repo"
            source = root / "src/runtime/new_module.rs"
            source.parent.mkdir(parents=True)
            source.write_text("pub const VALUE: u8 = 7;\n")
            manifest = self.write_manifest(root, ["src/runtime/new_module.rs"])
            self.assertEqual(
                load_candidate_paths(root, manifest), {"src/runtime/new_module.rs"}
            )

    def test_changed_candidate_bytes_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "repo"
            source = root / "source.rs"
            source.parent.mkdir(parents=True)
            source.write_text("before\n")
            manifest = self.write_manifest(root, ["source.rs"])
            source.write_text("befoze\n")
            with self.assertRaisesRegex(CandidateManifestError, "digest"):
                load_candidate_paths(root, manifest)

    def test_candidate_symlink_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "repo"
            root.mkdir()
            target = root / "target"
            target.write_text("target\n")
            link = root / "link"
            os.symlink(target.name, link)
            manifest = root.parent / "candidate.json"
            manifest.write_text(
                json.dumps(
                    {
                        "schema": "teslatlas.candidate-source-manifest/v1",
                        "files": [
                            {
                                "path": "link",
                                "sha256": hashlib.sha256(target.read_bytes()).hexdigest(),
                                "mode": "0777",
                                "bytes": target.stat().st_size,
                            }
                        ],
                    }
                )
            )
            with self.assertRaisesRegex(CandidateManifestError, "regular file"):
                load_candidate_paths(root, manifest)

    def test_candidate_symlink_ancestor_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / "repo"
            root.mkdir()
            outside = base / "outside"
            outside.mkdir()
            payload = outside / "payload.rs"
            payload.write_text("outside\n")
            os.symlink(outside, root / "src")
            manifest = self.write_manifest(root, ["src/payload.rs"])
            with self.assertRaisesRegex(CandidateManifestError, "ancestor"):
                load_candidate_paths(root, manifest)

    def test_candidate_fifo_is_rejected_without_reading(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "repo"
            root.mkdir()
            fifo = root / "candidate.fifo"
            os.mkfifo(fifo)
            manifest = root.parent / "candidate.json"
            manifest.write_text(
                json.dumps(
                    {
                        "schema": "teslatlas.candidate-source-manifest/v1",
                        "files": [
                            {
                                "path": "candidate.fifo",
                                "sha256": hashlib.sha256(b"").hexdigest(),
                                "mode": "0644",
                                "bytes": 0,
                            }
                        ],
                    }
                )
            )
            with self.assertRaisesRegex(CandidateManifestError, "regular file"):
                load_candidate_paths(root, manifest)

    def test_candidate_size_and_mode_are_checked_before_content(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "repo"
            root.mkdir()
            source = root / "source.rs"
            source.write_text("source\n")
            manifest = self.write_manifest(root, ["source.rs"])
            value = json.loads(manifest.read_text())
            value["files"][0]["bytes"] += 1
            manifest.write_text(json.dumps(value))
            with self.assertRaisesRegex(CandidateManifestError, "size"):
                load_candidate_paths(root, manifest)

            manifest = self.write_manifest(root, ["source.rs"])
            value = json.loads(manifest.read_text())
            value["files"][0]["mode"] = "0600"
            manifest.write_text(json.dumps(value))
            with self.assertRaisesRegex(CandidateManifestError, "mode"):
                load_candidate_paths(root, manifest)

    def test_candidate_replacement_during_read_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "repo"
            root.mkdir()
            source = root / "source.rs"
            source.write_text("source\n")
            manifest = self.write_manifest(root, ["source.rs"])
            original_read = os.read
            replaced = False

            def replace_after_read(descriptor: int, length: int) -> bytes:
                nonlocal replaced
                payload = original_read(descriptor, length)
                if not replaced:
                    replaced = True
                    source.rename(root / "original.rs")
                    source.write_bytes(payload)
                return payload

            with mock.patch("candidate_source_manifest.os.read", replace_after_read):
                with self.assertRaisesRegex(CandidateManifestError, "changed"):
                    load_candidate_paths(root, manifest)


if __name__ == "__main__":
    unittest.main()
