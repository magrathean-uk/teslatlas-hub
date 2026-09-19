#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Exercise the exact five-companion local-candidate catalog generator."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
GENERATOR = ROOT / "scripts" / "bootstrap-dev-catalog.py"
sys.path.insert(0, str(ROOT / "tools"))
from companions.core import (  # noqa: E402
    CatalogError,
    parse_catalog,
    select_cohort,
    source_manifest_for,
)
from companions.recipes import EXPECTED_PROFILES, KNOWN_REPOSITORIES  # noqa: E402

VERSION = "2026.36.2"
SDK_SHA256 = "42348d3688c5a723bd154e3c1e8172bc07b20d1bf28944818ccfdbf3d97891f7"
COMPONENTS = tuple(KNOWN_REPOSITORIES)


class BootstrapDevCatalogTests(unittest.TestCase):
    def test_generates_and_reads_back_exact_five_companion_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = {name: self._source(root, name) for name in COMPONENTS}
            manifest = root / "local-sources.json"
            manifest.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "components": {
                            name: self._record(name, source, marker)
                            for (name, source), marker in zip(sources.items(), "abcde")
                        },
                    }
                ),
                encoding="utf-8",
            )
            output = root / "catalog.json"

            result = self._run_generator(manifest, sources, output)

            self.assertEqual(result.returncode, 0, result.stderr)
            value = json.loads(output.read_text(encoding="utf-8"))
            catalog = parse_catalog(value)
            cohort = value["cohorts"][0]
            self.assertEqual(set(COMPONENTS), set(cohort["components"]))
            self.assertEqual(cohort["publication_status"], "local-unpublished")
            self.assertEqual(cohort["admitted_hub_versions"], [VERSION])
            selected = select_cohort(
                catalog,
                ("protocol", "sdk-typescript"),
                VERSION,
                allow_candidates=True,
                update=False,
            )
            self.assertEqual(set(selected.components), {"protocol", "sdk-typescript"})
            self.assertEqual(
                cohort["components"]["sdk-typescript"]["artifacts"],
                {
                    "package_filename": f"teslatlas-sdk-{VERSION}.tgz",
                    "package_sha256": SDK_SHA256,
                },
            )
            self.assertEqual(
                cohort["components"]["home-assistant"]["artifacts"],
                {
                    "payload_manifest_sha256": "f" * 64,
                    "selection_receipt_sha256": "0" * 64,
                },
            )

    def test_missing_active_component_fails_without_writing_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = {name: self._source(root, name) for name in COMPONENTS}
            records = {
                name: self._record(name, source, marker)
                for (name, source), marker in zip(sources.items(), "abcde")
            }
            records.pop("edge")
            manifest = root / "local-sources.json"
            manifest.write_text(
                json.dumps({"schema_version": 1, "components": records}),
                encoding="utf-8",
            )
            output = root / "catalog.json"

            result = self._run_generator(manifest, sources, output)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("component set mismatch", result.stderr)
            self.assertFalse(output.exists())

    def test_parser_rejects_deferred_product(self) -> None:
        catalog = parse_catalog({"schema_version": 1, "cohorts": []})
        with self.assertRaisesRegex(CatalogError, "unknown components"):
            select_cohort(
                catalog,
                ("deferred-product",),
                VERSION,
                allow_candidates=True,
                update=False,
            )

    def test_metadata_compatible_but_byte_mismatched_binding_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = {name: self._source(root, name) for name in COMPONENTS}
            records = {
                name: self._record(name, source, marker)
                for (name, source), marker in zip(sources.items(), "abcde")
            }
            replacement = self._source(root / "replacement", "protocol")
            (replacement / "README.md").write_text("different bytes\n", encoding="utf-8")
            records["protocol"]["path"] = str(replacement.resolve())
            sources["protocol"] = replacement.resolve()
            manifest = root / "local-sources.json"
            manifest.write_text(
                json.dumps({"schema_version": 1, "components": records}),
                encoding="utf-8",
            )
            output = root / "catalog.json"

            result = self._run_generator(manifest, sources, output)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not bind the supplied protocol source bytes", result.stderr)
            self.assertNotIn("Traceback", result.stderr)
            self.assertFalse(output.exists())

    def test_malformed_version_metadata_fails_without_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sources = {name: self._source(root, name) for name in COMPONENTS}
            (sources["sdk-typescript"] / "package.json").write_text(
                "[]\n", encoding="utf-8"
            )
            records = {
                name: self._record(name, source, marker)
                for (name, source), marker in zip(sources.items(), "abcde")
            }
            manifest = root / "local-sources.json"
            manifest.write_text(
                json.dumps({"schema_version": 1, "components": records}),
                encoding="utf-8",
            )
            output = root / "catalog.json"

            result = self._run_generator(manifest, sources, output)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("sdk-typescript source metadata could not be read", result.stderr)
            self.assertNotIn("Traceback", result.stderr)
            self.assertFalse(output.exists())

    def _run_generator(
        self, manifest: Path, sources: dict[str, Path], output: Path
    ) -> subprocess.CompletedProcess[str]:
        command = [
            sys.executable,
            str(GENERATOR),
            "--local-sources",
            str(manifest),
        ]
        for name, source in sources.items():
            command.extend(("--source", f"{name}={source}"))
        command.extend(
            (
                "--hub-version",
                VERSION,
                "--sdk-package-sha256",
                SDK_SHA256,
                "--ha-payload-manifest-sha256",
                "f" * 64,
                "--ha-selection-receipt-sha256",
                "0" * 64,
                "--output",
                str(output),
            )
        )
        return subprocess.run(command, check=False, capture_output=True, text=True)

    @staticmethod
    def _source(root: Path, name: str) -> Path:
        source = root / name
        (source / "compatibility").mkdir(parents=True)
        if name == "sdk-typescript":
            (source / "package.json").write_text(
                json.dumps({"name": "@teslatlas/sdk", "version": VERSION}),
                encoding="utf-8",
            )
        elif name in {"protocol", "home-assistant"}:
            (source / "pyproject.toml").write_text(
                f'[project]\nversion = "{VERSION}"\n', encoding="utf-8"
            )
        elif name == "sdk-swift":
            (source / "VERSION").write_text(f"{VERSION}\n", encoding="utf-8")
        else:
            (source / "Cargo.toml").write_text(
                f'[package]\nversion = "{VERSION}"\n', encoding="utf-8"
            )
        (source / "compatibility" / "hub.json").write_text(
            json.dumps(
                {
                    "status": "candidate",
                    "product_version": VERSION,
                    "profile": EXPECTED_PROFILES[name],
                }
            ),
            encoding="utf-8",
        )
        return source.resolve()

    @staticmethod
    def _record(name: str, source: Path, marker: str) -> dict:
        return source_manifest_for(
            name,
            source,
            KNOWN_REPOSITORIES[name],
            marker * 40,
        )


if __name__ == "__main__":
    unittest.main()
