# SPDX-License-Identifier: AGPL-3.0-only
from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from jsonschema import Draft202012Validator

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from companions.core import (  # noqa: E402
    BootstrapError,
    CatalogError,
    PrefixLock,
    active_release_id,
    fetch_git_source,
    install_cohort,
    parse_catalog,
    remove,
    rollback,
    select_cohort,
    source_manifest_for,
    status,
    verify_local_source,
)
from companions.recipes import RecipeContext  # noqa: E402
from companions.targets import activate_targets  # noqa: E402

PROFILE_SHA = "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"
COMMITS = {
    "protocol": "1" * 40,
    "sdk-typescript": "2" * 40,
    "sdk-swift": "3" * 40,
    "home-assistant": "4" * 40,
    "edge": "5" * 40,
}
REPOSITORIES = {
    "protocol": "https://github.com/magrathean-uk/teslatlas-protocol.git",
    "sdk-typescript": "https://github.com/magrathean-uk/teslatlas-sdk-typescript.git",
    "sdk-swift": "https://github.com/magrathean-uk/teslatlas-sdk-swift.git",
    "home-assistant": "https://github.com/magrathean-uk/teslatlas-home-assistant.git",
    "edge": "https://github.com/magrathean-uk/teslatlas-edge.git",
}


def component(name: str, version: str, *, digest: str | None = None) -> dict:
    value = {
        "repository": REPOSITORIES[name],
        "commit": COMMITS[name],
        "source_sha256": digest or (str(tuple(REPOSITORIES).index(name) + 1) * 64),
        "product_version": version,
        "profile": (
            {
                "id": "edge-delivery-v2",
                "revision": "2.0.0",
                "sha256": "e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224",
            }
            if name == "edge"
            else {"id": "hub-http-v1", "revision": "1.0.0", "sha256": PROFILE_SHA}
        ),
    }
    if name == "sdk-typescript":
        value["artifacts"] = {
            "package_filename": f"teslatlas-sdk-{version}.tgz",
            "package_sha256": (
                "070906b5e3ead04a32223ca996d88ebf6f22be252821e56ef1839da3a13e23d7"
                if version == "2026.36.2"
                else "d" * 64
            ),
        }
    if name == "home-assistant":
        value["artifacts"] = {
            "payload_manifest_sha256": "e" * 64,
            "selection_receipt_sha256": "f" * 64,
        }
    return value


def catalog_data() -> dict:
    return {
        "schema_version": 1,
        "cohorts": [
            {
                "product_version": "2026.36.2",
                "publication_status": "local-unpublished",
                "admitted_hub_versions": [],
                "components": {
                    name: component(name, "2026.36.2") for name in REPOSITORIES
                },
            },
            {
                "product_version": "2026.37.1",
                "publication_status": "published",
                "admitted_hub_versions": ["2026.36.2"],
                "components": {
                    name: component(name, "2026.37.1") for name in REPOSITORIES
                },
            },
        ],
    }


def tree_snapshot(root: Path) -> dict[str, tuple[int, bytes | str | None]]:
    paths = [root, *sorted(root.rglob("*"))]
    result: dict[str, tuple[int, bytes | str | None]] = {}
    for path in paths:
        metadata = path.lstat()
        if stat.S_ISREG(metadata.st_mode):
            payload: bytes | str | None = path.read_bytes()
        elif stat.S_ISLNK(metadata.st_mode):
            payload = os.readlink(path)
        else:
            payload = None
        result[str(path.relative_to(root))] = (stat.S_IFMT(metadata.st_mode), payload)
    return result


class CatalogTests(unittest.TestCase):
    def test_catalog_schema_accepts_home_assistant_target_provenance(self) -> None:
        schema = json.loads(
            (Path(__file__).resolve().parents[1] / "catalog.schema.json").read_text()
        )
        catalog = catalog_data()

        self.assertEqual([], list(Draft202012Validator(schema).iter_errors(catalog)))

    def test_catalog_schema_binds_keys_repositories_and_artifact_shapes(self) -> None:
        schema = json.loads(
            (Path(__file__).resolve().parents[1] / "catalog.schema.json").read_text()
        )
        validator = Draft202012Validator(schema)

        mismatched_repository = catalog_data()
        mismatched_repository["cohorts"][0]["components"]["protocol"][
            "repository"
        ] = REPOSITORIES["sdk-typescript"]
        self.assertNotEqual(
            [], list(validator.iter_errors(mismatched_repository)),
            "a component key must bind its own repository",
        )

        for name in ("protocol", "sdk-swift", "edge"):
            with self.subTest(component=name):
                unexpected_artifact = catalog_data()
                unexpected_artifact["cohorts"][0]["components"][name]["artifacts"] = {
                    "package_filename": "teslatlas-sdk-2026.36.2.tgz",
                    "package_sha256": "a" * 64,
                }
                self.assertNotEqual(
                    [], list(validator.iter_errors(unexpected_artifact)),
                    f"{name} must forbid artifacts",
                )

        for name, wrong_artifacts in (
            (
                "sdk-typescript",
                {
                    "payload_manifest_sha256": "a" * 64,
                    "selection_receipt_sha256": "b" * 64,
                },
            ),
            (
                "home-assistant",
                {
                    "package_filename": "teslatlas-sdk-2026.36.2.tgz",
                    "package_sha256": "a" * 64,
                },
            ),
        ):
            with self.subTest(component=name):
                wrong_shape = catalog_data()
                wrong_shape["cohorts"][0]["components"][name][
                    "artifacts"
                ] = wrong_artifacts
                self.assertNotEqual(
                    [], list(validator.iter_errors(wrong_shape)),
                    f"{name} must use its repository-specific artifact shape",
                )

    def test_home_assistant_catalog_requires_target_provenance(self) -> None:
        data = catalog_data()
        del data["cohorts"][0]["components"]["home-assistant"]["artifacts"]

        with self.assertRaisesRegex(CatalogError, "provenance"):
            parse_catalog(data)

    def test_same_cohort_is_default_and_update_needs_explicit_later_admission(
        self,
    ) -> None:
        data = catalog_data()
        data["cohorts"][1]["admitted_hub_versions"] = []
        catalog = parse_catalog(data)
        selected = select_cohort(
            catalog,
            ("protocol", "sdk-typescript"),
            "2026.36.2",
            allow_candidates=True,
            update=True,
        )
        self.assertEqual(selected.product_version, "2026.36.2")

        data["cohorts"][1]["admitted_hub_versions"] = ["2026.36.2"]
        selected = select_cohort(
            parse_catalog(data),
            ("protocol", "sdk-typescript"),
            "2026.36.2",
            allow_candidates=True,
            update=True,
        )
        self.assertEqual(selected.product_version, "2026.37.1")
        self.assertEqual(set(selected.components), {"protocol", "sdk-typescript"})

        protocol_only = select_cohort(
            parse_catalog(data),
            ("protocol",),
            "2026.36.2",
            allow_candidates=True,
            update=False,
        )
        self.assertEqual(set(protocol_only.components), {"protocol"})

    def test_production_rejects_candidate_and_unknown_or_mutable_source(self) -> None:
        data = catalog_data()
        with self.assertRaisesRegex(CatalogError, "unpublished"):
            select_cohort(
                parse_catalog(data),
                ("protocol",),
                "2026.36.2",
                allow_candidates=False,
                update=False,
            )

        data["cohorts"][0]["components"]["protocol"]["repository"] = (
            "https://example.invalid/teslatlas-protocol.git"
        )
        with self.assertRaisesRegex(CatalogError, "repository"):
            parse_catalog(data)

        data = catalog_data()
        data["cohorts"][0]["components"]["protocol"]["commit"] = "main"
        with self.assertRaisesRegex(CatalogError, "commit"):
            parse_catalog(data)

    def test_production_update_can_skip_an_unpublished_same_version(self) -> None:
        selected = select_cohort(
            parse_catalog(catalog_data()),
            ("protocol",),
            "2026.36.2",
            allow_candidates=False,
            update=True,
        )
        self.assertEqual(selected.product_version, "2026.37.1")
        self.assertEqual(selected.publication_status, "published")

    def test_component_version_and_profile_must_match_the_cohort(self) -> None:
        data = catalog_data()
        data["cohorts"][0]["components"]["protocol"]["product_version"] = "2026.36.1"
        with self.assertRaisesRegex(CatalogError, "product_version"):
            parse_catalog(data)

        data = catalog_data()
        data["cohorts"][0]["components"]["protocol"]["profile"]["sha256"] = "0" * 64
        with self.assertRaisesRegex(CatalogError, "profile"):
            parse_catalog(data)

        data = catalog_data()
        data["cohorts"][0]["components"]["sdk-typescript"]["artifacts"][
            "package_sha256"
        ] = "0" * 64
        with self.assertRaisesRegex(CatalogError, "reviewed artifacts"):
            parse_catalog(data)

    def test_home_assistant_catalog_retains_both_provenance_hashes(self) -> None:
        data = catalog_data()
        cohort = data["cohorts"][0]
        cohort["components"]["home-assistant"] = {
                "repository": "https://github.com/magrathean-uk/teslatlas-home-assistant.git",
                "commit": "a" * 40,
                "source_sha256": "b" * 64,
                "product_version": "2026.36.2",
                "profile": {
                    "id": "hub-http-v1",
                    "revision": "1.0.0",
                    "sha256": PROFILE_SHA,
                },
                "artifacts": {
                    "payload_manifest_sha256": "c" * 64,
                    "selection_receipt_sha256": "d" * 64,
                },
            }

        catalog = parse_catalog(data)

        self.assertEqual(
            catalog.cohorts[0].components["home-assistant"].artifacts,
            {
                "payload_manifest_sha256": "c" * 64,
                "selection_receipt_sha256": "d" * 64,
            },
        )


class SourceTests(unittest.TestCase):
    def test_source_manifest_rejects_a_symlinked_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            outside = root / "outside"
            source.mkdir()
            outside.mkdir()
            (outside / "payload.txt").write_text("outside\n")
            (source / "linked").symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(BootstrapError, "unsupported file type"):
                source_manifest_for(
                    "protocol",
                    source,
                    REPOSITORIES["protocol"],
                    COMMITS["protocol"],
                )

    def test_local_manifest_binds_every_copied_byte_and_rejects_changes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            (source / "a.txt").write_text("alpha\n")
            script = source / "run.sh"
            script.write_text("#!/bin/sh\nexit 0\n")
            script.chmod(0o755)
            (source / ".git").mkdir()
            (source / ".git" / "ignored").write_text("mutable metadata")
            (source / "AGENTS.md").write_text("workspace instructions\n")
            record = source_manifest_for(
                "protocol", source, REPOSITORIES["protocol"], COMMITS["protocol"]
            )
            destination = root / "copy"
            verify_local_source(record, destination)
            self.assertEqual((destination / "a.txt").read_text(), "alpha\n")
            self.assertEqual((destination / "a.txt").stat().st_mode & 0o777, 0o644)
            self.assertTrue((destination / "run.sh").stat().st_mode & 0o100)
            self.assertFalse((destination / ".git").exists())
            self.assertFalse((destination / "AGENTS.md").exists())

            (source / "a.txt").write_text("changed\n")
            with self.assertRaisesRegex(BootstrapError, "changed"):
                verify_local_source(record, root / "rejected")

    def test_git_transport_checks_the_exact_detached_commit_and_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repository = root / "repo"
            repository.mkdir()
            subprocess.run(["git", "init", "-q"], cwd=repository, check=True)
            subprocess.run(
                ["git", "config", "user.email", "fixture@example.invalid"],
                cwd=repository,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.name", "Fixture"], cwd=repository, check=True
            )
            (repository / "payload.txt").write_text("immutable\n")
            subprocess.run(["git", "add", "payload.txt"], cwd=repository, check=True)
            subprocess.run(
                ["git", "commit", "-qm", "fixture"], cwd=repository, check=True
            )
            commit = subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=repository, text=True
            ).strip()
            expected = source_manifest_for(
                "protocol", repository, REPOSITORIES["protocol"], commit
            )
            destination = root / "checkout"
            observed = fetch_git_source(
                repository.as_uri(),
                commit,
                destination,
                expected["source_sha256"],
                allowed_repositories={repository.as_uri()},
                timeout_seconds=30,
            )
            self.assertEqual(observed["commit"], commit)
            self.assertEqual((destination / "payload.txt").read_text(), "immutable\n")

            with self.assertRaisesRegex(BootstrapError, "source digest"):
                fetch_git_source(
                    repository.as_uri(),
                    commit,
                    root / "bad-checkout",
                    "0" * 64,
                    allowed_repositories={repository.as_uri()},
                    timeout_seconds=30,
                )


class PrefixTests(unittest.TestCase):
    def test_remove_rejects_unsupported_active_paths_without_any_change(self) -> None:
        for kind in ("regular-file", "directory", "fifo"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                prefix = Path(temporary) / "prefix"
                release = prefix / "releases" / "foreign"
                release.mkdir(parents=True)
                (release / "keep").write_text("foreign release\n")
                (prefix / "history.json").write_text('["foreign"]\n')
                active = prefix / "active"
                if kind == "regular-file":
                    active.write_text("foreign active\n")
                elif kind == "directory":
                    active.mkdir()
                    (active / "keep").write_text("foreign directory\n")
                else:
                    os.mkfifo(active)
                before = tree_snapshot(prefix)

                with self.assertRaisesRegex(BootstrapError, "active path is unsafe"):
                    remove(prefix)

                self.assertEqual(tree_snapshot(prefix), before)

    def test_remove_rejects_unsafe_releases_without_any_change(self) -> None:
        for kind in ("symlink", "regular-file", "fifo"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                prefix = root / "prefix"
                outside = root / "outside"
                prefix.mkdir()
                outside.mkdir()
                (outside / "keep").write_text("outside release\n")
                (prefix / "history.json").write_text('["foreign"]\n')
                (prefix / "active").symlink_to("releases/foreign")
                (root / "ha-link").symlink_to(prefix / "active/ha")
                releases = prefix / "releases"
                if kind == "symlink":
                    releases.symlink_to(outside, target_is_directory=True)
                elif kind == "regular-file":
                    releases.write_text("foreign releases\n")
                else:
                    os.mkfifo(releases)
                before = tree_snapshot(root)

                with self.assertRaisesRegex(
                    BootstrapError, "releases path is unsafe"
                ):
                    remove(prefix)

                self.assertEqual(tree_snapshot(root), before)

    def test_remove_rejects_unsafe_history_without_any_change(self) -> None:
        for kind in ("symlink", "directory", "fifo"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                prefix = root / "prefix"
                release = prefix / "releases/foreign"
                release.mkdir(parents=True)
                (release / "keep").write_text("foreign release\n")
                (prefix / "active").symlink_to("releases/foreign")
                outside = root / "outside-history"
                outside.write_text('["outside"]\n')
                history = prefix / "history.json"
                if kind == "symlink":
                    history.symlink_to(outside)
                elif kind == "directory":
                    history.mkdir()
                    (history / "keep").write_text("foreign history\n")
                else:
                    os.mkfifo(history)
                before = tree_snapshot(root)

                with self.assertRaisesRegex(
                    BootstrapError, "history path is unsafe"
                ):
                    remove(prefix)

                self.assertEqual(tree_snapshot(root), before)

    def test_remove_rechecks_replaceable_paths_after_recovery_and_before_delete(
        self,
    ) -> None:
        from companions import core as core_module

        for boundary in ("after-recovery", "before-delete"):
            with (
                self.subTest(boundary=boundary),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary)
                prefix = root / "prefix"
                releases = prefix / "releases"
                releases.mkdir(parents=True)
                (releases / "keep").write_text("managed release\n")
                (prefix / "history.json").write_text("[]\n")
                outside = root / "outside"
                outside.mkdir()
                marker = outside / "keep"
                marker.write_text("outside release\n")
                saved = root / "saved-releases"

                def replace_releases() -> None:
                    releases.rename(saved)
                    releases.symlink_to(outside, target_is_directory=True)

                if boundary == "after-recovery":
                    def recover_then_replace(_prefix: Path) -> None:
                        replace_releases()
                        return None

                    context = patch(
                        "companions.core._recover_transaction",
                        side_effect=recover_then_replace,
                    )
                else:
                    real_validate = core_module._validate_removal_paths
                    validation_calls = 0

                    def replace_before_final_check(target_prefix: Path) -> None:
                        nonlocal validation_calls
                        validation_calls += 1
                        if validation_calls == 4:
                            replace_releases()
                        real_validate(target_prefix)

                    context = patch(
                        "companions.core._validate_removal_paths",
                        side_effect=replace_before_final_check,
                    )

                with context, self.assertRaisesRegex(
                    BootstrapError, "releases path is unsafe"
                ):
                    remove(prefix)

                self.assertEqual(marker.read_text(), "outside release\n")
                self.assertEqual((saved / "keep").read_text(), "managed release\n")
                self.assertEqual((prefix / "history.json").read_text(), "[]\n")
                self.assertTrue(releases.is_symlink())
                self.assertFalse((prefix / "active").exists())
                self.assertFalse((prefix / "transaction.json").exists())

    def test_remove_preserves_data_and_config_and_unlinks_only_owned_target(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            source = root / "source"
            source.mkdir()
            (source / "payload.txt").write_text("source\n")
            repository = REPOSITORIES["home-assistant"]
            record = source_manifest_for(
                "home-assistant", source, repository, "1" * 40
            )
            data = catalog_data()
            data["cohorts"] = [data["cohorts"][0]]
            component_data = component(
                "home-assistant", "2026.36.2", digest=record["source_sha256"]
            )
            component_data["commit"] = "1" * 40
            component_data["artifacts"]["payload_manifest_sha256"] = (
                "b720c922e53edd47a62de17d99ff1d32c638706886d32a5e2fd7339b11d78347"
            )
            data["cohorts"][0]["components"]["home-assistant"] = component_data
            selected = select_cohort(
                parse_catalog(data),
                ("home-assistant",),
                "2026.36.2",
                allow_candidates=True,
                update=False,
            )
            config = root / "ha-config"
            config.mkdir()

            def build(_name: str, _source: Path, output: Path) -> dict:
                payload = output / "custom_components" / "teslatlas_hub"
                payload.mkdir(parents=True)
                (payload / "manifest.json").write_text("{}\n")
                return {"commands": [], "dependencies": {}}

            install_cohort(
                prefix,
                selected,
                {"home-assistant": record},
                build,
                target_binding={"ha_config": str(config.resolve())},
            )
            (prefix / "data").mkdir()
            (prefix / "data" / "keep").write_text("data\n")
            (prefix / "config").mkdir()
            (prefix / "config" / "keep").write_text("config\n")

            result = remove(prefix)

            self.assertEqual(result["status"], "removed")
            self.assertFalse((prefix / "active").exists())
            self.assertFalse((prefix / "releases").exists())
            self.assertFalse((config / "custom_components/teslatlas_hub").exists())
            self.assertEqual((prefix / "data" / "keep").read_text(), "data\n")
            self.assertEqual((prefix / "config" / "keep").read_text(), "config\n")
            self.assertEqual(status(prefix)["status"], "not-installed")
            self.assertEqual(remove(prefix)["status"], "not-installed")

    def test_home_assistant_install_binds_catalog_provenance_to_target_receipt(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            source = root / "source"
            source.mkdir()
            (source / "payload.txt").write_text("source\n")
            repository = "https://github.com/magrathean-uk/teslatlas-home-assistant.git"
            record = source_manifest_for("home-assistant", source, repository, "1" * 40)
            data = catalog_data()
            data["cohorts"] = [data["cohorts"][0]]
            data["cohorts"][0]["components"]["home-assistant"] = {
                                "repository": repository,
                                "commit": "1" * 40,
                                "source_sha256": record["source_sha256"],
                                "product_version": "2026.36.2",
                                "profile": {
                                    "id": "hub-http-v1",
                                    "revision": "1.0.0",
                                    "sha256": PROFILE_SHA,
                                },
                                "artifacts": {
                                    "payload_manifest_sha256": "b720c922e53edd47a62de17d99ff1d32c638706886d32a5e2fd7339b11d78347",
                                    "selection_receipt_sha256": "d" * 64,
                                },
            }
            selected = select_cohort(
                parse_catalog(data),
                ("home-assistant",),
                "2026.36.2",
                allow_candidates=True,
                update=False,
            )
            config = root / "ha-config"
            config.mkdir()

            def build(_name: str, _source: Path, output: Path) -> dict:
                payload = output / "custom_components" / "teslatlas_hub"
                payload.mkdir(parents=True)
                (payload / "manifest.json").write_text("{}\n")
                return {"commands": [], "dependencies": {}}

            result = install_cohort(
                prefix,
                selected,
                {"home-assistant": record},
                build,
                target_binding={"ha_config": str(config.resolve())},
            )

            receipt = json.loads(Path(result["receipt"]).read_text())
            self.assertEqual(
                receipt["targets"],
                {
                    "ha_config": str(config.resolve()),
                    "ha_payload_manifest_sha256": "b720c922e53edd47a62de17d99ff1d32c638706886d32a5e2fd7339b11d78347",
                    "ha_selection_receipt_sha256": "d" * 64,
                },
            )
            target = config / "custom_components" / "teslatlas_hub"
            self.assertTrue(target.is_symlink())
            target.unlink()

            activate_targets(
                prefix,
                ("home-assistant",),
                RecipeContext(ha_config=config),
            )

            self.assertTrue(target.is_symlink())
            receipt["targets"]["ha_selection_receipt_sha256"] = "0" * 64
            Path(result["receipt"]).write_text(json.dumps(receipt))
            with self.assertRaisesRegex(
                BootstrapError, "Home Assistant target provenance"
            ):
                status(prefix)

    def test_prefix_operations_reject_a_symlinked_prefix(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            actual = root / "actual"
            actual.mkdir()
            link = root / "prefix"
            link.symlink_to(actual, target_is_directory=True)
            with self.assertRaisesRegex(BootstrapError, "must not be a symlink"):
                status(link)

    def test_lock_never_follows_an_existing_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            prefix.mkdir()
            outside = root / "outside"
            outside.write_text("preserve\n")
            (prefix / ".bootstrap.lock").symlink_to(outside)
            with self.assertRaisesRegex(BootstrapError, "lock path is unsafe"):
                with PrefixLock(prefix):
                    self.fail("unsafe lock acquired")
            self.assertEqual(outside.read_text(), "preserve\n")

    def test_complete_source_set_is_validated_before_any_build_runs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            records = {}
            data = catalog_data()
            data["cohorts"] = [data["cohorts"][0]]
            for name in ("protocol", "sdk-typescript"):
                source = root / name
                source.mkdir()
                (source / "payload.txt").write_text(f"{name}\n")
                record = source_manifest_for(
                    name, source, REPOSITORIES[name], COMMITS[name]
                )
                records[name] = record
                data["cohorts"][0]["components"][name]["source_sha256"] = record[
                    "source_sha256"
                ]
            selected = select_cohort(
                parse_catalog(data),
                records,
                "2026.36.2",
                allow_candidates=True,
                update=False,
            )
            validated = []
            built = []

            def validate(name: str, _source: Path) -> dict:
                validated.append(name)
                if name == "sdk-typescript":
                    raise BootstrapError("synthetic metadata mismatch")
                return {}

            def build(name: str, _source: Path, _output: Path) -> dict:
                built.append(name)
                return {}

            with self.assertRaisesRegex(BootstrapError, "metadata mismatch"):
                install_cohort(
                    root / "prefix",
                    selected,
                    records,
                    build,
                    validate_component=validate,
                )
            self.assertEqual(validated, ["protocol", "sdk-typescript"])
            self.assertEqual(built, [])

    def test_lock_contention_fails_without_waiting(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            prefix = Path(temporary)
            with PrefixLock(prefix):
                read_fd, write_fd = os.pipe()
                child = os.fork()
                if child == 0:
                    os.close(read_fd)
                    try:
                        with PrefixLock(prefix):
                            result = b"unexpected"
                    except BootstrapError:
                        result = b"busy"
                    os.write(write_fd, result)
                    os._exit(0)
                os.close(write_fd)
                self.assertEqual(os.read(read_fd, 32), b"busy")
                os.waitpid(child, 0)

    def test_install_noop_failure_and_rollback_preserve_active_and_data(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            source = root / "source"
            source.mkdir()
            (source / "payload.txt").write_text("v1\n")
            record = source_manifest_for(
                "protocol", source, REPOSITORIES["protocol"], COMMITS["protocol"]
            )
            data = catalog_data()
            data["cohorts"] = [data["cohorts"][0]]
            data["cohorts"][0]["components"]["protocol"] = component(
                "protocol", "2026.36.2", digest=record["source_sha256"]
            )
            selected = select_cohort(
                parse_catalog(data),
                ("protocol",),
                "2026.36.2",
                allow_candidates=True,
                update=False,
            )
            local_sources = {"protocol": record}
            builds: list[str] = []

            def build(name: str, source_path: Path, output_path: Path) -> dict:
                builds.append(name)
                output_path.mkdir()
                (output_path / "installed.txt").write_text(
                    (source_path / "payload.txt").read_text()
                )
                return {"commands": ["fixture-build"], "dependencies": {}}

            first = install_cohort(prefix, selected, local_sources, build)
            self.assertEqual(first["status"], "installed")
            self.assertEqual(
                json.loads(Path(first["receipt"]).read_text())["hub_version"],
                "2026.36.2",
            )
            first_id = active_release_id(prefix)
            self.assertEqual(builds, ["protocol"])
            (prefix / "data" / "edge").mkdir(parents=True)
            (prefix / "data" / "edge" / "spool").write_text("keep")

            repeated = install_cohort(prefix, selected, local_sources, build)
            self.assertEqual(repeated["status"], "no-op")
            self.assertEqual(builds, ["protocol"])

            (source / "payload.txt").write_text("v2\n")
            record2 = source_manifest_for(
                "protocol", source, REPOSITORIES["protocol"], "3" * 40
            )
            data["cohorts"][0]["components"]["protocol"]["commit"] = "3" * 40
            data["cohorts"][0]["components"]["protocol"]["source_sha256"] = record2[
                "source_sha256"
            ]
            selected2 = select_cohort(
                parse_catalog(data),
                ("protocol",),
                "2026.36.2",
                allow_candidates=True,
                update=False,
            )

            def fail_build(_name: str, _source: Path, _output: Path) -> dict:
                raise BootstrapError("synthetic build failure")

            with self.assertRaisesRegex(BootstrapError, "synthetic build failure"):
                install_cohort(prefix, selected2, {"protocol": record2}, fail_build)
            self.assertEqual(active_release_id(prefix), first_id)
            failures = list((prefix / "failures").glob("*/failure.json"))
            self.assertEqual(len(failures), 1)
            self.assertEqual(
                json.loads(failures[0].read_text())["active_release_before"], first_id
            )

            from companions import core as core_module

            original_activate = core_module._activate
            activation_calls = 0

            def activate_then_interrupt(target_prefix: Path, release_id: str) -> None:
                nonlocal activation_calls
                activation_calls += 1
                original_activate(target_prefix, release_id)
                if activation_calls == 1:
                    raise KeyboardInterrupt

            with (
                patch("companions.core._activate", side_effect=activate_then_interrupt),
                self.assertRaises(KeyboardInterrupt),
            ):
                install_cohort(prefix, selected2, {"protocol": record2}, build)
            self.assertEqual(active_release_id(prefix), first_id)

            second = install_cohort(prefix, selected2, {"protocol": record2}, build)
            self.assertEqual(second["status"], "installed")
            second_id = active_release_id(prefix)
            self.assertNotEqual(second_id, first_id)
            self.assertEqual(rollback(prefix)["active_release"], first_id)
            self.assertEqual(active_release_id(prefix), first_id)
            self.assertEqual((prefix / "data" / "edge" / "spool").read_text(), "keep")

    def test_published_git_mode_is_distinct_in_input_and_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            source = root / "source"
            source.mkdir()
            (source / "payload.txt").write_text("published\n")
            record = source_manifest_for(
                "protocol", source, REPOSITORIES["protocol"], COMMITS["protocol"]
            )
            data = catalog_data()
            published = data["cohorts"][1]
            published["product_version"] = "2026.36.2"
            published["admitted_hub_versions"] = []
            published["components"] = {
                name: component(name, "2026.36.2") for name in REPOSITORIES
            }
            published["components"]["protocol"] = component(
                "protocol", "2026.36.2", digest=record["source_sha256"]
            )
            selected = select_cohort(
                parse_catalog({"schema_version": 1, "cohorts": [published]}),
                ("protocol",),
                "2026.36.2",
                allow_candidates=False,
                update=False,
            )

            def build(_name: str, source_path: Path, output_path: Path) -> dict:
                output_path.mkdir()
                (output_path / "installed.txt").write_bytes(
                    (source_path / "payload.txt").read_bytes()
                )
                return {"commands": [], "dependencies": {}}

            result = install_cohort(
                prefix,
                selected,
                {"protocol": record},
                build,
                installation_mode="production-git",
            )
            receipt = json.loads(Path(result["receipt"]).read_text())
            self.assertEqual(receipt["installation_mode"], "production-git")
            self.assertEqual(
                receipt["components"]["protocol"]["source"]["transport"],
                "git-immutable",
            )
            self.assertEqual(
                receipt["components"]["protocol"]["source"]["public_availability"],
                "verified",
            )

    def test_interruption_removes_staging_and_keeps_active(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            source = root / "source"
            source.mkdir()
            (source / "payload.txt").write_text("source\n")
            record = source_manifest_for(
                "protocol", source, REPOSITORIES["protocol"], COMMITS["protocol"]
            )
            data = catalog_data()
            data["cohorts"] = [data["cohorts"][0]]
            data["cohorts"][0]["components"]["protocol"] = component(
                "protocol", "2026.36.2", digest=record["source_sha256"]
            )
            selected = select_cohort(
                parse_catalog(data),
                ("protocol",),
                "2026.36.2",
                allow_candidates=True,
                update=False,
            )

            def interrupt(_name: str, _source: Path, _output: Path) -> dict:
                raise KeyboardInterrupt

            with self.assertRaises(KeyboardInterrupt):
                install_cohort(prefix, selected, {"protocol": record}, interrupt)
            self.assertIsNone(active_release_id(prefix))
            self.assertEqual(list((prefix / ".staging").iterdir()), [])

            def build(_name: str, source_path: Path, output_path: Path) -> dict:
                output_path.mkdir()
                (output_path / "installed.txt").write_bytes(
                    (source_path / "payload.txt").read_bytes()
                )
                return {"commands": [], "dependencies": {}}

            resumed = install_cohort(prefix, selected, {"protocol": record}, build)
            self.assertEqual(resumed["status"], "installed")
            self.assertEqual(
                (
                    prefix
                    / "active"
                    / "components"
                    / "protocol"
                    / "output"
                    / "installed.txt"
                ).read_text(),
                "source\n",
            )


if __name__ == "__main__":
    unittest.main()
