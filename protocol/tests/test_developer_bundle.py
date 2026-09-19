from __future__ import annotations

import hashlib
import gzip
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
import zlib

from support import ROOT
from tools.developer_bundle import (
    BundleError,
    build_archive,
    canonical_gzip,
    canonical_json,
    extract_archive,
    read_archive,
    verify_archive,
    verify_directory,
)


class DeveloperBundleTests(unittest.TestCase):
    @staticmethod
    def write_canonical_archive(archive: Path, members: tuple[tuple[str, str], ...]) -> None:
        with archive.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0) as zipped:
                with tarfile.open(fileobj=zipped, mode="w", format=tarfile.GNU_FORMAT) as tar:
                    for name, kind in members:
                        info = tarfile.TarInfo(name)
                        info.mode = 0o644
                        info.mtime = 0
                        if kind == "link":
                            info.type = tarfile.SYMTYPE
                            info.linkname = "target"
                            tar.addfile(info)
                        else:
                            data = b"unsafe"
                            info.size = len(data)
                            tar.addfile(info, io.BytesIO(data))

    @staticmethod
    def write_payload_archive(
        archive: Path,
        root: str,
        files: dict[str, bytes],
        modes: dict[str, int],
    ) -> None:
        tar_bytes = io.BytesIO()
        with tarfile.open(fileobj=tar_bytes, mode="w", format=tarfile.GNU_FORMAT) as tar:
            for relative, data in sorted(files.items()):
                info = tarfile.TarInfo(f"{root}/{relative}")
                info.size = len(data)
                info.mode = modes[relative]
                info.uid = 0
                info.gid = 0
                info.uname = ""
                info.gname = ""
                info.mtime = 0
                tar.addfile(info, io.BytesIO(data))
        archive.write_bytes(canonical_gzip(tar_bytes.getvalue()))

    def test_bundle_is_byte_deterministic_complete_and_extractable(self) -> None:
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            first_archive, first_digest = build_archive(Path(first), ROOT)
            second_archive, second_digest = build_archive(Path(second), ROOT)
            self.assertEqual(first_digest, second_digest)
            self.assertEqual(first_archive.read_bytes(), second_archive.read_bytes())
            self.assertEqual(first_digest, hashlib.sha256(first_archive.read_bytes()).hexdigest())

            manifest = verify_archive(first_archive)
            self.assertEqual(manifest["python_requires"], ">=3.11")
            self.assertEqual(manifest["dependency_lock"]["path"], "uv.lock")

            extraction_root = Path(first) / "extracted"
            extracted = extract_archive(first_archive, extraction_root)
            self.assertEqual(verify_directory(extracted), manifest)
            self.assertTrue((extracted / "profiles/hub-http-v1/1.0.0/SHA256SUMS").is_file())
            self.assertTrue((extracted / "profiles/edge-delivery-v2/2.0.0/SHA256SUMS").is_file())

    def test_extracted_bundle_rejects_tamper_and_extra_members(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive, _ = build_archive(root / "output", ROOT)
            extracted = extract_archive(archive, root / "extract")
            readme = extracted / "README.md"
            readme.write_bytes(readme.read_bytes() + b"tampered\n")
            with self.assertRaisesRegex(BundleError, "checksum mismatch"):
                verify_directory(extracted)

            extracted = extract_archive(archive, root / "second")
            (extracted / "unexpected.txt").write_text("unexpected", encoding="utf-8")
            with self.assertRaisesRegex(BundleError, "membership mismatch"):
                verify_directory(extracted)

            extracted = extract_archive(archive, root / "third")
            (extracted / "README.md").unlink()
            with self.assertRaisesRegex(BundleError, "membership mismatch"):
                verify_directory(extracted)

    def test_archive_rejects_links_and_path_traversal(self) -> None:
        for name, kind in (("root/link", "link"), ("root/../escape", "file")):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                archive = Path(temporary) / "unsafe.tar.gz"
                self.write_canonical_archive(archive, ((name, kind),))
                with self.assertRaises(BundleError):
                    verify_archive(archive)

    def test_archive_rejects_duplicate_and_parent_file_conflicts(self) -> None:
        cases = (("root/file", "root/file"), ("root/parent", "root/parent/child"))
        for names in cases:
            with self.subTest(names=names), tempfile.TemporaryDirectory() as temporary:
                archive = Path(temporary) / "conflict.tar.gz"
                self.write_canonical_archive(archive, tuple((name, "file") for name in names))
                with self.assertRaises(BundleError):
                    verify_archive(archive)

    def test_archive_rejects_every_kind_of_trailing_input(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive, _ = build_archive(root / "output", ROOT)
            original = archive.read_bytes()

            raw_tail = root / "raw-tail.tar.gz"
            raw_tail.write_bytes(original + b"raw trailing bytes")
            with self.assertRaisesRegex(BundleError, "exactly one complete gzip member"):
                verify_archive(raw_tail)

            concatenated = root / "concatenated.tar.gz"
            concatenated.write_bytes(original + canonical_gzip(b"second member"))
            with self.assertRaisesRegex(BundleError, "exactly one complete gzip member"):
                verify_archive(concatenated)

            tar_tail = root / "tar-tail.tar.gz"
            decoded = zlib.decompress(original, wbits=16 + zlib.MAX_WBITS)
            tar_tail.write_bytes(canonical_gzip(decoded + b"decompressed trailing bytes"))
            with self.assertRaisesRegex(BundleError, "tar stream is not canonical"):
                verify_archive(tar_tail)

    def test_extract_rejects_renamed_root_without_side_effects(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive, _ = build_archive(root / "output", ROOT)
            _, files, modes = read_archive(archive)
            renamed = root / "renamed.tar.gz"
            self.write_payload_archive(renamed, "renamed-root", files, modes)
            destination = root / "must-not-exist"

            with self.assertRaisesRegex(BundleError, "archive root mismatch"):
                extract_archive(renamed, destination)
            self.assertFalse(destination.exists())

    def test_manifest_schema_contracts_and_file_order_are_closed(self) -> None:
        mutations = (
            ("top-level schema", lambda manifest: manifest.update({"unknown": True}), "top-level schema"),
            (
                "contract identity",
                lambda manifest: manifest["contracts"].update({"current_hub": "hub-http-v1@9.9.9"}),
                "contract identities",
            ),
            ("file order", lambda manifest: manifest["files"].reverse(), "canonical path order"),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive, _ = build_archive(root / "output", ROOT)
            for index, (name, mutate, message) in enumerate(mutations):
                with self.subTest(name=name):
                    extracted = extract_archive(archive, root / f"extract-{index}")
                    manifest_path = extracted / "BUNDLE-MANIFEST.json"
                    manifest = json.loads(manifest_path.read_bytes())
                    mutate(manifest)
                    manifest_path.write_bytes(canonical_json(manifest))
                    with self.assertRaisesRegex(BundleError, message):
                        verify_directory(extracted)

    def test_archive_rejects_special_mode_bits(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive, _ = build_archive(root / "output", ROOT)
            bundle_root, files, modes = read_archive(archive)
            target = "tools/check"
            for special_mode in (0o4755, 0o2755, 0o1755):
                with self.subTest(mode=oct(special_mode)):
                    changed_modes = dict(modes)
                    changed_modes[target] = special_mode
                    changed = root / f"mode-{special_mode:o}.tar.gz"
                    self.write_payload_archive(changed, bundle_root, files, changed_modes)
                    with self.assertRaisesRegex(BundleError, "metadata is not canonical"):
                        verify_archive(changed)


if __name__ == "__main__":
    unittest.main()
