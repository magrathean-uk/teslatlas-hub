#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Create one exact five-companion unpublished local-candidate catalog."""

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

from companions.core import BootstrapError, parse_catalog, source_manifest_for  # noqa: E402
from companions.recipes import (  # noqa: E402
    EXPECTED_PROFILES,
    KNOWN_REPOSITORIES,
)


COMPONENTS = tuple(KNOWN_REPOSITORIES)
HEX64 = re.compile(r"^[0-9a-f]{64}$")
VERSION = re.compile(r"^[0-9]{4}\.[0-9]{1,2}\.[0-9]+$")


class CatalogInputError(ValueError):
    """The private source binding cannot form an admitted local candidate."""


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--local-sources", type=Path, required=True)
    parser.add_argument("--source", action="append", default=[])
    parser.add_argument("--hub-version", required=True)
    parser.add_argument("--sdk-package-sha256", required=True)
    parser.add_argument("--ha-payload-manifest-sha256", required=True)
    parser.add_argument("--ha-selection-receipt-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def source_bindings(values: list[str]) -> dict[str, Path]:
    bindings: dict[str, Path] = {}
    for value in values:
        name, separator, raw_path = value.partition("=")
        path = Path(raw_path)
        try:
            resolved = path.resolve(strict=True)
        except OSError:
            resolved = path
        if (
            separator != "="
            or name not in COMPONENTS
            or name in bindings
            or not path.is_absolute()
            or not path.is_dir()
            or resolved != path
        ):
            raise CatalogInputError(
                "--source must bind each selected component to an existing absolute directory"
            )
        bindings[name] = resolved
    if set(bindings) != set(COMPONENTS):
        raise CatalogInputError("--source must bind all five active companions exactly once")
    return bindings


def read_manifest(
    path: Path, bindings: dict[str, Path]
) -> dict[str, dict[str, str]]:
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
        or set(records) != set(COMPONENTS)
    ):
        raise CatalogInputError("local source manifest component set mismatch")
    result: dict[str, dict[str, str]] = {}
    for name in COMPONENTS:
        record = records[name]
        try:
            repository = record["repository"]
            commit = record["commit"]
            observed = source_manifest_for(name, bindings[name], repository, commit)
        except (BootstrapError, KeyError, TypeError, OSError) as error:
            raise CatalogInputError("local source manifest record is invalid") from error
        if record != observed:
            raise CatalogInputError(
                f"local source manifest does not bind the supplied {name} source bytes"
            )
        source_sha256 = observed["source_sha256"]
        result[name] = {
            "repository": repository,
            "commit": commit,
            "source_sha256": source_sha256,
        }
    return result


def source_product_version(source: Path, name: str) -> str:
    if name == "sdk-typescript":
        return json.loads((source / "package.json").read_text(encoding="utf-8"))["version"]
    if name in {"protocol", "home-assistant"}:
        contents = (source / "pyproject.toml").read_text(encoding="utf-8")
        match = re.search(r'^version\s*=\s*"([^"]+)"', contents, re.MULTILINE)
        if match is None:
            raise CatalogInputError(f"{name} source product version is missing")
        return match.group(1)
    if name == "sdk-swift":
        return (source / "VERSION").read_text(encoding="utf-8").strip()
    contents = (source / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', contents, re.MULTILINE)
    if match is None:
        raise CatalogInputError(f"{name} source product version is missing")
    return match.group(1)


def source_metadata(source: Path, hub_version: str, name: str) -> None:
    try:
        compatibility = json.loads(
            (source / "compatibility" / "hub.json").read_text(encoding="utf-8")
        )
        product_version = source_product_version(source, name)
    except (OSError, UnicodeError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise CatalogInputError(f"{name} source metadata could not be read") from error
    if (
        product_version != hub_version
        or not isinstance(compatibility, dict)
        or compatibility.get("status") not in {"accepted", "candidate"}
        or compatibility.get("product_version") != hub_version
        or compatibility.get("profile") != EXPECTED_PROFILES[name]
    ):
        raise CatalogInputError(f"{name} source metadata is not the current Hub candidate")


def artifacts(
    name: str,
    hub_version: str,
    sdk_package_sha256: str,
    ha_payload_manifest_sha256: str,
    ha_selection_receipt_sha256: str,
) -> dict[str, str]:
    if name == "sdk-typescript":
        return {
            "package_filename": f"teslatlas-sdk-{hub_version}.tgz",
            "package_sha256": sdk_package_sha256,
        }
    if name == "home-assistant":
        return {
            "payload_manifest_sha256": ha_payload_manifest_sha256,
            "selection_receipt_sha256": ha_selection_receipt_sha256,
        }
    return {}


def catalog(
    records: dict[str, dict[str, str]],
    bindings: dict[str, Path],
    hub_version: str,
    sdk_package_sha256: str,
    ha_payload_manifest_sha256: str,
    ha_selection_receipt_sha256: str,
) -> dict[str, Any]:
    if not VERSION.fullmatch(hub_version):
        raise CatalogInputError("--hub-version must use the Hub calendar version format")
    components: dict[str, dict[str, Any]] = {}
    for name in COMPONENTS:
        source_metadata(bindings[name], hub_version, name)
        component = {
            **records[name],
            "product_version": hub_version,
            "profile": EXPECTED_PROFILES[name],
        }
        component_artifacts = artifacts(
            name,
            hub_version,
            sdk_package_sha256,
            ha_payload_manifest_sha256,
            ha_selection_receipt_sha256,
        )
        if component_artifacts:
            component["artifacts"] = component_artifacts
        components[name] = component
    value = {
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
    parse_catalog(value)
    return value


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
        for value in (
            arguments.sdk_package_sha256,
            arguments.ha_payload_manifest_sha256,
            arguments.ha_selection_receipt_sha256,
        ):
            if not HEX64.fullmatch(value):
                raise CatalogInputError("artifact hashes must be lowercase SHA-256 values")
        bindings = source_bindings(arguments.source)
        records = read_manifest(arguments.local_sources, bindings)
        write_catalog(
            arguments.output,
            catalog(
                records,
                bindings,
                arguments.hub_version,
                arguments.sdk_package_sha256,
                arguments.ha_payload_manifest_sha256,
                arguments.ha_selection_receipt_sha256,
            ),
        )
    except CatalogInputError as error:
        print(f"bootstrap-dev-catalog: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
