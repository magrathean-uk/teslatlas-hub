import importlib.util
import os
from pathlib import Path
import stat
import tempfile
import unittest


MODULE_PATH = Path(__file__).with_name("source_handoff.py")
SPEC = importlib.util.spec_from_file_location("source_handoff", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
source_handoff = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(source_handoff)


def make_minimal_source_root(root: Path) -> None:
    for relative in source_handoff.ROOT_FILES + source_handoff.PUBLIC_DOCUMENTS:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(relative, encoding="utf-8")
    for relative in source_handoff.SOURCE_DIRECTORIES:
        (root / relative).mkdir(parents=True, exist_ok=True)


def metadata_snapshot(root: Path) -> dict[str, tuple[str, int, int]]:
    result: dict[str, tuple[str, int, int]] = {}
    for path in [root, *sorted(root.rglob("*"))]:
        information = path.lstat()
        kind = "directory" if stat.S_ISDIR(information.st_mode) else "file"
        relative = "." if path == root else path.relative_to(root).as_posix()
        result[relative] = (
            kind,
            stat.S_IMODE(information.st_mode),
            information.st_mtime_ns,
        )
    return result


class SourceHandoffTests(unittest.TestCase):
    def test_prepare_is_deterministic_and_verifies_exact_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first = root / "first"
            second = root / "second"
            previous_umask = os.umask(0o022)
            try:
                first_manifest = source_handoff.prepare(
                    source_handoff._source_root(),
                    first,
                )
            finally:
                os.umask(previous_umask)
            previous_umask = os.umask(0o077)
            try:
                second_manifest = source_handoff.prepare(
                    source_handoff._source_root(),
                    second,
                )
            finally:
                os.umask(previous_umask)

            self.assertEqual(first_manifest, second_manifest)
            self.assertEqual(
                (first / source_handoff.HANDOFF_MANIFEST).read_bytes(),
                (second / source_handoff.HANDOFF_MANIFEST).read_bytes(),
            )
            self.assertEqual(metadata_snapshot(first), metadata_snapshot(second))
            self.assertEqual(
                first_manifest["source_package"]["canonical_root"],
                "teslatlas-sdk-swift",
            )
            self.assertEqual(
                first_manifest["public_products"],
                list(source_handoff.PUBLIC_PRODUCTS),
            )
            self.assertFalse(first_manifest["runtime_acceptance"])
            source_handoff.verify(first)

    def test_validator_rejects_changed_and_extra_files(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            handoff = Path(temporary) / "handoff"
            source_handoff.prepare(source_handoff._source_root(), handoff)
            package = handoff / source_handoff.CANONICAL_ROOT

            readme = package / "README.md"
            original = readme.read_bytes()
            readme.write_bytes(original + b"changed\n")
            source_handoff._normalize_tree_metadata(handoff)
            with self.assertRaisesRegex(source_handoff.HandoffError, "bytes differ"):
                source_handoff.verify(handoff)

            readme.write_bytes(original)
            (package / "unexpected.txt").write_text("unexpected", encoding="utf-8")
            source_handoff._normalize_tree_metadata(handoff)
            with self.assertRaisesRegex(source_handoff.HandoffError, "missing or extra"):
                source_handoff.verify(handoff)

    def test_validator_rejects_special_entries_and_unexpected_directories(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            handoff = Path(temporary) / "handoff"
            source_handoff.prepare(source_handoff._source_root(), handoff)
            package = handoff / source_handoff.CANONICAL_ROOT

            fifo = package / "unexpected-fifo"
            os.mkfifo(fifo)
            os.utime(
                package,
                ns=(
                    source_handoff.FIXED_TIMESTAMP_NS,
                    source_handoff.FIXED_TIMESTAMP_NS,
                ),
            )
            with self.assertRaisesRegex(source_handoff.HandoffError, "special entry"):
                source_handoff.verify(handoff)
            fifo.unlink()

            (package / "unexpected-directory").mkdir()
            source_handoff._normalize_tree_metadata(handoff)
            with self.assertRaisesRegex(
                source_handoff.HandoffError,
                "missing or extra directories",
            ):
                source_handoff.verify(handoff)

    def test_validator_rejects_duplicate_manifest_keys(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            handoff = Path(temporary) / "handoff"
            source_handoff.prepare(source_handoff._source_root(), handoff)
            manifest = handoff / source_handoff.HANDOFF_MANIFEST
            text = manifest.read_text(encoding="utf-8")
            manifest.write_text(
                text.replace(
                    '  "source_only": true,',
                    '  "source_only": true,\n  "source_only": false,',
                    1,
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(source_handoff.HandoffError, "duplicate key"):
                source_handoff.verify(handoff)

    def test_validator_enforces_modes_and_timestamps(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            handoff = Path(temporary) / "handoff"
            source_handoff.prepare(source_handoff._source_root(), handoff)
            readme = handoff / source_handoff.CANONICAL_ROOT / "README.md"

            readme.chmod(0o600)
            with self.assertRaisesRegex(source_handoff.HandoffError, "mode differs"):
                source_handoff.verify(handoff)

            readme.chmod(source_handoff.REGULAR_FILE_MODE)
            os.utime(readme, ns=(1, 1))
            with self.assertRaisesRegex(
                source_handoff.HandoffError,
                "modification timestamp differs",
            ):
                source_handoff.verify(handoff)

    def test_source_selection_rejects_symlink_root_and_escaped_file(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "source"
            root.mkdir()
            make_minimal_source_root(root)
            external = Path(temporary) / "external"
            external.mkdir()

            (root / "Sources").rmdir()
            (root / "Sources").symlink_to(external, target_is_directory=True)
            with self.assertRaisesRegex(
                source_handoff.HandoffError,
                "selection root is a symlink",
            ):
                source_handoff._selected_source_files(root)

            (root / "Sources").unlink()
            (root / "Sources").mkdir()
            external_file = external / "escaped.swift"
            external_file.write_text("escaped", encoding="utf-8")
            (root / "Sources" / "escaped.swift").symlink_to(external_file)
            with self.assertRaisesRegex(source_handoff.HandoffError, "symlink"):
                source_handoff._selected_source_files(root)

    def test_prepare_refuses_existing_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "existing"
            output.mkdir()
            with self.assertRaisesRegex(source_handoff.HandoffError, "already exists"):
                source_handoff.prepare(source_handoff._source_root(), output)


if __name__ == "__main__":
    unittest.main()
