#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for external finite evidence finalization."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest


HERE = Path(__file__).resolve().parent
SOURCE_MODULE = HERE / "source_evidence.py"
FINAL_MODULE = HERE / "finalize_evidence.py"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path: Path, value: object) -> Path:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


def git(repo: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(repo), *args], check=True, text=True, stdout=subprocess.PIPE).stdout.strip()


class FinalizationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        os.chmod(self.root, 0o700)
        self.ignored_refresh_index = 0
        self.repo = self.root / "hub-source"
        self.repo.mkdir()
        git(self.repo, "init", "-q")
        git(self.repo, "config", "user.email", "fixture@example.invalid")
        git(self.repo, "config", "user.name", "Fixture")
        docs = self.repo / "docs/compatibility"
        docs.mkdir(parents=True)
        self.execution_before = {
            "schema_version": 1, "status": "active",
            "tasks": {"10": "pending", "11": "pending", "12": "pending", "10a": "pending"},
            "platforms": {"macos_arm64": "frozen"},
            "evidence": {"final_cohort": None}, "publication_performed": False,
        }
        self.release_before = {
            "schema_version": 1, "status": "candidate", "product_version": "2026.36.2",
            "source_tag": {"status": "not_created"}, "test_receipt_paths": [],
        }
        write_json(docs / "execution-state.json", self.execution_before)
        write_json(docs / "ecosystem-release.json", self.release_before)
        (docs / "acceptance.md").write_text("Pending final evidence.\n", encoding="utf-8")
        write_json(docs / "receipt-index.json", {"schema_version": 1, "cohort_id": "fixture-cohort", "receipts": []})
        (self.repo / "Cargo.lock").write_text("locked\n", encoding="utf-8")
        (self.repo / ".gitignore").write_text("vendor-input.bin\n", encoding="utf-8")
        (self.repo / "vendor-input.bin").write_bytes(b"ignored B input\n")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-qm", "B placeholder")

        self.source = load(SOURCE_MODULE, "final_source_evidence")
        self.final = load(FINAL_MODULE, "task10_finalize_evidence") if FINAL_MODULE.exists() else None
        self.before = self.source.capture_repository_snapshot(
            self.repo, self.root / "snapshot-B", role="hub_source", observer=self.observer
        )
        self.receipts = []
        self.validation = {}
        for kind, scope, schema, status, gates in (
            ("matrix", "local-tested-cohort-not-published", "hub-compatibility-receipt-v2", "passed", ["21-cells", "cleanup"]),
            ("package", "local-built-not-published", "hub-package-receipt-v1", "passed", ["macos", "debian-amd64", "debian-arm64"]),
            ("bootstrap", "local-source-candidate", "hub-bootstrap-receipt-v2", "passed", ["six-companions"]),
            ("upgrade", "isolated-upgrade", "hub-upgrade-receipt-v1", "passed", ["hub", "mixed-version"]),
            ("private-lane", "authorized-private-lane", "hub-private-lane-receipt-v1", "pending", ["task11-explicit-pending"]),
        ):
            path = self.root / f"{kind}.evidence"
            path.write_text(f"{kind} evidence\n", encoding="utf-8")
            binding = {"kind": kind, "path": str(path), "sha256": digest(path), "scope": scope}
            self.receipts.append(binding)
            self.validation[str(path)] = {
                "schema_version": 2, "kind": kind, "path": str(path), "sha256": digest(path),
                "cohort_id": "fixture-cohort", "scope": scope, "status": status,
                "artifact_schema": schema, "gate_ids": gates, "accepted_review_verdict": None,
            }
        historical_inputs = write_json(self.root / "historical-A-inputs.json", {
            "schema_version": 1, "cohort_id": "fixture-historical-A", "status": "retained"
        })
        self.historical_inputs = {"path": str(historical_inputs), "sha256": digest(historical_inputs)}
        historical = self.root / "historical.evidence"
        historical.write_text("historical A evidence\n", encoding="utf-8")
        historical_receipt = {
            "kind": "historical", "path": str(historical), "sha256": digest(historical),
            "scope": "historical-local-evidence",
        }
        self.receipts.append(historical_receipt)
        self.validation[str(historical)] = {
            "schema_version": 2, "kind": "historical", "path": str(historical),
            "sha256": digest(historical), "cohort_id": "fixture-cohort",
            "scope": "historical-local-evidence", "status": "passed",
            "artifact_schema": "historical-evidence-v1", "gate_ids": [],
            "accepted_review_verdict": None,
        }
        review = self.root / "review.md"
        review.write_text("Verdict: accepted\n", encoding="utf-8")
        self.review = {"kind": "review", "path": str(review), "sha256": digest(review), "scope": "independent-review"}
        self.validation[str(review)] = {
            "schema_version": 2, "kind": "review", "path": str(review), "sha256": digest(review),
            "cohort_id": "fixture-cohort", "scope": "independent-review", "status": "passed",
            "artifact_schema": "markdown-independent-review-v1", "gate_ids": ["final-contracts"],
            "accepted_review_verdict": True,
        }
        report = self.root / "final-report.md"
        report.write_text("External report\n", encoding="utf-8")
        self.report = {"path": str(report), "sha256": digest(report)}
        cohort = self._make_cohort_inputs()
        self.cohort = {"path": str(cohort), "sha256": digest(cohort)}
        self.source.validate_cohort_inputs(cohort, validation_mode="live")
        for result in self.validation.values():
            if result["kind"] == "historical":
                result["tested_cohort_inputs"] = None
                result["historical_tested_inputs"] = dict(self.historical_inputs)
            else:
                result["tested_cohort_inputs"] = dict(self.cohort)
                result["historical_tested_inputs"] = None
        self.required_gates = {
            "matrix": ["21-cells", "cleanup"],
            "package": ["macos", "debian-amd64", "debian-arm64"],
            "bootstrap": ["six-companions"],
            "upgrade": ["hub", "mixed-version"],
            "private-lane": ["task11-explicit-pending"],
            "review": ["final-contracts"],
        }

    def tearDown(self):
        self.temporary.cleanup()

    def observer(self, repo: Path, role: str) -> dict[str, object]:
        patch = subprocess.run(["git", "-C", str(repo), "diff", "--binary", "HEAD", "--"], check=True, stdout=subprocess.PIPE).stdout
        return {
            "role": role, "repo": str(repo), "head": git(repo, "rev-parse", "HEAD"),
            "dirty_patch_sha256": hashlib.sha256(patch).hexdigest(),
            "untracked_source_manifest_sha256": hashlib.sha256().hexdigest(),
        }

    def validator(
        self, binding: dict[str, str], raw: bytes, context: dict[str, object]
    ) -> dict[str, object]:
        del raw
        self.assertEqual("fixture-cohort", context["cohort_id"])
        self.assertEqual(self.cohort, context["cohort_inputs"])
        return dict(self.validation[binding["path"]])

    def _make_cohort_inputs(self) -> Path:
        snapshot = json.loads(Path(self.before["path"]).read_text(encoding="utf-8"))
        snapshot_members = {item["path"]: item for item in snapshot["members"]}
        retained_paths = [
            "Cargo.lock", "docs/compatibility/execution-state.json",
            "docs/compatibility/ecosystem-release.json",
            "docs/compatibility/acceptance.md",
            "docs/compatibility/receipt-index.json",
        ]
        members = []
        export_root = self.root / "retained-B-exports"
        export_root.mkdir()
        exports = []
        for relative in retained_paths:
            member = snapshot_members[relative]
            members.append({
                "role": "hub_source", "root": str(self.repo), "path": relative,
                "type": member["type"], "mode": member["mode"], "bytes": member["bytes"],
                "sha256": member["sha256"], "symlink_target": member["symlink_target"],
                "git_ignored": False, "retained_input": None,
            })
            destination = export_root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(self.repo / relative, destination)
            os.chmod(destination, int(member["mode"], 8))
            exports.append({
                "role": "retained_B_" + relative.replace("/", "_"),
                "path": str(destination), "export_root": "retained_B_exports",
                "source_role": "hub_source", "source_path": relative,
                "type": member["type"], "mode": member["mode"], "bytes": member["bytes"],
                "sha256": member["sha256"], "symlink_target": member["symlink_target"],
            })
        ignored_path = self.repo / "vendor-input.bin"
        ignored_payload = ignored_path.read_bytes()
        ignored_mode = f"{stat.S_IMODE(ignored_path.stat().st_mode):04o}"
        retained_ignored = self.root / "retained-B-ignored.bin"
        retained_ignored.write_bytes(ignored_payload)
        ignored_member = {
            "role": "hub_source", "root": str(self.repo), "path": "vendor-input.bin",
            "type": "file", "mode": ignored_mode, "bytes": len(ignored_payload),
            "sha256": hashlib.sha256(ignored_payload).hexdigest(), "symlink_target": None,
            "git_ignored": True,
            "retained_input": {"path": str(retained_ignored), "sha256": digest(retained_ignored)},
        }
        members.append(ignored_member)
        ignored_export = export_root / "vendor-input.bin"
        ignored_export.write_bytes(ignored_payload)
        os.chmod(ignored_export, int(ignored_mode, 8))
        exports.append({
            "role": "retained_B_vendor_input", "path": str(ignored_export),
            "export_root": "retained_B_exports", "source_role": "hub_source",
            "source_path": "vendor-input.bin", "type": "file", "mode": ignored_mode,
            "bytes": len(ignored_payload), "sha256": hashlib.sha256(ignored_payload).hexdigest(),
            "symlink_target": None,
        })
        input_manifest = write_json(self.root / "build-inputs.json", {
            "schema_version": 2, "build_id": "hub-build", "members": members,
            "export_roots": [{
                "role": "retained_B_exports", "path": str(export_root),
                "mode": f"{stat.S_IMODE(export_root.stat().st_mode):04o}",
                "members": self.source.inventory_artifact(export_root, "directory"),
            }],
            "exports": exports,
        })
        tool = self.root / "compiler"
        tool.write_text("tool\n", encoding="utf-8")
        os.chmod(tool, 0o755)
        log = self.root / "build.log"
        log.write_text("build\n", encoding="utf-8")
        evidence = write_json(self.root / "toolchain.json", {"tool": "fixture"})
        artifact = self.root / "hub-artifact"
        artifact.write_text("2026.36.2\n", encoding="utf-8")
        build = {
            "id": "hub-build", "source_roles": ["hub_source"],
            "input_manifest": {"path": str(input_manifest), "sha256": digest(input_manifest)},
            "recipe": {"argv": [str(tool)], "cwd": str(self.repo),
                       "command_files": [{"path": str(tool), "sha256": digest(tool)}],
                       "log": {"path": str(log), "sha256": digest(log)}},
            "toolchain": {
                "host": {"os": "synthetic", "architecture": "arm64", "native_or_emulated": "native"},
                "tools": [{"name": "compiler", "path": str(tool), "version": "1", "sha256": digest(tool)}],
                "dependencies": [], "evidence": [{"path": str(evidence), "sha256": digest(evidence)}],
            },
            "outputs": [{"role": "hub_artifact", "path": str(artifact), "type": "file",
                         "sha256": digest(artifact), "embedded_version": "2026.36.2",
                         "members": self.source.inventory_artifact(artifact, "file")}],
        }
        return write_json(self.root / "cohort-inputs.json", {
            "schema_version": 2, "cohort_id": "fixture-cohort", "product_version": "2026.36.2",
            "repository_observations": [{"source_identity": self.observer(self.repo, "hub_source"), "snapshot": self.before}],
            "builds": [build],
        })

    def _document(self, ignored_mutation: str | None = None) -> tuple[Path, dict[str, object]]:
        assert self.final is not None
        final_cohort = {
            "cohort_id": "fixture-cohort", "cohort_inputs": self.cohort,
            "matrix_receipt": {key: self.receipts[0][key] for key in ("path", "sha256")},
            "upgrade_receipts": [{key: self.receipts[3][key] for key in ("path", "sha256")}],
            "review_receipts": [{key: self.review[key] for key in ("path", "sha256")}],
            "acceptance_scope": "local-tested-cohort-not-published",
        }
        execution = json.loads(json.dumps(self.execution_before))
        execution["status"] = "complete"
        execution["tasks"].update({"10": "complete", "11": "pending", "12": "complete", "10a": "complete"})
        execution["evidence"]["final_cohort"] = final_cohort
        release = json.loads(json.dumps(self.release_before))
        release["test_receipt_paths"] = sorted(item["path"] for item in self.receipts)
        docs = self.repo / "docs/compatibility"
        write_json(docs / "execution-state.json", execution)
        write_json(docs / "ecosystem-release.json", release)
        acceptance = self.final.render_acceptance(
            cohort_id="fixture-cohort", cohort_inputs=self.cohort,
            final_receipts=self.receipts, review_receipts=[self.review],
            status="passed", publication_performed=False,
        )
        (docs / "acceptance.md").write_bytes(acceptance)
        index = {
            "schema_version": 1, "cohort_id": "fixture-cohort",
            "receipts": sorted(self.receipts + [self.review], key=lambda item: (item["kind"], item["path"])),
        }
        write_json(docs / "receipt-index.json", index)
        ignored_path = self.repo / "vendor-input.bin"
        if ignored_mutation == "bytes":
            ignored_path.write_bytes(b"changed C ignored input\n")
        elif ignored_mutation == "mode":
            current = stat.S_IMODE(ignored_path.stat().st_mode)
            os.chmod(ignored_path, 0o700 if current != 0o700 else 0o600)
        elif ignored_mutation == "deleted":
            ignored_path.unlink()
        elif ignored_mutation == "symlink":
            ignored_path.unlink()
            ignored_path.symlink_to("different-ignored-target")
        after = self.source.capture_repository_snapshot(
            self.repo, self.root / "snapshot-C", role="hub_source", observer=self.observer
        )
        ignored_after = self.source.capture_ignored_input_observation(
            self.cohort, [{"repo": "hub_source", **after}], self.root / "ignored-inputs-C"
        )
        before_sha = {name: hashlib.sha256((self.repo / name).read_bytes()).hexdigest() for name in []}
        changes = []
        before_snapshot = json.loads(Path(self.before["path"]).read_text(encoding="utf-8"))
        after_snapshot = json.loads(Path(after["path"]).read_text(encoding="utf-8"))
        before_members = {item["path"]: item for item in before_snapshot["members"]}
        after_members = {item["path"]: item for item in after_snapshot["members"]}
        pointers = {
            "docs/compatibility/execution-state.json": ["/evidence/final_cohort", "/status", "/tasks/10", "/tasks/10a", "/tasks/12"],
            "docs/compatibility/ecosystem-release.json": ["/test_receipt_paths"],
            "docs/compatibility/acceptance.md": [],
            "docs/compatibility/receipt-index.json": ["/receipts"],
        }
        empty = hashlib.sha256(b"").hexdigest()
        for path, json_pointers in pointers.items():
            before_member = before_members.get(path)
            after_member = after_members[path]
            changes.append({
                "repo": "hub_source", "path": path,
                "before_sha256": before_member["sha256"] if before_member else empty,
                "after_sha256": after_member["sha256"],
                "json_pointers": json_pointers,
                "classification": "final-evidence-annotation",
            })
        document = {
            "schema_version": 3, "cohort_id": "fixture-cohort", "cohort_inputs": self.cohort,
            "final_receipts": self.receipts,
            "repository_before": [{"repo": "hub_source", **self.before}],
            "repository_after": [{"repo": "hub_source", **after}],
            "ignored_inputs_after": ignored_after,
            "changes": changes, "report_files": [self.report], "review_receipts": [self.review],
            "publication_performed": False, "status": "passed",
        }
        path = write_json(self.root / "finalization.json", document)
        return path, document

    @staticmethod
    def _refresh_after_digest(document: dict[str, object], after: dict[str, str], relative: str) -> None:
        snapshot = json.loads(Path(after["path"]).read_text(encoding="utf-8"))
        member = next(item for item in snapshot["members"] if item["path"] == relative)
        change = next(item for item in document["changes"] if item["path"] == relative)
        change["after_sha256"] = member["sha256"]

    def _refresh_ignored_after(self, document: dict[str, object]) -> None:
        self.ignored_refresh_index += 1
        document["ignored_inputs_after"] = self.source.capture_ignored_input_observation(
            self.cohort,
            document["repository_after"],
            self.root / f"ignored-inputs-C-refresh-{self.ignored_refresh_index}",
        )

    def test_module_contract_exists(self):
        self.assertTrue(FINAL_MODULE.is_file(), "finalize_evidence.py is not implemented")

    def test_validates_exact_delta_receipts_and_deterministic_rendering(self):
        path, _ = self._document()
        result = self.final.validate_finalization(
            path, receipt_validator=self.validator, required_gates=self.required_gates
        )
        self.assertEqual("passed", result["status"])
        self.assertEqual(7, len(result["validated_receipts"]))

    def test_ignored_input_is_independently_observed_at_C_and_cannot_drift(self):
        for index, mutation in enumerate(("bytes", "mode", "deleted", "symlink")):
            with self.subTest(mutation=mutation):
                if index:
                    self.tearDown(); self.setUp()
                path, _document = self._document(ignored_mutation=mutation)
                with self.assertRaisesRegex(self.final.FinalizationError, "ignored input.*changed|C ignored"):
                    self.final.validate_finalization(
                        path, receipt_validator=self.validator, required_gates=self.required_gates
                    )

    def test_receipts_bind_exact_selected_inputs_and_historical_A_binding(self):
        path, document = self._document()
        other_path = self.root / "other-cohort-inputs.json"
        other_path.write_bytes(Path(self.cohort["path"]).read_bytes())
        self.validation[self.receipts[0]["path"]]["tested_cohort_inputs"] = {
            "path": str(other_path), "sha256": digest(other_path)
        }
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "tested.*input|cohort input binding"):
            self.final.validate_finalization(
                path, receipt_validator=self.validator, required_gates=self.required_gates
            )

        self.tearDown(); self.setUp(); path, document = self._document()
        Path(self.historical_inputs["path"]).write_text("tampered A inputs\n", encoding="utf-8")
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "historical.*input.*digest"):
            self.final.validate_finalization(
                path, receipt_validator=self.validator, required_gates=self.required_gates
            )

    def test_exact_cohort_snapshot_binding_cannot_be_substituted_by_same_identity(self):
        os.chmod(self.repo / "Cargo.lock", 0o600)
        substituted = self.source.capture_repository_snapshot(
            self.repo, self.root / "substituted-B", role="hub_source", observer=self.observer
        )
        path, document = self._document()
        document["repository_before"] = [{"repo": "hub_source", **substituted}]
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "exact cohort.*snapshot|snapshot binding"):
            self.final.validate_finalization(
                path, receipt_validator=self.validator, required_gates=self.required_gates
            )

    def test_evidence_and_final_files_are_outside_source_export_and_output_roots(self):
        path, document = self._document()
        inside_receipt = self.repo / "inside-receipt.evidence"
        inside_receipt.write_text("matrix evidence\n", encoding="utf-8")
        old_path = self.receipts[0]["path"]
        inside_binding = {
            **self.receipts[0], "path": str(inside_receipt), "sha256": digest(inside_receipt)
        }
        document["final_receipts"][0] = inside_binding
        result = dict(self.validation[old_path])
        result.update({"path": str(inside_receipt), "sha256": digest(inside_receipt)})
        self.validation[str(inside_receipt)] = result
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "receipt.*outside|protected.*root|source root"):
            self.final.validate_finalization(
                path, receipt_validator=self.validator, required_gates=self.required_gates
            )

        self.tearDown(); self.setUp(); path, document = self._document()
        cohort = json.loads(Path(self.cohort["path"]).read_text(encoding="utf-8"))
        inputs = json.loads(Path(cohort["builds"][0]["input_manifest"]["path"]).read_text(encoding="utf-8"))
        export_root = Path(inputs["export_roots"][0]["path"])
        inside_report = export_root / "nested-report.md"
        inside_report.write_text("inside retained exports\n", encoding="utf-8")
        document["report_files"] = [{"path": str(inside_report), "sha256": digest(inside_report)}]
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "export root|protected.*root"):
            self.final.validate_finalization(
                path, receipt_validator=self.validator, required_gates=self.required_gates
            )

        self.tearDown(); self.setUp(); _path, document = self._document()
        cohort = json.loads(Path(self.cohort["path"]).read_text(encoding="utf-8"))
        inputs = json.loads(Path(cohort["builds"][0]["input_manifest"]["path"]).read_text(encoding="utf-8"))
        inside_final = Path(inputs["export_roots"][0]["path"]) / "finalization.json"
        write_json(inside_final, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "export root|protected.*root"):
            self.final.validate_finalization(
                inside_final, receipt_validator=self.validator, required_gates=self.required_gates
            )

        self.tearDown(); self.setUp(); path, document = self._document()
        alias = self.root / "source-alias"
        alias.symlink_to(self.repo, target_is_directory=True)
        aliased_receipt = alias / "aliased-receipt.evidence"
        (self.repo / "aliased-receipt.evidence").write_text("matrix evidence\n", encoding="utf-8")
        document["final_receipts"][0]["path"] = str(aliased_receipt)
        document["final_receipts"][0]["sha256"] = digest(self.repo / "aliased-receipt.evidence")
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "alias|source root"):
            self.final.validate_finalization(
                path, receipt_validator=self.validator, required_gates=self.required_gates
            )

    def test_C_snapshot_dependencies_cannot_point_into_retained_export_tree(self):
        path, document = self._document()
        cohort = json.loads(Path(self.cohort["path"]).read_text(encoding="utf-8"))
        input_manifest = json.loads(Path(
            cohort["builds"][0]["input_manifest"]["path"]
        ).read_text(encoding="utf-8"))
        cargo_export = next(
            item for item in input_manifest["exports"] if item["source_path"] == "Cargo.lock"
        )
        after_binding = document["repository_after"][0]
        after_path = Path(after_binding["path"])
        after_snapshot = json.loads(after_path.read_text(encoding="utf-8"))
        cargo_member = next(
            item for item in after_snapshot["members"] if item["path"] == "Cargo.lock"
        )
        cargo_member["blob"] = cargo_export["path"]
        write_json(after_path, after_snapshot)
        after_binding["sha256"] = digest(after_path)
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "C snapshot.*protected|snapshot.*export"):
            self.final.validate_finalization(
                path, receipt_validator=self.validator, required_gates=self.required_gates
            )

    def test_no_validator_arbitrary_result_future_receipt_and_v1_matrix_fail_closed(self):
        path, document = self._document()
        with self.assertRaisesRegex(self.final.FinalizationError, "receipt validator"):
            self.final.validate_finalization(path, receipt_validator=None, required_gates=self.required_gates)
        with self.assertRaisesRegex(self.final.FinalizationError, "validation result"):
            self.final.validate_finalization(path, receipt_validator=lambda *_: True, required_gates=self.required_gates)

        document["final_receipts"][0]["path"] = str(self.root / "future.json")
        future = write_json(self.root / "future-finalization.json", document)
        with self.assertRaisesRegex(self.final.FinalizationError, "missing|regular"):
            self.final.validate_finalization(future, receipt_validator=self.validator, required_gates=self.required_gates)

        self.tearDown(); self.setUp()
        _, document = self._document()
        self.validation[self.receipts[0]["path"]]["artifact_schema"] = "hub-compatibility-receipt-v1"
        old = write_json(self.root / "old-matrix-finalization.json", document)
        with self.assertRaisesRegex(self.final.FinalizationError, "matrix.*v2"):
            self.final.validate_finalization(old, receipt_validator=self.validator, required_gates=self.required_gates)

    def test_nonallowlisted_lock_platform_extra_leaf_mode_and_symlink_changes_fail(self):
        mutations = ("lock", "platform", "extra", "mode", "symlink")
        for index, mutation in enumerate(mutations):
            with self.subTest(mutation=mutation):
                if index:
                    self.tearDown(); self.setUp()
                path, document = self._document()
                docs = self.repo / "docs/compatibility"
                if mutation == "lock":
                    (self.repo / "Cargo.lock").write_text("changed after R\n", encoding="utf-8")
                elif mutation == "platform":
                    value = json.loads((docs / "execution-state.json").read_text())
                    value["platforms"]["macos_arm64"] = "changed"
                    write_json(docs / "execution-state.json", value)
                elif mutation == "extra":
                    value = json.loads((docs / "ecosystem-release.json").read_text())
                    value["extra"] = 1
                    write_json(docs / "ecosystem-release.json", value)
                elif mutation == "mode":
                    os.chmod(docs / "receipt-index.json", 0o755)
                else:
                    target = docs / "acceptance-real.md"
                    target.write_bytes((docs / "acceptance.md").read_bytes())
                    (docs / "acceptance.md").unlink()
                    os.symlink("acceptance-real.md", docs / "acceptance.md")
                after = self.source.capture_repository_snapshot(
                    self.repo, self.root / f"mutated-{mutation}", role="hub_source", observer=self.observer
                )
                document["repository_after"] = [{"repo": "hub_source", **after}]
                self._refresh_ignored_after(document)
                if mutation in {"platform", "extra"}:
                    relative = "docs/compatibility/execution-state.json" if mutation == "platform" else "docs/compatibility/ecosystem-release.json"
                    self._refresh_after_digest(document, after, relative)
                write_json(path, document)
                with self.assertRaisesRegex(self.final.FinalizationError, "allowlist|platform|unknown|mode|symlink|changes|cohort inputs|input member"):
                    self.final.validate_finalization(path, receipt_validator=self.validator, required_gates=self.required_gates)

    def test_duplicate_json_bool_int_old_digest_acceptance_tamper_and_self_reference_fail(self):
        path, document = self._document()
        docs = self.repo / "docs/compatibility"
        (docs / "execution-state.json").write_text('{"status":"complete","status":"complete"}\n', encoding="utf-8")
        after = self.source.capture_repository_snapshot(self.repo, self.root / "duplicate-C", role="hub_source", observer=self.observer)
        document["repository_after"] = [{"repo": "hub_source", **after}]
        self._refresh_ignored_after(document)
        self._refresh_after_digest(document, after, "docs/compatibility/execution-state.json")
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "duplicate JSON"):
            self.final.validate_finalization(path, receipt_validator=self.validator, required_gates=self.required_gates)

        self.tearDown(); self.setUp(); path, document = self._document()
        document["schema_version"] = True
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "schema"):
            self.final.validate_finalization(path, receipt_validator=self.validator, required_gates=self.required_gates)

        self.tearDown(); self.setUp(); path, document = self._document()
        document["changes"][0]["after_sha256"] = "a" * 64
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "change digest"):
            self.final.validate_finalization(path, receipt_validator=self.validator, required_gates=self.required_gates)

        self.tearDown(); self.setUp(); path, document = self._document()
        docs = self.repo / "docs/compatibility"
        (docs / "acceptance.md").write_text("fabricated acceptance\n", encoding="utf-8")
        after = self.source.capture_repository_snapshot(self.repo, self.root / "tampered-C", role="hub_source", observer=self.observer)
        document["repository_after"] = [{"repo": "hub_source", **after}]
        self._refresh_ignored_after(document)
        self._refresh_after_digest(document, after, "docs/compatibility/acceptance.md")
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "acceptance rendering"):
            self.final.validate_finalization(path, receipt_validator=self.validator, required_gates=self.required_gates)

        self.tearDown(); self.setUp(); path, document = self._document()
        document["report_files"].append({"path": str(path), "sha256": digest(path)})
        write_json(path, document)
        with self.assertRaisesRegex(self.final.FinalizationError, "self-referential"):
            self.final.validate_finalization(path, receipt_validator=self.validator, required_gates=self.required_gates)

        self.tearDown(); self.setUp(); _path, document = self._document()
        inside = write_json(self.repo / "embedded-finalization.json", document)
        with self.assertRaisesRegex(self.final.FinalizationError, "protected.*source|source.*root"):
            self.final.validate_finalization(inside, receipt_validator=self.validator, required_gates=self.required_gates)

    def test_nested_array_number_type_changes_are_not_equal(self):
        for index, replacement in enumerate((True, 1.0)):
            with self.subTest(replacement=replacement):
                if index:
                    self.tearDown(); self.setUp()
                self.execution_before["platforms"]["typed_array"] = [1]
                write_json(self.repo / "docs/compatibility/execution-state.json", self.execution_before)
                self.before = self.source.capture_repository_snapshot(
                    self.repo, self.root / "typed-array-B", role="hub_source", observer=self.observer
                )
                shutil.rmtree(self.root / "retained-B-exports")
                self.cohort = {"path": str(self._make_cohort_inputs()), "sha256": ""}
                self.cohort["sha256"] = digest(Path(self.cohort["path"]))
                for result in self.validation.values():
                    if result["kind"] != "historical":
                        result["tested_cohort_inputs"] = dict(self.cohort)
                path, document = self._document()
                execution_path = self.repo / "docs/compatibility/execution-state.json"
                execution = json.loads(execution_path.read_text(encoding="utf-8"))
                execution["platforms"]["typed_array"] = [replacement]
                write_json(execution_path, execution)
                after = self.source.capture_repository_snapshot(
                    self.repo, self.root / "typed-array-C", role="hub_source", observer=self.observer
                )
                document["repository_after"] = [{"repo": "hub_source", **after}]
                self._refresh_ignored_after(document)
                self._refresh_after_digest(document, after, "docs/compatibility/execution-state.json")
                write_json(path, document)
                with self.assertRaisesRegex(self.final.FinalizationError, "platform|unknown JSON"):
                    self.final.validate_finalization(
                        path, receipt_validator=self.validator, required_gates=self.required_gates
                    )


if __name__ == "__main__":
    unittest.main()
