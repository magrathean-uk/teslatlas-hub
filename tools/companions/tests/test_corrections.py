# SPDX-License-Identifier: AGPL-3.0-only
from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import signal
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from companions import cli, core, recipes, targets  # noqa: E402
from companions.processes import ProcessFailure, run_process  # noqa: E402

PROFILE = {
    "id": "hub-http-v1",
    "revision": "1.0.0",
    "sha256": recipes.HUB_PROFILE_SHA256,
}


def _source(root: Path, name: str, commit: str) -> dict:
    path = root / f"source-{name}-{commit[0]}"
    path.mkdir()
    (path / "compatibility").mkdir()
    (path / "compatibility/hub.json").write_text(
        json.dumps(
            {
                "product_version": "2026.36.2",
                "status": "candidate",
                "profile": recipes.EXPECTED_PROFILES[name],
            }
        )
    )
    (path / "pyproject.toml").write_text('version = "2026.36.2"\n')
    (path / "uv.lock").write_text("locked\n")
    return core.source_manifest_for(
        name, path, recipes.KNOWN_REPOSITORIES[name], commit
    )


def _component(name: str, record: dict) -> dict:
    component = {
        "repository": record["repository"],
        "commit": record["commit"],
        "source_sha256": record["source_sha256"],
        "product_version": "2026.36.2",
        "profile": recipes.EXPECTED_PROFILES[name],
    }
    if name == "home-assistant":
        component["artifacts"] = {
            "payload_manifest_sha256": "9d857e0ee62076cdda1238ca7fc14ad2afa3695b2cf5cdaefc8c4edbc64e870a",
            "selection_receipt_sha256": "e" * 64,
        }
    if name == "sdk-typescript":
        component["artifacts"] = {
            "package_filename": "teslatlas-sdk-2026.36.2.tgz",
            "package_sha256": recipes.SDK_TARBALL_SHA256,
        }
    return component


def _catalog_components(records: dict[str, dict]) -> dict[str, dict]:
    catalog_records = dict(records)
    for index, (name, repository) in enumerate(recipes.KNOWN_REPOSITORIES.items(), 1):
        catalog_records.setdefault(
            name,
            {
                "repository": repository,
                "commit": str(index) * 40,
                "source_sha256": str(index) * 64,
            },
        )
    return {
        name: _component(name, catalog_records[name])
        for name in recipes.KNOWN_REPOSITORIES
    }


def _cohort(records: dict[str, dict], names: tuple[str, ...]) -> core.Cohort:
    data = {
        "schema_version": 1,
        "cohorts": [
            {
                "product_version": "2026.36.2",
                "publication_status": "local-unpublished",
                "admitted_hub_versions": [],
                "components": _catalog_components(records),
            }
        ],
    }
    return core.select_cohort(
        core.parse_catalog(data),
        names,
        "2026.36.2",
        allow_candidates=True,
        update=False,
    )


def _build(name: str, source: Path, output: Path) -> dict:
    output.mkdir(parents=True)
    if name == "home-assistant":
        payload = output / "custom_components/teslatlas_hub"
        payload.mkdir(parents=True)
        (payload / "manifest.json").write_text('{"domain":"teslatlas_hub"}\n')
    else:
        (output / "installed.txt").write_bytes((source / "pyproject.toml").read_bytes())
    return {"commands": [], "dependencies": {}}


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class TransactionAndTargetTests(unittest.TestCase):
    def test_target_failure_is_compensated_while_competing_operation_is_locked(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            config = root / "ha"
            config.mkdir()
            records = {
                "protocol": _source(root, "protocol", "1" * 40),
                "home-assistant": _source(root, "home-assistant", "2" * 40),
            }
            first = core.install_cohort(
                prefix, _cohort(records, ("protocol",)), records, _build
            )
            built: list[str] = []

            def build(name: str, source: Path, output: Path) -> dict:
                built.append(name)
                return _build(name, source, output)

            real_apply = targets.apply_external_state
            calls = 0

            def fail_once(entries, passed_prefix):
                nonlocal calls
                calls += 1
                with self.assertRaisesRegex(core.BootstrapError, "busy"):
                    with core.PrefixLock(prefix):
                        pass
                real_apply(entries, passed_prefix)
                if calls == 1:
                    raise core.BootstrapError("synthetic target failure")

            with (
                patch("companions.targets.apply_external_state", side_effect=fail_once),
                self.assertRaisesRegex(core.BootstrapError, "target failure"),
            ):
                core.install_cohort(
                    prefix,
                    _cohort(records, ("home-assistant",)),
                    records,
                    build,
                    target_binding={"ha_config": str(config)},
                )
            self.assertEqual(built, ["home-assistant"])
            self.assertEqual(core.active_release_id(prefix), first["active_release"])
            self.assertFalse((config / "custom_components/teslatlas_hub").exists())
            self.assertFalse((prefix / "transaction.json").exists())

    def test_ha_add_remove_config_change_and_rollback_reconcile_only_owned_links(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            config_a = root / "ha-a"
            config_b = root / "ha-b"
            config_a.mkdir()
            config_b.mkdir()
            registry = config_a / ".storage"
            registry.mkdir()
            (registry / "core.entity_registry").write_text("preserve\n")
            records = {
                "protocol": _source(root, "protocol", "1" * 40),
                "home-assistant": _source(root, "home-assistant", "2" * 40),
            }
            ha = _cohort(records, ("home-assistant",))
            protocol = _cohort(records, ("protocol",))
            core.install_cohort(
                prefix,
                ha,
                records,
                _build,
                target_binding={"ha_config": str(config_a)},
            )
            link_a = config_a / "custom_components/teslatlas_hub"
            link_b = config_b / "custom_components/teslatlas_hub"
            self.assertTrue(link_a.is_symlink())
            core.install_cohort(prefix, protocol, records, _build)
            self.assertFalse(link_a.exists())
            core.rollback(prefix)
            self.assertTrue(link_a.is_symlink())
            core.install_cohort(
                prefix,
                ha,
                records,
                _build,
                target_binding={"ha_config": str(config_b)},
            )
            self.assertFalse(link_a.exists())
            self.assertTrue(link_b.is_symlink())
            core.rollback(prefix)
            self.assertTrue(link_a.is_symlink())
            self.assertFalse(link_b.exists())
            self.assertEqual(
                (registry / "core.entity_registry").read_text(), "preserve\n"
            )

    def test_process_death_at_each_boundary_recovers_or_finishes_explicitly(self):
        for phase in ("intent", "targets", "history", "active", "committed"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                prefix = root / "prefix"
                config = root / "ha"
                config.mkdir()
                records = {
                    "protocol": _source(root, "protocol", "1" * 40),
                    "home-assistant": _source(root, "home-assistant", "2" * 40),
                }
                first = core.install_cohort(
                    prefix, _cohort(records, ("protocol",)), records, _build
                )
                child = os.fork()
                if child == 0:
                    with patch(
                        "companions.core._transaction_checkpoint",
                        side_effect=lambda observed: os._exit(77)
                        if observed == phase
                        else None,
                    ):
                        core.install_cohort(
                            prefix,
                            _cohort(records, ("home-assistant",)),
                            records,
                            _build,
                            target_binding={"ha_config": str(config)},
                        )
                    os._exit(0)
                _pid, wait_status = os.waitpid(child, 0)
                self.assertEqual(os.waitstatus_to_exitcode(wait_status), 77)
                self.assertTrue((prefix / "transaction.json").is_file())
                recovered = core.status(prefix)
                link = config / "custom_components/teslatlas_hub"
                if phase == "committed":
                    self.assertNotEqual(
                        recovered["active_release"], first["active_release"]
                    )
                    self.assertEqual(recovered["recovery"], "completed")
                    self.assertTrue(link.is_symlink())
                else:
                    self.assertEqual(
                        recovered["active_release"], first["active_release"]
                    )
                    self.assertEqual(recovered["recovery"], "rolled-back")
                    self.assertFalse(link.exists())
                self.assertFalse((prefix / "transaction.json").exists())


class IdentityAndDryRunTests(unittest.TestCase):
    def test_output_manifest_includes_runtime_node_modules_and_detects_changes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runtime = root / "runtime/node_modules/@teslatlas/sdk/bin"
            assets = root / "runtime/node_modules/@teslatlas/sdk/dist/assets"
            runtime.mkdir(parents=True)
            assets.mkdir(parents=True)
            cli_path = runtime / "teslatlas-sdk.mjs"
            asset = assets / "index.js"
            cli_path.write_text("one\n")
            asset.write_text("asset-one\n")
            before = core._directory_manifest(root)
            paths = {item["path"] for item in before["files"]}
            self.assertIn(
                "runtime/node_modules/@teslatlas/sdk/bin/teslatlas-sdk.mjs",
                paths,
            )
            cli_path.write_text("two\n")
            after_cli = core._directory_manifest(root)
            self.assertNotEqual(before["sha256"], after_cli["sha256"])
            asset.write_text("asset-two\n")
            self.assertNotEqual(
                after_cli["sha256"], core._directory_manifest(root)["sha256"]
            )
            (root / "unsafe-link").symlink_to(asset)
            with self.assertRaisesRegex(core.BootstrapError, "unsupported link"):
                core._directory_manifest(root)

    def test_output_manifest_rejects_missing_or_symlinked_roots(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(core.BootstrapError, "produced no output"):
                core._directory_manifest(root / "missing")
            output = root / "output"
            output.mkdir()
            link = root / "linked-output"
            link.symlink_to(output)
            with self.assertRaisesRegex(core.BootstrapError, "missing or unsafe"):
                core._directory_manifest(link)

    def test_corrupt_retained_release_is_rebuilt_without_discarding_good_stage(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            records_a = {"protocol": _source(root, "protocol", "1" * 40)}
            cohort_a = _cohort(records_a, ("protocol",))
            first = core.install_cohort(prefix, cohort_a, records_a, _build)
            source_b = _source(root, "protocol", "3" * 40)
            records_b = {"protocol": source_b}
            cohort_b = _cohort(records_b, ("protocol",))
            core.install_cohort(prefix, cohort_b, records_b, _build)
            damaged = (
                prefix
                / "releases"
                / first["active_release"]
                / "components/protocol/output/installed.txt"
            )
            damaged.write_text("CORRUPTED\n")
            repaired = core.install_cohort(prefix, cohort_a, records_a, _build)
            self.assertIn("-repair-", repaired["active_release"])
            self.assertNotEqual(
                (
                    prefix / "active/components/protocol/output/installed.txt"
                ).read_text(),
                "CORRUPTED\n",
            )
            self.assertNotEqual(repaired["active_release"], first["active_release"])
            active_payload = prefix / "active/components/protocol/output/installed.txt"
            active_payload.unlink()
            repaired_again = core.install_cohort(prefix, cohort_a, records_a, _build)
            self.assertIn("-repair-", repaired_again["active_release"])
            self.assertTrue(
                (prefix / "active/components/protocol/output/installed.txt").is_file()
            )

    def test_identical_dry_run_never_repairs_link_or_rewrites_marker(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            config = root / "ha"
            config.mkdir()
            records = {"home-assistant": _source(root, "home-assistant", "2" * 40)}
            cohort = _cohort(records, ("home-assistant",))
            installed = core.install_cohort(
                prefix,
                cohort,
                records,
                _build,
                target_binding={"ha_config": str(config)},
            )
            link = config / "custom_components/teslatlas_hub"
            link.unlink()
            marker = (
                prefix
                / "releases"
                / installed["active_release"]
                / "external-actions.json"
            )
            marker_before = marker.read_bytes()
            marker_stat = marker.stat()
            catalog = root / "catalog.json"
            catalog.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "cohorts": [
                            {
                                "product_version": cohort.product_version,
                                "publication_status": cohort.publication_status,
                                "admitted_hub_versions": [],
                                "components": _catalog_components(records),
                            }
                        ],
                    }
                )
            )
            source_manifest = root / "sources.json"
            source_manifest.write_text(
                json.dumps({"schema_version": 1, "components": records})
            )
            arguments = [
                "dry-run",
                "--prefix",
                str(prefix),
                "--components",
                "home-assistant",
                "--hub-version",
                "2026.36.2",
                "--catalog",
                str(catalog),
                "--mode",
                "local-candidate",
                "--local-sources",
                str(source_manifest),
                "--ha-config",
                str(config),
            ]
            with (
                patch("companions.cli.check_recipe_environment", return_value={}),
                contextlib.redirect_stdout(io.StringIO()) as output,
            ):
                code = cli.main(arguments)
            self.assertEqual(code, 0, output.getvalue())
            self.assertEqual(json.loads(output.getvalue())["status"], "dry-run")
            self.assertFalse(link.exists())
            self.assertEqual(marker.read_bytes(), marker_before)
            self.assertEqual(marker.stat().st_mtime_ns, marker_stat.st_mtime_ns)

    def test_selected_source_binding_and_dependencies_precede_dry_run(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = _source(root, "protocol", "1" * 40)
            cohort = _cohort({"protocol": record}, ("protocol",))
            mismatched = dict(record, source_sha256="0" * 64)
            with self.assertRaisesRegex(core.BootstrapError, "does not match"):
                core.validate_selected_sources(cohort, {"protocol": mismatched})
            with self.assertRaisesRegex(core.BootstrapError, "unknown components"):
                recipes.validate_component_set(("deferred-product",))

    def test_fresh_dry_run_never_creates_installation_parent_on_success_or_failure(
        self,
    ):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = _source(root, "protocol", "1" * 40)
            cohort = _cohort({"protocol": record}, ("protocol",))
            catalog = root / "catalog.json"
            catalog.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "cohorts": [
                            {
                                "product_version": cohort.product_version,
                                "publication_status": cohort.publication_status,
                                "admitted_hub_versions": [],
                                "components": _catalog_components({"protocol": record}),
                            }
                        ],
                    }
                )
            )
            sources = root / "sources.json"
            sources.write_text(
                json.dumps({"schema_version": 1, "components": {"protocol": record}})
            )
            absent_parent = root / "absent" / "parent"
            arguments = [
                "dry-run",
                "--prefix",
                str(absent_parent / "prefix"),
                "--components",
                "protocol",
                "--hub-version",
                "2026.36.2",
                "--catalog",
                str(catalog),
                "--mode",
                "local-candidate",
                "--local-sources",
                str(sources),
            ]
            with (
                patch("companions.cli.check_recipe_environment", return_value={}),
                contextlib.redirect_stdout(io.StringIO()) as output,
            ):
                self.assertEqual(cli.main(arguments), 0, output.getvalue())
            self.assertFalse(absent_parent.parent.exists())
            with (
                patch(
                    "companions.cli.check_recipe_environment",
                    side_effect=core.BootstrapError("synthetic missing tool"),
                ),
                contextlib.redirect_stdout(io.StringIO()) as output,
            ):
                self.assertEqual(cli.main(arguments), 2, output.getvalue())
            self.assertFalse(absent_parent.parent.exists())


class RecoveryAndCliBoundaryTests(unittest.TestCase):
    def _state(self, prefix: Path, config: Path) -> dict:
        link = config / "custom_components/teslatlas_hub"
        markers = {}
        for marker in sorted((prefix / "releases").glob("*/external-actions.json")):
            markers[str(marker.relative_to(prefix))] = {
                "content": marker.read_bytes(),
                "mtime": marker.stat().st_mtime_ns,
            }
        return {
            "active": os.readlink(prefix / "active"),
            "active_mtime": (prefix / "active").lstat().st_mtime_ns,
            "history": (prefix / "history.json").read_bytes(),
            "history_mtime": (prefix / "history.json").stat().st_mtime_ns,
            "journal": (prefix / "transaction.json").read_bytes(),
            "journal_mtime": (prefix / "transaction.json").stat().st_mtime_ns,
            "link": os.readlink(link) if link.is_symlink() else None,
            "link_mtime": link.lstat().st_mtime_ns if link.is_symlink() else None,
            "markers": markers,
        }

    def test_recovery_prevalidates_corrupt_or_missing_selected_release(self):
        for phase, selected in (("active", "before"), ("committed", "after")):
            for damage in ("corrupt", "missing"):
                with (
                    self.subTest(phase=phase, damage=damage),
                    tempfile.TemporaryDirectory() as temporary,
                ):
                    root = Path(temporary)
                    prefix = root / "prefix"
                    config = root / "ha"
                    config.mkdir()
                    records = {
                        "protocol": _source(root, "protocol", "1" * 40),
                        "home-assistant": _source(root, "home-assistant", "2" * 40),
                    }
                    first = core.install_cohort(
                        prefix, _cohort(records, ("protocol",)), records, _build
                    )
                    child = os.fork()
                    if child == 0:
                        with patch(
                            "companions.core._transaction_checkpoint",
                            side_effect=lambda observed: os._exit(77)
                            if observed == phase
                            else None,
                        ):
                            core.install_cohort(
                                prefix,
                                _cohort(records, ("home-assistant",)),
                                records,
                                _build,
                                target_binding={"ha_config": str(config)},
                            )
                        os._exit(0)
                    _pid, wait_status = os.waitpid(child, 0)
                    self.assertEqual(os.waitstatus_to_exitcode(wait_status), 77)
                    journal = json.loads((prefix / "transaction.json").read_text())
                    selected_release = journal[selected]["active"]
                    self.assertIsInstance(selected_release, str)
                    if selected == "before":
                        self.assertEqual(selected_release, first["active_release"])
                    payload = (
                        prefix
                        / "releases"
                        / selected_release
                        / "components"
                        / ("protocol" if selected == "before" else "home-assistant")
                        / "output"
                    )
                    file = next(path for path in payload.rglob("*") if path.is_file())
                    if damage == "corrupt":
                        file.write_text("CORRUPTED\n")
                    else:
                        file.unlink()
                    state_before = self._state(prefix, config)
                    with self.assertRaises(core.BootstrapError):
                        core.status(prefix)
                    self.assertEqual(self._state(prefix, config), state_before)

    def test_missing_ha_noop_repair_recovers_hard_exit_and_caught_failure(self):
        for phase in ("intent", "targets"):
            for failure in ("hard-exit", "caught"):
                with (
                    self.subTest(phase=phase, failure=failure),
                    tempfile.TemporaryDirectory() as temporary,
                ):
                    root = Path(temporary)
                    prefix = root / "prefix"
                    config = root / "ha"
                    registry = config / ".storage"
                    registry.mkdir(parents=True)
                    config_sentinel = config / "configuration.yaml"
                    registry_sentinel = registry / "core.entity_registry"
                    config_sentinel.write_text("preserve-config\n")
                    registry_sentinel.write_text("preserve-registry\n")
                    records = {
                        "protocol": _source(root, "protocol", "1" * 40),
                        "home-assistant": _source(root, "home-assistant", "2" * 40),
                    }
                    protocol = core.install_cohort(
                        prefix, _cohort(records, ("protocol",)), records, _build
                    )
                    ha_cohort = _cohort(records, ("home-assistant",))
                    ha = core.install_cohort(
                        prefix,
                        ha_cohort,
                        records,
                        _build,
                        target_binding={"ha_config": str(config)},
                    )
                    link = config / "custom_components/teslatlas_hub"
                    self.assertTrue(link.is_symlink())
                    expected_target = os.readlink(link)
                    link.unlink()
                    if failure == "hard-exit":
                        child = os.fork()
                        if child == 0:
                            with patch(
                                "companions.core._transaction_checkpoint",
                                side_effect=lambda observed: os._exit(77)
                                if observed == phase
                                else None,
                            ):
                                core.install_cohort(
                                    prefix,
                                    ha_cohort,
                                    records,
                                    _build,
                                    target_binding={"ha_config": str(config)},
                                )
                            os._exit(0)
                        _pid, wait_status = os.waitpid(child, 0)
                        self.assertEqual(os.waitstatus_to_exitcode(wait_status), 77)
                        self.assertTrue((prefix / "transaction.json").is_file())
                        recovered = core.status(prefix)
                        self.assertEqual(recovered["recovery"], "rolled-back")
                    else:

                        def interrupt(observed):
                            if observed == phase:
                                raise KeyboardInterrupt

                        with (
                            patch(
                                "companions.core._transaction_checkpoint",
                                side_effect=interrupt,
                            ),
                            self.assertRaises(KeyboardInterrupt),
                        ):
                            core.install_cohort(
                                prefix,
                                ha_cohort,
                                records,
                                _build,
                                target_binding={"ha_config": str(config)},
                            )
                        recovered = core.status(prefix)
                        if phase == "intent":
                            self.assertEqual(recovered["recovery"], "rolled-back")
                        else:
                            self.assertNotIn("recovery", recovered)
                    self.assertEqual(recovered["active_release"], ha["active_release"])
                    self.assertFalse(link.exists())
                    self.assertFalse((prefix / "transaction.json").exists())
                    core._verify_release(prefix, ha["active_release"])
                    retried = core.install_cohort(
                        prefix,
                        ha_cohort,
                        records,
                        _build,
                        target_binding={"ha_config": str(config)},
                    )
                    self.assertEqual(retried["status"], "no-op")
                    self.assertTrue(link.is_symlink())
                    self.assertEqual(os.readlink(link), expected_target)
                    rolled_back = core.rollback(prefix)
                    self.assertEqual(
                        rolled_back["active_release"], protocol["active_release"]
                    )
                    self.assertFalse(link.exists())
                    self.assertFalse((prefix / "transaction.json").exists())
                    self.assertEqual(config_sentinel.read_text(), "preserve-config\n")
                    self.assertEqual(
                        registry_sentinel.read_text(), "preserve-registry\n"
                    )

    def test_root_cli_status_preserves_pending_recovery_for_owner(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            config = root / "ha"
            config.mkdir()
            records = {
                "protocol": _source(root, "protocol", "1" * 40),
                "home-assistant": _source(root, "home-assistant", "2" * 40),
            }
            first = core.install_cohort(
                prefix, _cohort(records, ("protocol",)), records, _build
            )
            child = os.fork()
            if child == 0:
                with patch(
                    "companions.core._transaction_checkpoint",
                    side_effect=lambda observed: os._exit(77)
                    if observed == "active"
                    else None,
                ):
                    core.install_cohort(
                        prefix,
                        _cohort(records, ("home-assistant",)),
                        records,
                        _build,
                        target_binding={"ha_config": str(config)},
                    )
                os._exit(0)
            _pid, wait_status = os.waitpid(child, 0)
            self.assertEqual(os.waitstatus_to_exitcode(wait_status), 77)
            state_before = self._state(prefix, config)
            with (
                patch("companions.cli.os.geteuid", return_value=0),
                contextlib.redirect_stdout(io.StringIO()) as output,
            ):
                self.assertEqual(
                    cli.main(["status", "--prefix", str(prefix)]),
                    2,
                    output.getvalue(),
                )
            self.assertEqual(self._state(prefix, config), state_before)
            recovered = core.status(prefix)
            self.assertEqual(recovered["active_release"], first["active_release"])
            self.assertEqual(recovered["recovery"], "rolled-back")

    def test_core_status_root_guard_precedes_prefix_creation(self):
        with tempfile.TemporaryDirectory() as temporary:
            prefix = Path(temporary) / "absent" / "prefix"
            with (
                patch("companions.core.os.geteuid", return_value=0),
                self.assertRaisesRegex(core.BootstrapError, "must not run as root"),
            ):
                core.status(prefix)
            self.assertFalse(prefix.parent.exists())

    def test_cli_identical_production_noop_is_locked_and_skips_all_prerequisites(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            prefix = root / "prefix"
            record = _source(root, "protocol", "1" * 40)
            component = _component("protocol", record)
            raw_catalog = {
                "schema_version": 1,
                "cohorts": [
                    {
                        "product_version": "2026.36.2",
                        "publication_status": "published",
                        "admitted_hub_versions": [],
                        "components": _catalog_components({"protocol": record}),
                    }
                ],
            }
            cohort = core.select_cohort(
                core.parse_catalog(raw_catalog),
                ("protocol",),
                "2026.36.2",
                allow_candidates=False,
                update=False,
            )
            installed = core.install_cohort(
                prefix,
                cohort,
                {"protocol": record},
                _build,
                installation_mode="production-git",
                hub_version="2026.36.2",
            )
            catalog = root / "catalog.json"
            catalog.write_text(json.dumps(raw_catalog))
            real_prepare = targets.prepare_target_transition
            observed_lock = []

            def prepare(*args, **kwargs):
                with self.assertRaisesRegex(core.BootstrapError, "busy"):
                    with core.PrefixLock(prefix):
                        pass
                observed_lock.append(True)
                return real_prepare(*args, **kwargs)

            with (
                patch(
                    "companions.cli._download_sources",
                    side_effect=AssertionError("offline transport must not run"),
                ) as download,
                patch(
                    "companions.cli.check_recipe_environment",
                    side_effect=AssertionError("build tool probe must not run"),
                ) as tool_probe,
                patch(
                    "companions.targets.prepare_target_transition", side_effect=prepare
                ),
                contextlib.redirect_stdout(io.StringIO()) as output,
            ):
                code = cli.main(
                    [
                        "install",
                        "--prefix",
                        str(prefix),
                        "--components",
                        "protocol",
                        "--hub-version",
                        "2026.36.2",
                        "--catalog",
                        str(catalog),
                    ]
                )
            self.assertEqual(code, 0, output.getvalue())
            response = json.loads(output.getvalue())
            self.assertEqual(response["status"], "no-op")
            self.assertEqual(response["active_release"], installed["active_release"])
            self.assertEqual(observed_lock, [True])
            download.assert_not_called()
            tool_probe.assert_not_called()


class ProcessAndArtifactTests(unittest.TestCase):
    def _assert_dead(self, pid: int):
        deadline = time.monotonic() + 2
        while _alive(pid) and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertFalse(_alive(pid), f"owned descendant {pid} survived")

    def test_timeout_and_clean_leader_exit_drain_sigterm_resistant_descendants(self):
        for leader_sleep in (30, 0):
            with (
                self.subTest(leader_sleep=leader_sleep),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary)
                pidfile = root / "child.pid"
                child_code = (
                    "import os,signal,time,pathlib;"
                    "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
                    f"pathlib.Path({str(pidfile)!r}).write_text(str(os.getpid()));"
                    "time.sleep(30)"
                )
                parent_code = (
                    "import subprocess,sys,time;"
                    f"subprocess.Popen([sys.executable,'-c',{child_code!r}]);"
                    f"time.sleep({leader_sleep})"
                )
                try:
                    with self.assertRaises(ProcessFailure):
                        run_process(
                            [sys.executable, "-c", parent_code],
                            cwd=root,
                            timeout_seconds=1,
                            grace_seconds=0.2,
                        )
                    pid = int(pidfile.read_text())
                    self._assert_dead(pid)
                finally:
                    if pidfile.exists():
                        pid = int(pidfile.read_text())
                        if _alive(pid):
                            os.kill(pid, signal.SIGKILL)

    def test_git_helper_timeout_uses_the_same_owned_group_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tools = root / "tools"
            tools.mkdir()
            pidfile = root / "git-helper.pid"
            fake = tools / "git"
            helper_code = (
                "import os,signal,time,pathlib;"
                "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
                f"pathlib.Path({str(pidfile)!r}).write_text(str(os.getpid()));"
                "time.sleep(30)"
            )
            fake.write_text(
                f"#!{sys.executable}\n"
                "import os,signal,subprocess,sys,time\n"
                f"code={helper_code!r}\n"
                "subprocess.Popen([sys.executable,'-c',code])\n"
                "time.sleep(30)\n"
            )
            fake.chmod(0o755)
            old_path = os.environ.get("PATH", "")
            try:
                os.environ["PATH"] = str(tools) + os.pathsep + old_path
                with self.assertRaisesRegex(core.BootstrapError, "Git source fetch"):
                    core.fetch_git_source(
                        recipes.KNOWN_REPOSITORIES["protocol"],
                        "1" * 40,
                        root / "checkout",
                        "0" * 64,
                        timeout_seconds=1,
                    )
                pid = int(pidfile.read_text())
                self._assert_dead(pid)
            finally:
                os.environ["PATH"] = old_path
                if pidfile.exists():
                    pid = int(pidfile.read_text())
                    if _alive(pid):
                        os.kill(pid, signal.SIGKILL)

    def test_interruption_drains_only_the_owned_process_group(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pidfile = root / "descendant.pid"
            ready = root / "runner-ready"
            result = root / "runner-result"
            child_code = (
                "import os,signal,time,pathlib;"
                "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
                f"pathlib.Path({str(pidfile)!r}).write_text(str(os.getpid()));"
                "time.sleep(30)"
            )
            leader_code = (
                "import subprocess,sys,time;"
                f"subprocess.Popen([sys.executable,'-c',{child_code!r}]);"
                "time.sleep(30)"
            )
            worker = os.fork()
            if worker == 0:
                ready.write_text("ready")
                try:
                    run_process(
                        [sys.executable, "-c", leader_code],
                        cwd=root,
                        timeout_seconds=30,
                        grace_seconds=0.2,
                    )
                except KeyboardInterrupt:
                    result.write_text("interrupted")
                    os._exit(0)
                except BaseException as error:
                    result.write_text(type(error).__name__)
                    os._exit(2)
                os._exit(3)
            try:
                deadline = time.monotonic() + 3
                while (
                    not ready.exists() or not pidfile.exists()
                ) and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue(pidfile.exists())
                descendant = int(pidfile.read_text())
                os.kill(worker, signal.SIGINT)
                _pid, wait_status = os.waitpid(worker, 0)
                self.assertEqual(os.waitstatus_to_exitcode(wait_status), 0)
                self.assertEqual(result.read_text(), "interrupted")
                self._assert_dead(descendant)
            finally:
                try:
                    os.kill(worker, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                if pidfile.exists():
                    descendant = int(pidfile.read_text())
                    if _alive(descendant):
                        os.kill(descendant, signal.SIGKILL)

if __name__ == "__main__":
    unittest.main()
