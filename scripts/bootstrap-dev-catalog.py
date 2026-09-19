#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Create the private B1 Viewer local-candidate catalog."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import sys
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

from companions.recipes import (  # noqa: E402
    EXPECTED_PROFILES,
    KNOWN_REPOSITORIES,
    SDK_TARBALL_SHA256,
    VIEWER_ASSET_MANIFEST_SHA256,
    VIEWER_PACKAGE_SHA256,
)


COMPONENTS = ("sdk-typescript", "viewer")
HEX40 = re.compile(r"^[0-9a-f]{40}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
VERSION = re.compile(r"^[0-9]{4}\.[0-9]{1,2}\.[0-9]+$")


class CatalogInputError(ValueError):
    """The private source binding cannot form an admitted local candidate."""


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--local-sources", type=Path, required=True)
    parser.add_argument("--source", action="append", default=[])
    parser.add_argument("--hub-version", required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def source_bindings(values: list[str]) -> dict[str, Path]:
    bindings: dict[str, Path] = {}
    for value in values:
        name, separator, raw_path = value.partition("=")
        path = Path(raw_path)
        if (
            separator != "="
            or name not in COMPONENTS
            or name in bindings
            or not path.is_absolute()
            or not path.is_dir()
        ):
            raise CatalogInputError(
                "--source must bind each selected component to an existing absolute directory"
            )
        bindings[name] = path
    if set(bindings) != set(COMPONENTS):
        raise CatalogInputError("--source must bind sdk-typescript and viewer exactly once")
    return bindings


def read_manifest(path: Path) -> dict[str, dict[str, str]]:
    if not path.is_absolute() or not path.is_file():
        raise CatalogInputError("--local-sources must name an existing absolute file")
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise CatalogInputError("local source manifest could not be read") from error
    records = raw.get("components") if isinstance(raw, dict) else None
    if (
        not isinstance(raw, dict)
        or raw.get("schema_version") != 1
        or not isinstance(records, dict)
        or not set(COMPONENTS).issubset(records)
    ):
        raise CatalogInputError("local source manifest component set mismatch")
    result: dict[str, dict[str, str]] = {}
    for name in COMPONENTS:
        record = records[name]
        if not isinstance(record, dict):
            raise CatalogInputError("local source manifest record is invalid")
        repository = record.get("repository")
        commit = record.get("commit")
        source_sha256 = record.get("source_sha256")
        if (
            repository != KNOWN_REPOSITORIES[name]
            or not isinstance(commit, str)
            or not HEX40.fullmatch(commit)
            or not isinstance(source_sha256, str)
            or not HEX64.fullmatch(source_sha256)
        ):
            raise CatalogInputError("local source manifest record is invalid")
        result[name] = {
            "repository": repository,
            "commit": commit,
            "source_sha256": source_sha256,
        }
    return result


def source_metadata(source: Path, hub_version: str, name: str) -> None:
    try:
        package = json.loads((source / "package.json").read_text(encoding="utf-8"))
        compatibility = json.loads(
            (source / "compatibility" / "hub.json").read_text(encoding="utf-8")
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise CatalogInputError(f"{name} source metadata could not be read") from error
    if (
        not isinstance(package, dict)
        or package.get("version") != hub_version
        or not isinstance(compatibility, dict)
        or compatibility.get("status") != "candidate"
        or compatibility.get("product_version") != hub_version
        or compatibility.get("profile") != EXPECTED_PROFILES[name]
    ):
        raise CatalogInputError(f"{name} source metadata is not the current Hub candidate")


def artifacts(name: str, hub_version: str) -> dict[str, str]:
    sdk_package = f"teslatlas-sdk-{hub_version}.tgz"
    if name == "sdk-typescript":
        return {
            "package_filename": sdk_package,
            "package_sha256": SDK_TARBALL_SHA256,
        }
    return {
        "package_filename": f"teslatlas-viewer-{hub_version}.tgz",
        "package_sha256": VIEWER_PACKAGE_SHA256,
        "asset_manifest_sha256": VIEWER_ASSET_MANIFEST_SHA256,
        "sdk_package_filename": sdk_package,
        "sdk_package_sha256": SDK_TARBALL_SHA256,
    }


def catalog(
    records: dict[str, dict[str, str]], bindings: dict[str, Path], hub_version: str
) -> dict[str, Any]:
    if not VERSION.fullmatch(hub_version):
        raise CatalogInputError("--hub-version must use the Hub calendar version format")
    components: dict[str, dict[str, Any]] = {}
    for name in COMPONENTS:
        source_metadata(bindings[name], hub_version, name)
        components[name] = {
            **records[name],
            "product_version": hub_version,
            "profile": EXPECTED_PROFILES[name],
            "artifacts": artifacts(name, hub_version),
        }
    return {
        "schema_version": 1,
        "cohorts": [
            {
                "product_version": hub_version,
                "publication_status": "local-unpublished",
                "admitted_hub_versions": [hub_version],
                "components": components,
            }
        ],
    }


def write_catalog(path: Path, value: dict[str, Any]) -> None:
    if not path.is_absolute() or path.exists() or not path.parent.is_dir():
        raise CatalogInputError("--output must be a new file below an existing absolute directory")
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        json.dump(value, stream, sort_keys=True, separators=(",", ":"))
        stream.write("\n")


def main() -> int:
    try:
        arguments = parse_arguments()
        bindings = source_bindings(arguments.source)
        records = read_manifest(arguments.local_sources)
        write_catalog(arguments.output, catalog(records, bindings, arguments.hub_version))
    except CatalogInputError as error:
        print(f"bootstrap-dev-catalog: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
