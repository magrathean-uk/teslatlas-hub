#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for finite source, export, and build-output evidence."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tarfile
import tempfile
import unittest
import zipfile


HERE = Path(__file__).resolve().parent
MODULE = HERE / "source_evidence.py"


def load_module():
    spec = importlib.util.spec_from_file_location("task10_source_evidence", MODULE)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path: Path, value: object) -> Path:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


def run_git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    ).stdout.strip()


class SourceEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        os.chmod(self.root, 0o700)
        self.repo = self.root / "source"
        self.repo.mkdir()
        run_git(self.repo, "init", "-q")
        run_git(self.repo, "config", "user.email", "fixture@example.invalid")
        run_git(self.repo, "config", "user.name", "Fixture")
        (self.repo / ".gitignore").write_text("vendor-input.bin\n", encoding="utf-8")
        (self.repo / "Cargo.lock").write_text("lock-v1\n", encoding="utf-8")
        (self.repo / "profile.json").write_text('{"revision":1}\n', encoding="utf-8")
        (self.repo / "README.md").write_text("packaged docs\n", encoding="utf-8")
        (self.repo / "retained.txt").write_text("retained source\n", encoding="utf-8")
        (self.repo / "deleted.txt").write_text("delete me\n", encoding="utf-8")
        run_git(self.repo, "add", ".")
        run_git(self.repo, "commit", "-qm", "fixture")
        (self.repo / "Cargo.lock").write_text("lock-v2\n", encoding="utf-8")
        os.chmod(self.repo / "Cargo.lock", 0o755)
        (self.repo / "deleted.txt").unlink()
        (self.repo / "new.txt").write_text("new source\n", encoding="utf-8")
        os.symlink("README.md", self.repo / "docs-link")
        (self.repo / "vendor-input.bin").write_bytes(b"ignored build input")

    def tearDown(self):
        self.temporary.cleanup()

    def _observer(self, repo: Path, role: str) -> dict[str, object]:
        patch = subprocess.run(
            ["git", "-C", str(repo), "diff", "--binary", "HEAD", "--"],
            check=True, stdout=subprocess.PIPE,
        ).stdout
        names = subprocess.run(
            ["git", "-C", str(repo), "ls-files", "--others", "--exclude-standard", "-z"],
            check=True, stdout=subprocess.PIPE,
        ).stdout.split(b"\0")
        digest = hashlib.sha256()
        for encoded in sorted(item for item in names if item):
            path = repo / encoded.decode("utf-8")
            if path.is_symlink():
                kind = b"symlink"
                content = os.readlink(path).encode("utf-8")
            else:
                kind = b"file"
                content = path.read_bytes()
            digest.update(encoded + b"\0" + kind + b"\0")
            digest.update(hashlib.sha256(content).hexdigest().encode("ascii") + b"\0")
        return {
            "role": role,
            "repo": str(repo),
            "head": run_git(repo, "rev-parse", "HEAD"),
            "dirty_patch_sha256": hashlib.sha256(patch).hexdigest(),
            "untracked_source_manifest_sha256": digest.hexdigest(),
        }

    def _capture(self, name: str = "snapshot") -> dict[str, str]:
        module = load_module()
        return module.capture_repository_snapshot(
            self.repo, self.root / name, role="fixture_source", observer=self._observer
        )

    def _member(
        self, root: Path, relative: str, role: str = "fixture_source",
        retained_input: dict[str, str] | None = None,
    ) -> dict[str, object]:
        path = root / relative
        metadata = path.lstat()
        if path.is_symlink():
            target = os.readlink(path)
            payload = target.encode("utf-8")
            kind = "symlink"
        else:
            target = None
            payload = path.read_bytes()
            kind = "file"
        ignored = None
        if (root / ".git").is_dir():
            ignored = subprocess.run(
                ["git", "-C", str(root), "check-ignore", "-q", "--", relative]
            ).returncode == 0
        return {
            "role": role, "root": str(root), "path": relative, "type": kind,
            "mode": f"{stat.S_IMODE(metadata.st_mode):04o}", "bytes": len(payload),
            "sha256": hashlib.sha256(payload).hexdigest(), "symlink_target": target,
            "git_ignored": ignored, "retained_input": retained_input,
        }

    def _build_manifest(self, snapshot: dict[str, str]) -> Path:
        module = load_module()
        export_root = self.root / "export"
        export_root.mkdir()
        (export_root / "retained.txt").write_bytes((self.repo / "retained.txt").read_bytes())
        retained_ignored = self.root / "retained-ignored.bin"
        retained_ignored.write_bytes((self.repo / "vendor-input.bin").read_bytes())
        output = self.root / "artifact.tar"
        payload_root = self.root / "payload"
        payload_root.mkdir()
        (payload_root / "product").write_text("2026.36.2\n", encoding="utf-8")
        os.chmod(payload_root / "product", 0o755)
        with tarfile.open(output, "w") as archive:
            archive.add(payload_root / "product", arcname="bin/product", recursive=False)
        log = self.root / "build.log"
        log.write_text("synthetic build\n", encoding="utf-8")
        tool = self.root / "compiler"
        tool.write_text("compiler\n", encoding="utf-8")
        os.chmod(tool, 0o755)
        tool_evidence = write_json(self.root / "toolchain.json", {"compiler": "synthetic-1"})
        input_manifest = {
            "schema_version": 2,
            "build_id": "fixture-build",
            "members": [
                *[self._member(self.repo, name) for name in (
                    "Cargo.lock", "profile.json", "README.md", "retained.txt"
                )],
                self._member(
                    self.repo, "vendor-input.bin",
                    retained_input={"path": str(retained_ignored), "sha256": sha256(retained_ignored)},
                ),
            ],
            "export_roots": [{
                "role": "fixture_exports", "path": str(export_root),
                "mode": f"{stat.S_IMODE(export_root.stat().st_mode):04o}",
                "members": module.inventory_artifact(export_root, "directory"),
            }],
            "exports": [{
                "role": "retained_export", "path": str(export_root / "retained.txt"),
                "export_root": "fixture_exports",
                "source_role": "fixture_source", "source_path": "retained.txt",
                "type": "file", "mode": "0644", "bytes": len(b"retained source\n"),
                "sha256": hashlib.sha256(b"retained source\n").hexdigest(),
                "symlink_target": None,
            }],
        }
        input_path = write_json(self.root / "build-inputs.json", input_manifest)
        build = {
            "id": "fixture-build",
            "source_roles": ["fixture_source"],
            "input_manifest": {"path": str(input_path), "sha256": sha256(input_path)},
            "recipe": {
                "argv": [str(tool), "--locked"], "cwd": str(self.repo),
                "command_files": [{"path": str(tool), "sha256": sha256(tool)}],
                "log": {"path": str(log), "sha256": sha256(log)},
            },
            "toolchain": {
                "host": {"os": "synthetic", "architecture": "arm64", "native_or_emulated": "native"},
                "tools": [{"name": "compiler", "path": str(tool), "version": "1", "sha256": sha256(tool)}],
                "dependencies": [],
                "evidence": [{"path": str(tool_evidence), "sha256": sha256(tool_evidence)}],
            },
            "outputs": [{
                "role": "fixture_artifact", "path": str(output), "type": "tar",
                "sha256": sha256(output), "embedded_version": "2026.36.2",
                "members": module.inventory_artifact(output, "tar"),
            }],
        }
        cohort = {
            "schema_version": 2,
            "cohort_id": "fixture-cohort",
            "product_version": "2026.36.2",
            "repository_observations": [{
                "source_identity": self._observer(self.repo, "fixture_source"),
                "snapshot": snapshot,
            }],
            "builds": [build],
        }
        return write_json(self.root / "cohort-inputs.json", cohort)

    def test_module_contract_exists(self):
        self.assertTrue(MODULE.is_file(), "source_evidence.py is not implemented")

    def test_capture_retains_patch_files_deletion_symlink_mode_and_bytes(self):
        binding = self._capture()
        snapshot_path = Path(binding["path"])
        self.assertEqual(sha256(snapshot_path), binding["sha256"])
        snapshot = json.loads(snapshot_path.read_text(encoding="utf-8"))
        self.assertEqual(self._observer(self.repo, "fixture_source"), snapshot["source_identity"])
        by_path = {member["path"]: member for member in snapshot["members"]}
        self.assertEqual("deleted", by_path["deleted.txt"]["type"])
        self.assertEqual("symlink", by_path["docs-link"]["type"])
        self.assertEqual("README.md", by_path["docs-link"]["symlink_target"])
        self.assertEqual("0755", by_path["Cargo.lock"]["mode"])
        self.assertEqual(b"new source\n", Path(by_path["new.txt"]["blob"]).read_bytes())
        self.assertNotIn("vendor-input.bin", by_path)
        self.assertEqual(
            subprocess.run(["git", "-C", str(self.repo), "diff", "--binary", "HEAD", "--"], check=True, stdout=subprocess.PIPE).stdout,
            Path(snapshot["patch"]["path"]).read_bytes(),
        )

    def test_capture_rejects_change_during_observation_without_publishing_partial_snapshot(self):
        module = load_module()
        calls = 0

        def changing(repo: Path, role: str) -> dict[str, object]:
            nonlocal calls
            value = self._observer(repo, role)
            calls += 1
            if calls == 1:
                (repo / "new.txt").write_text("changed during capture\n", encoding="utf-8")
            return value

        destination = self.root / "unstable"
        with self.assertRaisesRegex(module.SourceEvidenceError, "changed during snapshot"):
            module.capture_repository_snapshot(
                self.repo, destination, role="fixture_source", observer=changing
            )
        self.assertFalse(destination.exists())

    def test_capture_rechecks_complete_inventory_after_late_mode_change(self):
        module = load_module()
        calls = 0

        def late_mode_change(repo: Path, role: str) -> dict[str, object]:
            nonlocal calls
            value = self._observer(repo, role)
            calls += 1
            if calls == 2:
                os.chmod(repo / "Cargo.lock", 0o700)
            return value

        destination = self.root / "late-mode"
        with self.assertRaisesRegex(module.SourceEvidenceError, "inventory|changed during snapshot"):
            module.capture_repository_snapshot(
                self.repo, destination, role="fixture_source", observer=late_mode_change
            )
        self.assertFalse(destination.exists())

    def test_capture_retains_nested_and_staged_deletions(self):
        module = load_module()
        nested = self.repo / "docs/obsolete/file.txt"
        nested.parent.mkdir(parents=True)
        nested.write_text("old\n", encoding="utf-8")
        staged = self.repo / "staged.txt"
        staged.write_text("staged\n", encoding="utf-8")
        run_git(self.repo, "add", "docs/obsolete/file.txt", "staged.txt")
        run_git(self.repo, "commit", "-qm", "deletion fixtures")
        nested.unlink()
        nested.parent.rmdir()
        nested.parent.parent.rmdir()
        run_git(self.repo, "rm", "-q", "staged.txt")

        binding = module.capture_repository_snapshot(
            self.repo, self.root / "deletions", role="fixture_source", observer=self._observer
        )
        snapshot = json.loads(Path(binding["path"]).read_text(encoding="utf-8"))
        members = {item["path"]: item for item in snapshot["members"]}
        self.assertEqual("deleted", members["docs/obsolete/file.txt"]["type"])
        self.assertEqual("deleted", members["staged.txt"]["type"])

    def test_capture_rejects_output_inside_source_and_symlink_alias(self):
        module = load_module()
        with self.assertRaisesRegex(module.SourceEvidenceError, "outside source roots"):
            module.capture_repository_snapshot(
                self.repo, self.repo / "evidence", role="fixture_source", observer=self._observer
            )
        alias = self.root / "alias"
        alias.symlink_to(self.repo, target_is_directory=True)
        with self.assertRaisesRegex(module.SourceEvidenceError, "alias|outside source roots"):
            module.capture_repository_snapshot(
                self.repo, alias / "evidence", role="fixture_source", observer=self._observer
            )

    def test_validate_cohort_inputs_checks_real_inputs_exports_and_archive_members(self):
        module = load_module()
        manifest = self._build_manifest(self._capture())
        validated = module.validate_cohort_inputs(manifest, validation_mode="live")
        self.assertEqual("fixture-cohort", validated["cohort_id"])
        self.assertEqual(["fixture-build"], [item["id"] for item in validated["builds"]])

    def test_lock_profile_doc_retained_source_and_ignored_input_tampering_fail(self):
        names = ("Cargo.lock", "profile.json", "README.md", "retained.txt", "vendor-input.bin")
        for index, name in enumerate(names):
            with self.subTest(name=name):
                if index:
                    self.tearDown()
                    self.setUp()
                module = load_module()
                manifest = self._build_manifest(self._capture())
                (self.repo / name).write_bytes(b"tampered\n")
                with self.assertRaisesRegex(module.SourceEvidenceError, "input member"):
                    module.validate_cohort_inputs(manifest, validation_mode="live")

    def test_source_to_export_and_artifact_member_mismatch_fail(self):
        module = load_module()
        manifest = self._build_manifest(self._capture())
        cohort = json.loads(manifest.read_text(encoding="utf-8"))
        input_path = Path(cohort["builds"][0]["input_manifest"]["path"])
        input_value = json.loads(input_path.read_text(encoding="utf-8"))
        input_value["exports"][0]["sha256"] = "a" * 64
        write_json(input_path, input_value)
        cohort["builds"][0]["input_manifest"]["sha256"] = sha256(input_path)
        write_json(manifest, cohort)
        with self.assertRaisesRegex(module.SourceEvidenceError, "export.*source|export digest"):
            module.validate_cohort_inputs(manifest, validation_mode="live")

        self.tearDown(); self.setUp()
        module = load_module()
        manifest = self._build_manifest(self._capture())
        cohort = json.loads(manifest.read_text(encoding="utf-8"))
        cohort["builds"][0]["outputs"][0]["members"][0]["sha256"] = "b" * 64
        write_json(manifest, cohort)
        with self.assertRaisesRegex(module.SourceEvidenceError, "artifact member"):
            module.validate_cohort_inputs(manifest, validation_mode="live")

    def test_unknown_fields_bool_integer_duplicate_keys_and_output_alias_fail(self):
        module = load_module()
        manifest = self._build_manifest(self._capture())
        cohort = json.loads(manifest.read_text(encoding="utf-8"))
        cohort["unknown"] = True
        write_json(manifest, cohort)
        with self.assertRaisesRegex(module.SourceEvidenceError, "schema"):
            module.validate_cohort_inputs(manifest, validation_mode="live")

        cohort.pop("unknown")
        cohort["schema_version"] = True
        write_json(manifest, cohort)
        with self.assertRaisesRegex(module.SourceEvidenceError, "schema"):
            module.validate_cohort_inputs(manifest, validation_mode="live")

        manifest.write_text('{"schema_version":1,"schema_version":1}\n', encoding="utf-8")
        with self.assertRaisesRegex(module.SourceEvidenceError, "duplicate JSON"):
            module.validate_cohort_inputs(manifest, validation_mode="live")

        self.tearDown(); self.setUp()
        module = load_module()
        manifest = self._build_manifest(self._capture())
        cohort = json.loads(manifest.read_text(encoding="utf-8"))
        cohort["builds"][0]["outputs"][0]["path"] = str(self.repo / "Cargo.lock")
        cohort["builds"][0]["outputs"][0]["type"] = "file"
        cohort["builds"][0]["outputs"][0]["sha256"] = sha256(self.repo / "Cargo.lock")
        cohort["builds"][0]["outputs"][0]["members"] = module.inventory_artifact(self.repo / "Cargo.lock", "file")
        write_json(manifest, cohort)
        with self.assertRaisesRegex(module.SourceEvidenceError, "outside source roots"):
            module.validate_cohort_inputs(manifest, validation_mode="live")

    def test_input_member_rejects_symlinked_ancestor(self):
        module = load_module()
        manifest = self._build_manifest(self._capture())
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "input.txt").write_text("escaped\n", encoding="utf-8")
        os.symlink(outside, self.repo / "escape", target_is_directory=True)
        cohort = json.loads(manifest.read_text(encoding="utf-8"))
        input_path = Path(cohort["builds"][0]["input_manifest"]["path"])
        inputs = json.loads(input_path.read_text(encoding="utf-8"))
        raw = (outside / "input.txt").read_bytes()
        retained_alias_input = self.root / "retained-alias-input.txt"
        retained_alias_input.write_bytes(raw)
        inputs["members"].append({
            "role": "fixture_source", "root": str(self.repo), "path": "escape/input.txt",
            "type": "file", "mode": "0644", "bytes": len(raw),
            "sha256": hashlib.sha256(raw).hexdigest(), "symlink_target": None,
            "git_ignored": True,
            "retained_input": {"path": str(retained_alias_input), "sha256": sha256(retained_alias_input)},
        })
        write_json(input_path, inputs)
        cohort["builds"][0]["input_manifest"]["sha256"] = sha256(input_path)
        write_json(manifest, cohort)
        with self.assertRaisesRegex(module.SourceEvidenceError, "ancestor|alias|unsafe"):
            module.validate_cohort_inputs(manifest, validation_mode="live")

    def test_snapshot_repository_must_match_unchanged_source_identity(self):
        module = load_module()
        snapshot = self._capture()
        snapshot_path = Path(snapshot["path"])
        value = json.loads(snapshot_path.read_text(encoding="utf-8"))
        foreign = self.root / "foreign"
        foreign.mkdir()
        value["repository"] = str(foreign)
        write_json(snapshot_path, value)
        snapshot["sha256"] = sha256(snapshot_path)
        manifest = self._build_manifest(snapshot)
        with self.assertRaisesRegex(module.SourceEvidenceError, "repository.*source identity"):
            module.validate_cohort_inputs(manifest, validation_mode="live")

    def test_recipe_argv_executable_must_be_the_first_hashed_command_file(self):
        module = load_module()
        manifest = self._build_manifest(self._capture())
        cohort = json.loads(manifest.read_text(encoding="utf-8"))
        cohort["builds"][0]["recipe"]["argv"][0] = "/usr/bin/false"
        write_json(manifest, cohort)
        with self.assertRaisesRegex(module.SourceEvidenceError, "argv.*command file"):
            module.validate_cohort_inputs(manifest, validation_mode="live")

    def test_relative_and_dangling_symlink_exports_are_verified_without_dereference(self):
        for index, target in enumerate(("README.md", "missing-target")):
            with self.subTest(target=target):
                if index:
                    self.tearDown(); self.setUp()
                module = load_module()
                source_link = self.repo / "retained-link"
                source_link.symlink_to(target)
                manifest = self._build_manifest(self._capture("symlink-B"))
                cohort = json.loads(manifest.read_text(encoding="utf-8"))
                input_path = Path(cohort["builds"][0]["input_manifest"]["path"])
                inputs = json.loads(input_path.read_text(encoding="utf-8"))
                source_member = self._member(self.repo, "retained-link")
                inputs["members"].append(source_member)
                export_link = self.root / "export/retained-link"
                export_link.symlink_to(target)
                payload = target.encode("utf-8")
                inputs["exports"].append({
                    "role": "retained_link", "path": str(export_link),
                    "export_root": "fixture_exports",
                    "source_role": "fixture_source", "source_path": "retained-link",
                    "type": "symlink", "mode": source_member["mode"], "bytes": len(payload),
                    "sha256": hashlib.sha256(payload).hexdigest(), "symlink_target": target,
                })
                inputs["export_roots"][0]["members"] = module.inventory_artifact(
                    self.root / "export", "directory"
                )
                write_json(input_path, inputs)
                cohort["builds"][0]["input_manifest"]["sha256"] = sha256(input_path)
                write_json(manifest, cohort)
                self.assertEqual(
                    "fixture-cohort",
                    module.validate_cohort_inputs(manifest, validation_mode="live")["cohort_id"],
                )

    def test_zip_preserves_explicit_zero_mode_and_rejects_unsupported_unix_type(self):
        module = load_module()
        zero_zip = self.root / "zero.zip"
        zero = zipfile.ZipInfo("zero-mode")
        zero.create_system = 3
        zero.external_attr = stat.S_IFREG << 16
        with zipfile.ZipFile(zero_zip, "w") as archive:
            archive.writestr(zero, b"payload")
        self.assertEqual("0000", module.inventory_artifact(zero_zip, "zip")[0]["mode"])

        special_zip = self.root / "special.zip"
        special = zipfile.ZipInfo("named-pipe")
        special.create_system = 3
        special.external_attr = (stat.S_IFIFO | 0o600) << 16
        with zipfile.ZipFile(special_zip, "w") as archive:
            archive.writestr(special, b"")
        with self.assertRaisesRegex(module.SourceEvidenceError, "unsupported"):
            module.inventory_artifact(special_zip, "zip")

        inconsistent_zip = self.root / "directory-without-slash.zip"
        directory = zipfile.ZipInfo("directory-name")
        directory.create_system = 3
        directory.external_attr = (stat.S_IFDIR | 0o755) << 16
        with zipfile.ZipFile(inconsistent_zip, "w") as archive:
            archive.writestr(directory, b"")
        with self.assertRaisesRegex(module.SourceEvidenceError, "directory.*name|unsupported"):
            module.inventory_artifact(inconsistent_zip, "zip")

    def test_public_build_verifier_rejects_malformed_closed_record(self):
        module = load_module()
        with self.assertRaisesRegex(module.SourceEvidenceError, "build record.*schema|schema"):
            module.verify_build_outputs({"id": "incomplete", "extra": True}, source_roots=[])

    def test_members_are_derived_from_exact_role_snapshot_and_role_path_is_unambiguous(self):
        mutations = ("foreign-root", "stale-snapshot", "duplicate-role-path")
        for index, mutation in enumerate(mutations):
            with self.subTest(mutation=mutation):
                if index:
                    self.tearDown(); self.setUp()
                module = load_module()
                manifest = self._build_manifest(self._capture("lineage-B"))
                cohort = json.loads(manifest.read_text(encoding="utf-8"))
                input_path = Path(cohort["builds"][0]["input_manifest"]["path"])
                inputs = json.loads(input_path.read_text(encoding="utf-8"))
                member = next(item for item in inputs["members"] if item["path"] == "retained.txt")
                foreign = self.root / "foreign-root"
                foreign.mkdir()
                (foreign / "retained.txt").write_bytes((self.repo / "retained.txt").read_bytes())
                if mutation == "foreign-root":
                    member["root"] = str(foreign)
                elif mutation == "stale-snapshot":
                    (self.repo / "retained.txt").write_text("new live source\n", encoding="utf-8")
                    refreshed = self._member(self.repo, "retained.txt")
                    member.update(refreshed)
                else:
                    duplicate = dict(member)
                    duplicate["root"] = str(foreign)
                    inputs["members"].append(duplicate)
                write_json(input_path, inputs)
                cohort["builds"][0]["input_manifest"]["sha256"] = sha256(input_path)
                write_json(manifest, cohort)
                with self.assertRaisesRegex(module.SourceEvidenceError, "snapshot|root|ambiguous|duplicated"):
                    module.validate_cohort_inputs(manifest, validation_mode="live")

    def test_retained_mode_uses_snapshot_and_complete_export_tree_not_live_source(self):
        module = load_module()
        manifest = self._build_manifest(self._capture("retained-B"))
        (self.repo / "retained.txt").write_text("permitted later source annotation\n", encoding="utf-8")
        with self.assertRaisesRegex(module.SourceEvidenceError, "input member"):
            module.validate_cohort_inputs(manifest, validation_mode="live")
        self.assertEqual(
            "fixture-cohort",
            module.validate_cohort_inputs(manifest, validation_mode="retained")["cohort_id"],
        )
        (self.root / "export/extra.txt").write_text("undeclared export drift\n", encoding="utf-8")
        with self.assertRaisesRegex(module.SourceEvidenceError, "export root inventory"):
            module.validate_cohort_inputs(manifest, validation_mode="retained")
        (self.root / "export/extra.txt").unlink()
        current_mode = stat.S_IMODE((self.root / "export").stat().st_mode)
        os.chmod(self.root / "export", 0o700 if current_mode != 0o700 else 0o755)
        with self.assertRaisesRegex(module.SourceEvidenceError, "export root mode"):
            module.validate_cohort_inputs(manifest, validation_mode="retained")

    def test_retained_ignored_input_is_independent_and_immutable(self):
        module = load_module()
        manifest = self._build_manifest(self._capture("ignored-B"))
        retained = self.root / "retained-ignored.bin"
        retained.write_bytes(b"tampered retained ignored input")
        with self.assertRaisesRegex(module.SourceEvidenceError, "retained input"):
            module.validate_cohort_inputs(manifest, validation_mode="retained")

    def test_symlink_export_target_type_mode_and_ancestor_drift_fail(self):
        for index, mutation in enumerate(("target", "type", "mode", "ancestor")):
            with self.subTest(mutation=mutation):
                if index:
                    self.tearDown(); self.setUp()
                module = load_module()
                (self.repo / "retained-link").symlink_to("missing-target")
                manifest = self._build_manifest(self._capture("symlink-negative-B"))
                cohort = json.loads(manifest.read_text(encoding="utf-8"))
                input_path = Path(cohort["builds"][0]["input_manifest"]["path"])
                inputs = json.loads(input_path.read_text(encoding="utf-8"))
                source_member = self._member(self.repo, "retained-link")
                inputs["members"].append(source_member)
                export_root = self.root / "export"
                export_path = export_root / "retained-link"
                export_path.symlink_to("missing-target")
                inputs["exports"].append({
                    "role": "retained_link", "path": str(export_path),
                    "export_root": "fixture_exports", "source_role": "fixture_source",
                    "source_path": "retained-link", "type": "symlink",
                    "mode": source_member["mode"], "bytes": source_member["bytes"],
                    "sha256": source_member["sha256"],
                    "symlink_target": source_member["symlink_target"],
                })
                if mutation == "target":
                    export_path.unlink(); export_path.symlink_to("wrong-target")
                elif mutation == "type":
                    export_path.unlink(); export_path.write_text("missing-target", encoding="utf-8")
                elif mutation == "mode":
                    inputs["exports"][-1]["mode"] = "0000"
                else:
                    export_path.unlink()
                    outside = self.root / "outside-export"
                    outside.mkdir()
                    (outside / "retained-link").symlink_to("missing-target")
                    (export_root / "nested").symlink_to(outside, target_is_directory=True)
                    inputs["exports"][-1]["path"] = str(export_root / "nested/retained-link")
                inputs["export_roots"][0]["members"] = module.inventory_artifact(
                    export_root, "directory"
                )
                write_json(input_path, inputs)
                cohort["builds"][0]["input_manifest"]["sha256"] = sha256(input_path)
                write_json(manifest, cohort)
                with self.assertRaisesRegex(module.SourceEvidenceError, "export|ancestor|type|mode|target"):
                    module.validate_cohort_inputs(manifest, validation_mode="retained")

    def test_retained_input_cannot_be_source_contained_and_modes_are_typed(self):
        module = load_module()
        manifest = self._build_manifest(self._capture("retained-boundary-B"))
        cohort = json.loads(manifest.read_text(encoding="utf-8"))
        input_path = Path(cohort["builds"][0]["input_manifest"]["path"])
        inputs = json.loads(input_path.read_text(encoding="utf-8"))
        ignored = next(item for item in inputs["members"] if item["git_ignored"])
        ignored["retained_input"] = {
            "path": str(self.repo / "vendor-input.bin"),
            "sha256": sha256(self.repo / "vendor-input.bin"),
        }
        write_json(input_path, inputs)
        cohort["builds"][0]["input_manifest"]["sha256"] = sha256(input_path)
        write_json(manifest, cohort)
        with self.assertRaisesRegex(module.SourceEvidenceError, "retained input.*outside"):
            module.validate_cohort_inputs(manifest, validation_mode="retained")

        self.tearDown(); self.setUp(); module = load_module()
        manifest = self._build_manifest(self._capture("typed-member-B"))
        cohort = json.loads(manifest.read_text(encoding="utf-8"))
        input_path = Path(cohort["builds"][0]["input_manifest"]["path"])
        inputs = json.loads(input_path.read_text(encoding="utf-8"))
        inputs["members"][0]["bytes"] = True
        write_json(input_path, inputs)
        cohort["builds"][0]["input_manifest"]["sha256"] = sha256(input_path)
        write_json(manifest, cohort)
        with self.assertRaisesRegex(module.SourceEvidenceError, "schema"):
            module.validate_cohort_inputs(manifest, validation_mode="retained")

        with self.assertRaisesRegex(module.SourceEvidenceError, "validation mode"):
            module.validate_cohort_inputs(manifest, validation_mode=True)

    def test_snapshot_evidence_cannot_be_contained_by_a_build_output_root(self):
        module = load_module()
        manifest = self._build_manifest(self._capture("protected-snapshot-B"))
        cohort = json.loads(manifest.read_text(encoding="utf-8"))
        output_root = self.root / "directory-output"
        output_root.mkdir()
        embedded_snapshot = output_root / "snapshot.json"
        original_snapshot = Path(cohort["repository_observations"][0]["snapshot"]["path"])
        embedded_snapshot.write_bytes(original_snapshot.read_bytes())
        cohort["repository_observations"][0]["snapshot"] = {
            "path": str(embedded_snapshot), "sha256": sha256(embedded_snapshot)
        }
        inventory = module.inventory_artifact(output_root, "directory")
        inventory_bytes = json.dumps(
            inventory, sort_keys=True, separators=(",", ":")
        ).encode("utf-8")
        cohort["builds"][0]["outputs"] = [{
            "role": "directory_artifact", "path": str(output_root), "type": "directory",
            "sha256": hashlib.sha256(inventory_bytes).hexdigest(),
            "embedded_version": "2026.36.2", "members": inventory,
        }]
        write_json(manifest, cohort)
        with self.assertRaisesRegex(module.SourceEvidenceError, "snapshot.*protected|evidence.*output"):
            module.validate_cohort_inputs(manifest, validation_mode="retained")

    def test_retained_inputs_are_checked_against_other_build_export_and_output_roots(self):
        for index, boundary in enumerate(("output", "export")):
            with self.subTest(boundary=boundary):
                if index:
                    self.tearDown(); self.setUp()
                module = load_module()
                (self.repo / "vendor-input.bin").write_bytes(b"retained source\n")
                manifest = self._build_manifest(self._capture("cross-build-B"))
                cohort = json.loads(manifest.read_text(encoding="utf-8"))
                build_one = cohort["builds"][0]
                input_one_path = Path(build_one["input_manifest"]["path"])
                input_one = json.loads(input_one_path.read_text(encoding="utf-8"))
                retained_member = next(
                    item for item in input_one["members"] if item["path"] == "retained.txt"
                )
                build_two_input = {
                    "schema_version": 2, "build_id": "fixture-build-two",
                    "members": [retained_member], "export_roots": [], "exports": [],
                }
                build_two = json.loads(json.dumps(build_one))
                build_two["id"] = "fixture-build-two"
                if boundary == "output":
                    output_root = self.root / "build-two-output"
                    output_root.mkdir()
                    cross_path = output_root / "saved-ignored.bin"
                    cross_path.write_bytes(b"retained source\n")
                    output_inventory = module.inventory_artifact(output_root, "directory")
                    output_digest = hashlib.sha256(json.dumps(
                        output_inventory, sort_keys=True, separators=(",", ":")
                    ).encode("utf-8")).hexdigest()
                    build_two["outputs"] = [{
                        "role": "fixture_artifact_two", "path": str(output_root),
                        "type": "directory", "sha256": output_digest,
                        "embedded_version": "2026.36.2", "members": output_inventory,
                    }]
                else:
                    export_root = self.root / "build-two-export"
                    export_root.mkdir()
                    cross_path = export_root / "retained.txt"
                    cross_path.write_bytes((self.repo / "retained.txt").read_bytes())
                    export_mode = f"{stat.S_IMODE(export_root.stat().st_mode):04o}"
                    build_two_input["export_roots"] = [{
                        "role": "fixture_exports_two", "path": str(export_root),
                        "mode": export_mode,
                        "members": module.inventory_artifact(export_root, "directory"),
                    }]
                    build_two_input["exports"] = [{
                        "role": "retained_export_two", "path": str(cross_path),
                        "export_root": "fixture_exports_two",
                        "source_role": "fixture_source", "source_path": "retained.txt",
                        "type": retained_member["type"], "mode": retained_member["mode"],
                        "bytes": retained_member["bytes"], "sha256": retained_member["sha256"],
                        "symlink_target": retained_member["symlink_target"],
                    }]
                    output = self.root / "artifact-two.tar"
                    shutil.copyfile(build_one["outputs"][0]["path"], output)
                    build_two["outputs"][0]["path"] = str(output)
                    build_two["outputs"][0]["sha256"] = sha256(output)
                ignored = next(item for item in input_one["members"] if item["git_ignored"])
                ignored["retained_input"] = {"path": str(cross_path), "sha256": sha256(cross_path)}
                write_json(input_one_path, input_one)
                build_one["input_manifest"]["sha256"] = sha256(input_one_path)
                build_two_input_path = write_json(self.root / "build-inputs-two.json", build_two_input)
                build_two["input_manifest"] = {
                    "path": str(build_two_input_path), "sha256": sha256(build_two_input_path)
                }
                cohort["builds"].append(build_two)
                write_json(manifest, cohort)
                with self.assertRaisesRegex(module.SourceEvidenceError, "retained input.*protected|evidence.*root"):
                    module.validate_cohort_inputs(manifest, validation_mode="retained")


if __name__ == "__main__":
    unittest.main()
