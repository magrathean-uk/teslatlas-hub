#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Exercise the B1 local-candidate catalog generator."""

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
from companions.core import parse_catalog, select_cohort  # noqa: E402

PROFILE = {
    "id": "hub-http-v1",
    "revision": "1.0.0",
    "sha256": "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926",
}


class BootstrapDevCatalogTests(unittest.TestCase):
    def test_generates_a_parser_accepted_viewer_cohort_from_bound_sources(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sdk = self._source(root, "sdk-typescript")
            viewer = self._source(root, "viewer")
            manifest = root / "local-sources.json"
            manifest.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "components": {
                            "sdk-typescript": self._record("sdk-typescript", "a"),
                            "viewer": self._record("viewer", "b"),
                        },
                    }
                ),
                encoding="utf-8",
            )
            output = root / "catalog.json"

            result = subprocess.run(
                [
                    sys.executable,
                    str(GENERATOR),
                    "--local-sources",
                    str(manifest),
                    "--source",
                    f"sdk-typescript={sdk}",
                    "--source",
                    f"viewer={viewer}",
                    "--hub-version",
                    "2026.36.2",
                    "--output",
                    str(output),
                ],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            catalog = json.loads(output.read_text(encoding="utf-8"))
            selected = select_cohort(
                parse_catalog(catalog),
                ("sdk-typescript", "viewer"),
                "2026.36.2",
                allow_candidates=True,
                update=False,
            )
            self.assertEqual(selected.product_version, "2026.36.2")
            cohort = catalog["cohorts"][0]
            self.assertEqual(cohort["publication_status"], "local-unpublished")
            self.assertEqual(cohort["admitted_hub_versions"], ["2026.36.2"])
            self.assertEqual(
                cohort["components"]["sdk-typescript"]["source_sha256"], "a" * 64
            )
            self.assertEqual(
                cohort["components"]["viewer"]["source_sha256"], "b" * 64
            )
            self.assertEqual(
                cohort["components"]["sdk-typescript"]["artifacts"],
                {
                    "package_filename": "teslatlas-sdk-2026.36.2.tgz",
                    "package_sha256": "03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c",
                },
            )
            self.assertEqual(
                cohort["components"]["viewer"]["artifacts"],
                {
                    "package_filename": "teslatlas-viewer-2026.36.2.tgz",
                    "package_sha256": "f92becdbeb0132b34fa8a674c261a2cd4d4eafc6361b29be5396e854bcf2cdc2",
                    "asset_manifest_sha256": "6d417a87a566d7b450e5556895e12b65af353fe27219dd8859bb0dd69e135e06",
                    "sdk_package_filename": "teslatlas-sdk-2026.36.2.tgz",
                    "sdk_package_sha256": "03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c",
                },
            )

    @staticmethod
    def _source(root: Path, name: str) -> Path:
        source = root / name
        (source / "compatibility").mkdir(parents=True)
        (source / "package.json").write_text(
            json.dumps({"name": name, "version": "2026.36.2"}), encoding="utf-8"
        )
        (source / "compatibility" / "hub.json").write_text(
            json.dumps({"status": "candidate", "product_version": "2026.36.2", "profile": PROFILE}),
            encoding="utf-8",
        )
        return source

    @staticmethod
    def _record(name: str, marker: str) -> dict[str, str]:
        return {
            "repository": f"https://github.com/magrathean-uk/teslatlas-{name}.git",
            "commit": marker * 40,
            "source_sha256": marker * 64,
        }


if __name__ == "__main__":
    unittest.main()
