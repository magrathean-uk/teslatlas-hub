#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only

"""Check or align the allowlisted Teslatlas ecosystem product-version fields."""

from __future__ import annotations

import argparse
import hashlib
import json
import plistlib
import re
import sys
import tarfile
from dataclasses import dataclass
from datetime import date
from pathlib import Path
from typing import Any, Protocol


PRODUCT_VERSION_RE = re.compile(r"^(0|[1-9][0-9]*)\.([1-9]|[1-4][0-9]|5[0-3])\.([1-9][0-9]*)$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SOURCE_FINGERPRINT_RE = re.compile(
    r"^git:[0-9a-f]{40,64};tracked-diff-sha256:[0-9a-f]{64};"
    r"(?:content|snapshot)-manifest-sha256:[0-9a-f]{64}$"
)

G3_RECEIPTS = {
    "hub/docs/development/active-macos-arm64-hub-typescript-g2-g4-2026-09-18-r1.json":
        "6efec1707c3a703f31c5d7db3c84fa2d44c9fdff49afe22644aa3cf863eb2a7b",
    "teslatlas-sdk-typescript/docs/development/macos-arm64-packed-node-browser-g4-2026-09-18-r1.json":
        "35a5881ffceb5ee4db3157bddc2758cbbc4e1ce25b8e96bafd1e34eab6f008e5",
    "hub/docs/development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json":
        "d3e78ff9f98acdb45b6d691c295df80e9fb2fbc70eb5b189c6a575ecd6e2ac5c",
    "teslatlas-edge/docs/development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json":
        "42209a8231e4ac6881a3fc4896a95573e8ba032dbb990ea747ea877e19e485f7",
    "hub/docs/development/g6-macos-arm64-swift-hub-acceptance-2026-09-19-r2.json":
        "37ee4c1376778392c94da1457c31b1b6e69210cb4c4d504b6f08d520a1d93bca",
    "teslatlas-sdk-swift/docs/development/g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json":
        "dd5cfb0087f0bcf89bf8a1ef33b7bcf2fd3a28430da373e78a1bb5862472fea8",
}

G2_HUB_SOURCE = (
    "git:7fe8cb202c0eb7e291901a9b719d3fbe65c675bc;"
    "tracked-diff-sha256:8c3fb77fd6f540ad41e8d0b417c5e4427b05389399e9c2cb335a2faed6d243ca;"
    "content-manifest-sha256:e84233ed9641f525f6e4dba328700153983297932a277ebc544a75cc841cf823"
)
G5_HUB_SOURCE = (
    "git:7fe8cb202c0eb7e291901a9b719d3fbe65c675bc;"
    "tracked-diff-sha256:8c3fb77fd6f540ad41e8d0b417c5e4427b05389399e9c2cb335a2faed6d243ca;"
    "snapshot-manifest-sha256:f3e9f240930c72909d982ad2f6429c4f6ab390eb34818505e92236f6027d443b"
)
G6_HUB_SOURCE = (
    "git:7fe8cb202c0eb7e291901a9b719d3fbe65c675bc;"
    "tracked-diff-sha256:8c3fb77fd6f540ad41e8d0b417c5e4427b05389399e9c2cb335a2faed6d243ca;"
    "content-manifest-sha256:42f1787eb94ad81595fad1e5795d547633d0fea848735dfd47ab4e86ed53249d"
)

HUB_PROFILE_SHA256 = "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"
EDGE_PROFILE_SHA256 = "e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224"

G3_PRODUCT_VERSION = "2026.36.2"
G3_G2_RECEIPTS = (
    "hub/docs/development/active-macos-arm64-hub-typescript-g2-g4-2026-09-18-r1.json",
    "teslatlas-sdk-typescript/docs/development/macos-arm64-packed-node-browser-g4-2026-09-18-r1.json",
)
G3_G5_RECEIPTS = (
    "hub/docs/development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json",
    "teslatlas-edge/docs/development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json",
)
G3_G6_RECEIPTS = (
    "hub/docs/development/g6-macos-arm64-swift-hub-acceptance-2026-09-19-r2.json",
    "teslatlas-sdk-swift/docs/development/g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json",
)
G3_PROFILE_BINDINGS = {
    "teslatlas-protocol/profiles/hub-http-v1/1.0.0/SHA256SUMS": HUB_PROFILE_SHA256,
    "teslatlas-sdk-typescript/protocol/source/profiles/hub-http-v1/1.0.0/SHA256SUMS":
        HUB_PROFILE_SHA256,
    "teslatlas-sdk-swift/Sources/TeslatlasCurrentHub/Binding/SHA256SUMS":
        HUB_PROFILE_SHA256,
    "teslatlas-protocol/profiles/edge-delivery-v2/2.0.0/SHA256SUMS": EDGE_PROFILE_SHA256,
}


def parse_product_version(value: str) -> tuple[int, int, int]:
    """Return a numerically comparable calendar product version."""
    match = PRODUCT_VERSION_RE.fullmatch(value)
    if match is None:
        raise ValueError(
            f"invalid product version {value!r}; expected YEAR.WEEK.REVISION without leading zeros"
        )
    year, week, revision = (int(part) for part in match.groups())
    if year < 1:
        raise ValueError(f"invalid product version {value!r}; year must be positive")
    return year, week, revision


def iso_release_week(release_date: date) -> str:
    iso = release_date.isocalendar()
    return f"{iso.year}.{iso.week}"


def check_cli_version(
    stdout: str, stderr: str, executable_name: str, expected_version: str
) -> None:
    parse_product_version(expected_version)
    expected_stdout = f"{executable_name} {expected_version}\n"
    if stdout != expected_stdout or stderr != "":
        raise ValueError(
            f"embedded CLI version mismatch: expected stdout {expected_stdout!r} and empty stderr"
        )


def check_debian_version(package_version: str, expected_version: str) -> None:
    parse_product_version(expected_version)
    if re.fullmatch(re.escape(expected_version) + r"-[1-9][0-9]*", package_version) is None:
        raise ValueError(
            f"Debian Version must be {expected_version}-PACKAGE_REVISION, found {package_version!r}"
        )


def check_npm_tarball_version(archive: Path, expected_version: str) -> None:
    parse_product_version(expected_version)
    with tarfile.open(archive, "r:gz") as package:
        try:
            member = package.getmember("package/package.json")
        except KeyError as error:
            raise ValueError("npm tarball does not contain package/package.json") from error
        extracted = package.extractfile(member)
        if extracted is None:
            raise ValueError("npm tarball package/package.json is not a regular file")
        metadata = json.loads(extracted.read())
    if metadata.get("version") != expected_version:
        raise ValueError(
            "npm tarball version mismatch: "
            f"expected {expected_version}, found {metadata.get('version')!r}"
        )


def _replace_span(text: str, start: int, end: int, replacement: str) -> str:
    return text[:start] + replacement + text[end:]


def _toml_section_bounds(text: str, section: str) -> tuple[int, int]:
    header = re.compile(rf"(?m)^\[{re.escape(section)}\]\s*$")
    match = header.search(text)
    if match is None:
        raise ValueError(f"missing TOML section [{section}]")
    next_header = re.search(r"(?m)^\[{1,2}[^\n]+\]{1,2}\s*$", text[match.end() :])
    end = len(text) if next_header is None else match.end() + next_header.start()
    return match.end(), end


def _replace_toml_section_string(text: str, section: str, key: str, value: str) -> str:
    start, end = _toml_section_bounds(text, section)
    pattern = re.compile(rf'(?m)^(\s*{re.escape(key)}\s*=\s*)"(?:\\.|[^"\\])*"(\s*(?:#.*)?)$')
    matches = list(pattern.finditer(text, start, end))
    if len(matches) != 1:
        raise ValueError(f"expected one TOML [{section}].{key} field, found {len(matches)}")
    match = matches[0]
    replacement = f'{match.group(1)}{json.dumps(value)}{match.group(2)}'
    return _replace_span(text, match.start(), match.end(), replacement)


def _read_toml_section_string(text: str, section: str, key: str) -> str:
    start, end = _toml_section_bounds(text, section)
    pattern = re.compile(rf'(?m)^\s*{re.escape(key)}\s*=\s*("(?:\\.|[^"\\])*")\s*(?:#.*)?$')
    matches = list(pattern.finditer(text, start, end))
    if len(matches) != 1:
        raise ValueError(f"expected one TOML [{section}].{key} field, found {len(matches)}")
    value = json.loads(matches[0].group(1))
    if not isinstance(value, str):
        raise ValueError(f"TOML [{section}].{key} is not a string")
    return value


def _toml_package_block(text: str, package_name: str) -> tuple[int, int]:
    blocks = list(re.finditer(r"(?ms)^\[\[package\]\]\s*\n.*?(?=^\[\[package\]\]|\Z)", text))
    matches: list[tuple[int, int]] = []
    for block in blocks:
        name = re.search(r'(?m)^name\s*=\s*"([^"\n]+)"\s*$', block.group())
        if name is not None and name.group(1) == package_name:
            matches.append((block.start(), block.end()))
    if len(matches) != 1:
        raise ValueError(f"expected one TOML package named {package_name}, found {len(matches)}")
    return matches[0]


def _replace_toml_package_string(text: str, package_name: str, key: str, value: str) -> str:
    start, end = _toml_package_block(text, package_name)
    pattern = re.compile(rf'(?m)^(\s*{re.escape(key)}\s*=\s*)"(?:\\.|[^"\\])*"(\s*(?:#.*)?)$')
    matches = list(pattern.finditer(text, start, end))
    if len(matches) != 1:
        raise ValueError(
            f"expected one {key} field in TOML package {package_name}, found {len(matches)}"
        )
    match = matches[0]
    replacement = f'{match.group(1)}{json.dumps(value)}{match.group(2)}'
    return _replace_span(text, match.start(), match.end(), replacement)


def _read_toml_package_string(text: str, package_name: str, key: str) -> str:
    start, end = _toml_package_block(text, package_name)
    pattern = re.compile(rf'(?m)^\s*{re.escape(key)}\s*=\s*("(?:\\.|[^"\\])*")\s*(?:#.*)?$')
    matches = list(pattern.finditer(text, start, end))
    if len(matches) != 1:
        raise ValueError(
            f"expected one {key} field in TOML package {package_name}, found {len(matches)}"
        )
    value = json.loads(matches[0].group(1))
    if not isinstance(value, str):
        raise ValueError(f"TOML package {package_name}.{key} is not a string")
    return value


class _JsonLocator:
    def __init__(self, text: str):
        self.text = text
        self.decoder = json.JSONDecoder()
        self.scalars: dict[tuple[str | int, ...], tuple[int, int, Any]] = {}

    def locate(self) -> dict[tuple[str | int, ...], tuple[int, int, Any]]:
        end = self._parse_value(self._skip(0), ())
        if self._skip(end) != len(self.text):
            raise ValueError("unexpected trailing JSON data")
        return self.scalars

    def _skip(self, index: int) -> int:
        while index < len(self.text) and self.text[index].isspace():
            index += 1
        return index

    def _parse_value(self, index: int, path: tuple[str | int, ...]) -> int:
        if index >= len(self.text):
            raise ValueError("unexpected end of JSON input")
        if self.text[index] == "{":
            end = self._parse_object(index, path)
            self.scalars[path] = (index, end, json.loads(self.text[index:end]))
            return end
        if self.text[index] == "[":
            end = self._parse_array(index, path)
            self.scalars[path] = (index, end, json.loads(self.text[index:end]))
            return end
        value, end = self.decoder.raw_decode(self.text, index)
        self.scalars[path] = (index, end, value)
        return end

    def _parse_object(self, index: int, path: tuple[str | int, ...]) -> int:
        index = self._skip(index + 1)
        if index < len(self.text) and self.text[index] == "}":
            return index + 1
        while True:
            key, index = self.decoder.raw_decode(self.text, index)
            if not isinstance(key, str):
                raise ValueError("JSON object key is not a string")
            index = self._skip(index)
            if index >= len(self.text) or self.text[index] != ":":
                raise ValueError("missing JSON object colon")
            index = self._parse_value(self._skip(index + 1), (*path, key))
            index = self._skip(index)
            if index < len(self.text) and self.text[index] == "}":
                return index + 1
            if index >= len(self.text) or self.text[index] != ",":
                raise ValueError("missing JSON object comma")
            index = self._skip(index + 1)

    def _parse_array(self, index: int, path: tuple[str | int, ...]) -> int:
        index = self._skip(index + 1)
        item = 0
        if index < len(self.text) and self.text[index] == "]":
            return index + 1
        while True:
            index = self._parse_value(index, (*path, item))
            item += 1
            index = self._skip(index)
            if index < len(self.text) and self.text[index] == "]":
                return index + 1
            if index >= len(self.text) or self.text[index] != ",":
                raise ValueError("missing JSON array comma")
            index = self._skip(index + 1)


def _json_values(text: str) -> dict[tuple[str | int, ...], tuple[int, int, Any]]:
    json.loads(text)
    return _JsonLocator(text).locate()


def _replace_json_strings(
    text: str, paths: tuple[tuple[str | int, ...], ...], value: str
) -> str:
    scalars = _json_values(text)
    spans = []
    for path in paths:
        located = scalars.get(path)
        if located is None:
            raise ValueError(f"missing JSON field {'.'.join(str(part) for part in path)}")
        start, end, current = located
        if not isinstance(current, str):
            raise ValueError(f"JSON field {'.'.join(str(part) for part in path)} is not a string")
        spans.append((start, end))
    for start, end in sorted(spans, reverse=True):
        text = _replace_span(text, start, end, json.dumps(value))
    return text


def _replace_json_values(
    text: str, replacements: dict[tuple[str | int, ...], Any]
) -> str:
    scalars = _json_values(text)
    spans = []
    for path, value in replacements.items():
        located = scalars.get(path)
        if located is None:
            raise ValueError(f"missing JSON field {'.'.join(str(part) for part in path)}")
        start, end, _ = located
        spans.append((start, end, json.dumps(value, separators=(",", ":"))))
    for start, end, replacement in sorted(spans, reverse=True):
        text = _replace_span(text, start, end, replacement)
    return text


def _yaml_scalar(text: str, path: tuple[str, ...]) -> tuple[str, int, int]:
    stack: list[tuple[int, str]] = []
    matches: list[tuple[str, int, int]] = []
    offset = 0
    for line in text.splitlines(keepends=True):
        match = re.match(r"^( *)([A-Za-z_][A-Za-z0-9_]*):(?:[ \t]*(.*?))?[ \t]*(?:\n)?$", line)
        if match is not None and len(match.group(1)) % 2 == 0:
            indent = len(match.group(1))
            while stack and stack[-1][0] >= indent:
                stack.pop()
            current_path = tuple(item[1] for item in stack) + (match.group(2),)
            raw = match.group(3) or ""
            if current_path == path and raw:
                value_start = offset + match.start(3)
                value_end = offset + match.end(3)
                try:
                    decoded = json.loads(raw)
                except json.JSONDecodeError:
                    decoded = raw
                if not isinstance(decoded, str):
                    raise ValueError(f"YAML field {'.'.join(path)} is not a string")
                matches.append((decoded, value_start, value_end))
            if not raw:
                stack.append((indent, match.group(2)))
        offset += len(line)
    if len(matches) != 1:
        raise ValueError(f"expected one YAML field {'.'.join(path)}, found {len(matches)}")
    return matches[0]


class VersionField(Protocol):
    label: str

    def read(self, workspace: Path) -> str: ...

    def apply(self, workspace: Path, version: str) -> None: ...


@dataclass(frozen=True)
class TomlSectionField:
    relative_path: str
    section: str
    key: str = "version"

    @property
    def label(self) -> str:
        return f"{self.relative_path}:{self.section}.{self.key}"

    def read(self, workspace: Path) -> str:
        path = workspace / self.relative_path
        return _read_toml_section_string(
            path.read_text(encoding="utf-8"), self.section, self.key
        )

    def apply(self, workspace: Path, version: str) -> None:
        path = workspace / self.relative_path
        text = path.read_text(encoding="utf-8")
        updated = _replace_toml_section_string(text, self.section, self.key, version)
        if updated != text:
            path.write_text(updated, encoding="utf-8")


@dataclass(frozen=True)
class TomlPackageField:
    relative_path: str
    package_name: str

    @property
    def label(self) -> str:
        return f"{self.relative_path}:package[{self.package_name}].version"

    def read(self, workspace: Path) -> str:
        path = workspace / self.relative_path
        return _read_toml_package_string(
            path.read_text(encoding="utf-8"), self.package_name, "version"
        )

    def apply(self, workspace: Path, version: str) -> None:
        path = workspace / self.relative_path
        text = path.read_text(encoding="utf-8")
        updated = _replace_toml_package_string(text, self.package_name, "version", version)
        if updated != text:
            path.write_text(updated, encoding="utf-8")


@dataclass(frozen=True)
class JsonField:
    relative_path: str
    paths: tuple[tuple[str | int, ...], ...]

    @property
    def label(self) -> str:
        return f"{self.relative_path}:{'.'.join(str(part) for part in self.paths[0])}"

    def read(self, workspace: Path) -> str:
        path = workspace / self.relative_path
        scalars = _json_values(path.read_text(encoding="utf-8"))
        values = []
        for field_path in self.paths:
            located = scalars.get(field_path)
            if located is None or not isinstance(located[2], str):
                raise ValueError(
                    f"{self.relative_path}:{'.'.join(str(part) for part in field_path)}: missing string value"
                )
            values.append(located[2])
        if len(set(values)) != 1:
            raise ValueError(f"{self.label}: fields disagree: {values}")
        return values[0]

    def apply(self, workspace: Path, version: str) -> None:
        path = workspace / self.relative_path
        text = path.read_text(encoding="utf-8")
        updated = _replace_json_strings(text, self.paths, version)
        if updated != text:
            path.write_text(updated, encoding="utf-8")


@dataclass(frozen=True)
class TextField:
    relative_path: str

    @property
    def label(self) -> str:
        return self.relative_path

    def read(self, workspace: Path) -> str:
        path = workspace / self.relative_path
        text = path.read_text(encoding="utf-8")
        if not text.endswith("\n") or "\n" in text[:-1]:
            raise ValueError(f"{self.label}: expected one newline-terminated version")
        return text[:-1]

    def apply(self, workspace: Path, version: str) -> None:
        path = workspace / self.relative_path
        updated = f"{version}\n"
        if path.read_text(encoding="utf-8") != updated:
            path.write_text(updated, encoding="utf-8")


@dataclass(frozen=True)
class SwiftGeneratedField:
    relative_path: str

    @property
    def label(self) -> str:
        return f"{self.relative_path}:teslatlasProductVersion"

    def read(self, workspace: Path) -> str:
        text = (workspace / self.relative_path).read_text(encoding="utf-8")
        expected_prefix = "// @generated by hub/scripts/sync-ecosystem-versions.py\n"
        match = re.fullmatch(
            re.escape(expected_prefix)
            + r'public let teslatlasProductVersion = "([^"]+)"\n',
            text,
        )
        if match is None:
            raise ValueError(f"{self.label}: invalid generated Swift metadata")
        return match.group(1)

    def apply(self, workspace: Path, version: str) -> None:
        path = workspace / self.relative_path
        updated = (
            "// @generated by hub/scripts/sync-ecosystem-versions.py\n"
            f'public let teslatlasProductVersion = "{version}"\n'
        )
        if path.read_text(encoding="utf-8") != updated:
            path.write_text(updated, encoding="utf-8")


@dataclass(frozen=True)
class YamlField:
    relative_path: str
    path: tuple[str, ...]

    @property
    def label(self) -> str:
        return f"{self.relative_path}:{'.'.join(self.path)}"

    def read(self, workspace: Path) -> str:
        value, _, _ = _yaml_scalar(
            (workspace / self.relative_path).read_text(encoding="utf-8"), self.path
        )
        return value

    def apply(self, workspace: Path, version: str) -> None:
        path = workspace / self.relative_path
        text = path.read_text(encoding="utf-8")
        _, start, end = _yaml_scalar(text, self.path)
        updated = _replace_span(text, start, end, json.dumps(version))
        if updated != text:
            path.write_text(updated, encoding="utf-8")


COMPATIBILITY_COMPONENTS = (
    "teslatlas-protocol",
    "teslatlas-sdk-typescript",
    "teslatlas-sdk-swift",
    "teslatlas-viewer",
    "teslatlas-home-assistant",
    "teslatlas-edge",
)


FIELDS: tuple[VersionField, ...] = (
    TomlSectionField("hub/Cargo.toml", "package"),
    TomlPackageField("hub/Cargo.lock", "teslatlas-hub"),
    YamlField("hub/macos/TeslatlasHubApp/project.yml", ("settings", "base", "MARKETING_VERSION")),
    JsonField("hub/docs/compatibility/ecosystem-release.json", (("product_version",),)),
    TomlSectionField("teslatlas-protocol/pyproject.toml", "project"),
    TomlPackageField("teslatlas-protocol/uv.lock", "teslatlas-protocol-conformance"),
    TextField("teslatlas-protocol/VERSION"),
    JsonField("teslatlas-sdk-typescript/package.json", (("version",),)),
    JsonField(
        "teslatlas-sdk-typescript/package-lock.json",
        (("version",), ("packages", "", "version")),
    ),
    TextField("teslatlas-sdk-swift/VERSION"),
    SwiftGeneratedField("teslatlas-sdk-swift/Sources/TeslatlasHubSDK/ProductVersion.swift"),
    JsonField("teslatlas-viewer/package.json", (("version",),)),
    JsonField(
        "teslatlas-viewer/package-lock.json", (("version",), ("packages", "", "version"))
    ),
    JsonField("teslatlas-viewer/public/version.json", (("product_version",),)),
    TomlSectionField("teslatlas-home-assistant/pyproject.toml", "project"),
    TomlPackageField("teslatlas-home-assistant/uv.lock", "teslatlas-home-assistant"),
    JsonField(
        "teslatlas-home-assistant/custom_components/teslatlas_hub/manifest.json",
        (("version",),),
    ),
    TomlSectionField("teslatlas-edge/Cargo.toml", "package"),
    TomlPackageField("teslatlas-edge/Cargo.lock", "teslatlas-edge"),
    *(
        JsonField(f"{component}/compatibility/hub.json", (("product_version",),))
        for component in COMPATIBILITY_COMPONENTS
    ),
)


G3_VERSION_FIELDS: tuple[VersionField, ...] = (
    TomlSectionField("hub/Cargo.toml", "package"),
    TomlPackageField("hub/Cargo.lock", "teslatlas-hub"),
    TomlSectionField("teslatlas-protocol/pyproject.toml", "project"),
    TomlPackageField("teslatlas-protocol/uv.lock", "teslatlas-protocol-conformance"),
    TextField("teslatlas-protocol/VERSION"),
    JsonField("teslatlas-sdk-typescript/package.json", (("version",),)),
    JsonField(
        "teslatlas-sdk-typescript/package-lock.json",
        (("version",), ("packages", "", "version")),
    ),
    TextField("teslatlas-sdk-swift/VERSION"),
    SwiftGeneratedField("teslatlas-sdk-swift/Sources/TeslatlasHubSDK/ProductVersion.swift"),
    TomlSectionField("teslatlas-edge/Cargo.toml", "package"),
    TomlPackageField("teslatlas-edge/Cargo.lock", "teslatlas-edge"),
)


def _check_plist_guards(workspace: Path) -> list[str]:
    relative = "hub/macos/TeslatlasHubApp/TeslatlasHubApp/Info.plist"
    try:
        with (workspace / relative).open("rb") as source:
            document = plistlib.load(source)
    except (OSError, plistlib.InvalidFileException) as error:
        return [f"{relative}: {error}"]
    expected = {
        "CFBundleShortVersionString": "$(MARKETING_VERSION)",
        "CFBundleVersion": "$(CURRENT_PROJECT_VERSION)",
        "TeslatlasHubVersion": "$(TESLATLAS_HUB_VERSION)",
    }
    return [
        f"{relative}:{key}: expected {value}, found {document.get(key)!r}"
        for key, value in expected.items()
        if document.get(key) != value
    ]


def _check_compatibility_records(workspace: Path, version: str) -> list[str]:
    errors: list[str] = []
    relatives = [f"{component}/compatibility/hub.json" for component in COMPATIBILITY_COMPONENTS]
    typescript_source = "teslatlas-sdk-typescript/protocol/source/compatibility/hub.json"
    if (workspace / typescript_source).is_file():
        relatives.append(typescript_source)
    for relative in relatives:
        try:
            document = json.loads((workspace / relative).read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            errors.append(f"{relative}: {error}")
            continue
        status = document.get("status")
        if status == "candidate":
            requirements = (
                ("tested_hub_versions", "candidate record must have no tested Hub versions"),
                (
                    "tested_hub_source_fingerprints",
                    "candidate record must have no tested Hub source fingerprints",
                ),
                ("test_receipt_paths", "candidate record must have no test receipts"),
            )
            for key, message in requirements:
                if document.get(key) != []:
                    errors.append(f"{relative}:{key}: {message}")
            continue
        if status != "accepted":
            errors.append(f"{relative}:status: expected candidate or accepted")
            continue
        profile = document.get("profile")
        if not isinstance(profile, dict) or not isinstance(profile.get("sha256"), str) or SHA256_RE.fullmatch(profile["sha256"]) is None:
            errors.append(f"{relative}:profile.sha256: accepted record requires an exact digest")
        if document.get("tested_hub_versions") != [version]:
            errors.append(f"{relative}:tested_hub_versions: accepted record must bind exactly {version}")
        fingerprints = document.get("tested_hub_source_fingerprints")
        if not isinstance(fingerprints, list) or not fingerprints or len(fingerprints) != len(set(fingerprints)) or any(
            not isinstance(item, str) or SOURCE_FINGERPRINT_RE.fullmatch(item) is None
            for item in fingerprints
        ):
            errors.append(f"{relative}:tested_hub_source_fingerprints: accepted record requires unique content-bound identities")
        receipts = document.get("test_receipt_paths")
        if not isinstance(receipts, list) or not receipts or len(receipts) != len(set(receipts)):
            errors.append(f"{relative}:test_receipt_paths: accepted record requires unique receipts")
        elif any(
            not isinstance(item, str)
            or Path(item).is_absolute()
            or Path(item).as_posix() != item
            or ".." in Path(item).parts
            or not (workspace / item).is_file()
            for item in receipts
        ):
            errors.append(f"{relative}:test_receipt_paths: accepted receipt path is missing or unsafe")
    return errors


def _sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _accepted_record(
    current: dict[str, Any], *, version: str, profile_sha256: str,
    fingerprints: list[str], receipts: list[str]
) -> dict[str, Any]:
    updated = dict(current)
    updated["status"] = "accepted"
    profile = dict(updated["profile"])
    profile["sha256"] = profile_sha256
    updated["profile"] = profile
    updated["tested_hub_versions"] = [version]
    updated["tested_hub_source_fingerprints"] = fingerprints
    updated["test_receipt_paths"] = receipts
    return updated


def _g3_specifications() -> dict[str, tuple[str, str, str, list[str], list[str]]]:
    protocol_receipts = [*G3_G2_RECEIPTS, *G3_G5_RECEIPTS, *G3_G6_RECEIPTS]
    return {
        "teslatlas-protocol/compatibility/hub.json": (
            "hub-http-v1", "1.0.0", HUB_PROFILE_SHA256,
            [G2_HUB_SOURCE, G5_HUB_SOURCE, G6_HUB_SOURCE], protocol_receipts,
        ),
        "teslatlas-sdk-typescript/protocol/source/compatibility/hub.json": (
            "hub-http-v1", "1.0.0", HUB_PROFILE_SHA256,
            [G2_HUB_SOURCE, G5_HUB_SOURCE, G6_HUB_SOURCE], protocol_receipts,
        ),
        "teslatlas-sdk-typescript/compatibility/hub.json": (
            "hub-http-v1", "1.0.0", HUB_PROFILE_SHA256,
            [G2_HUB_SOURCE], list(G3_G2_RECEIPTS),
        ),
        "teslatlas-edge/compatibility/hub.json": (
            "edge-delivery-v2", "2.0.0", EDGE_PROFILE_SHA256,
            [G5_HUB_SOURCE], list(G3_G5_RECEIPTS),
        ),
        "teslatlas-sdk-swift/compatibility/hub.json": (
            "hub-http-v1", "1.0.0", HUB_PROFILE_SHA256,
            [G6_HUB_SOURCE], list(G3_G6_RECEIPTS),
        ),
    }


def _check_g3_input_identities(workspace: Path) -> list[str]:
    errors: list[str] = []
    for relative, expected in G3_RECEIPTS.items():
        path = workspace / relative
        try:
            actual = _sha256_file(path)
            receipt = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            errors.append(f"G3 receipt identity mismatch: {relative}: {error}")
            continue
        if actual != expected:
            errors.append(f"G3 receipt identity mismatch: {relative}")
        if not isinstance(receipt, dict) or (
            receipt.get("result") != "ACCEPTED"
            and receipt.get("state") not in {"accepted_closed", "ACCEPTED"}
        ):
            errors.append(f"G3 receipt is not accepted: {relative}")

    for relative, expected in G3_PROFILE_BINDINGS.items():
        path = workspace / relative
        try:
            actual = _sha256_file(path)
        except OSError as error:
            errors.append(f"G3 profile identity mismatch: {relative}: {error}")
            continue
        if actual != expected:
            errors.append(f"G3 profile identity mismatch: {relative}")
    return errors


def _check_g3_records(
    workspace: Path, version: str, *, allow_candidates: bool
) -> list[str]:
    errors: list[str] = []
    documents: dict[str, dict[str, Any]] = {}
    for relative, specification in _g3_specifications().items():
        profile_id, profile_revision, profile_sha256, fingerprints, receipts = specification
        path = workspace / relative
        try:
            document = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            errors.append(f"{relative}: {error}")
            continue
        if not isinstance(document, dict):
            errors.append(f"{relative}: expected a JSON object")
            continue
        documents[relative] = document
        if document.get("schema_version") != 1:
            errors.append(f"{relative}:schema_version: expected 1")
        if document.get("product_version") != version:
            errors.append(
                f"{relative}:product_version: expected {version}, "
                f"found {document.get('product_version')!r}"
            )
        profile = document.get("profile")
        if not isinstance(profile, dict):
            errors.append(f"{relative}:profile: expected an object")
            continue
        if profile.get("id") != profile_id or profile.get("revision") != profile_revision:
            errors.append(
                f"{relative}:profile: expected {profile_id}@{profile_revision}"
            )
        expected = _accepted_record(
            document,
            version=version,
            profile_sha256=profile_sha256,
            fingerprints=fingerprints,
            receipts=receipts,
        )
        status = document.get("status")
        if status == "accepted":
            if document != expected:
                errors.append(f"refusing to rewrite a different accepted G3 record: {relative}")
            continue
        if status != "candidate":
            errors.append(f"G3 compatibility status cannot transition: {relative}")
            continue
        if not allow_candidates:
            errors.append(f"G3 compatibility record is not accepted: {relative}")
        if profile.get("sha256") not in {None, profile_sha256}:
            errors.append(f"{relative}:profile.sha256: unexpected candidate digest")
        for key in (
            "tested_hub_versions",
            "tested_hub_source_fingerprints",
            "test_receipt_paths",
        ):
            if document.get(key) != []:
                errors.append(f"{relative}:{key}: candidate G3 field must be empty")

    protocol_relative = "teslatlas-protocol/compatibility/hub.json"
    vendored_relative = "teslatlas-sdk-typescript/protocol/source/compatibility/hub.json"
    if protocol_relative in documents and vendored_relative in documents:
        try:
            if (workspace / protocol_relative).read_bytes() != (workspace / vendored_relative).read_bytes():
                errors.append(
                    f"{vendored_relative}: must be byte-identical to {protocol_relative}"
                )
        except OSError as error:
            errors.append(f"G3 vendored compatibility readback failed: {error}")
    return errors


def check_g3_workspace(
    workspace: Path, *, allow_candidates: bool = False
) -> tuple[str, list[str]]:
    """Validate only the active Hub, Protocol, TypeScript, Edge and Swift G3 boundary."""
    version = _authority_version(workspace)
    errors: list[str] = []
    if version != G3_PRODUCT_VERSION:
        errors.append(f"G3 admission is bound to product version {G3_PRODUCT_VERSION}")
    for field in G3_VERSION_FIELDS:
        try:
            actual = field.read(workspace)
            parse_product_version(actual)
            if actual != version:
                errors.append(f"{field.label}: expected {version}, found {actual}")
        except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
            errors.append(f"{field.label}: {error}")
    errors.extend(_check_g3_input_identities(workspace))
    errors.extend(_check_g3_records(workspace, version, allow_candidates=allow_candidates))
    return version, errors


def admit_g3(workspace: Path) -> str:
    """Admit exact G2/G4, G5 and G6 evidence through the scoped G3 boundary."""
    version, errors = check_g3_workspace(workspace, allow_candidates=True)
    if errors:
        raise ValueError("G3 pre-admission validation failed:\n" + "\n".join(errors))

    updates: dict[Path, str] = {}
    for relative, specification in _g3_specifications().items():
        _, _, profile_sha256, fingerprints, receipts = specification
        path = workspace / relative
        current = json.loads(path.read_text(encoding="utf-8"))
        accepted = _accepted_record(
            current,
            version=version,
            profile_sha256=profile_sha256,
            fingerprints=fingerprints,
            receipts=receipts,
        )
        updates[path] = json.dumps(accepted, indent=2) + "\n"

    for path, updated in updates.items():
        if path.read_text(encoding="utf-8") != updated:
            path.write_text(updated, encoding="utf-8")

    checked_version, errors = check_g3_workspace(workspace)
    if errors:
        raise ValueError("G3 scoped post-admission validation failed:\n" + "\n".join(errors))
    return checked_version


def _ecosystem_release_document(workspace: Path) -> tuple[Path, str, dict[str, Any]]:
    path = workspace / "hub/docs/compatibility/ecosystem-release.json"
    text = path.read_text(encoding="utf-8")
    document = json.loads(text)
    if not isinstance(document, dict):
        raise ValueError("ecosystem release must be a JSON object")
    return path, text, document


def _check_ecosystem_release(workspace: Path, version: str) -> list[str]:
    relative = "hub/docs/compatibility/ecosystem-release.json"
    errors: list[str] = []
    try:
        _, _, document = _ecosystem_release_document(workspace)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        return [f"{relative}: {error}"]

    source_tag = document.get("source_tag")
    if not isinstance(source_tag, dict):
        errors.append(f"{relative}:source_tag: expected an object")
    else:
        status = source_tag.get("status")
        if status not in ("not_created", "created"):
            errors.append(
                f"{relative}:source_tag.status: expected not_created or created, found {status!r}"
            )
        expected_tag = f"v{version}"
        if source_tag.get("name") != expected_tag:
            errors.append(
                f"{relative}:source_tag.name: expected {expected_tag}, found {source_tag.get('name')!r}"
            )

    artifacts = document.get("artifacts")
    if not isinstance(artifacts, list):
        errors.append(f"{relative}:artifacts: expected an array")
        return errors
    for index, artifact in enumerate(artifacts):
        label = f"{relative}:artifacts[{index}]"
        if not isinstance(artifact, dict):
            errors.append(f"{label}: expected an object")
            continue
        embedded = artifact.get("embedded_version")
        if embedded != version:
            errors.append(
                f"{label}.embedded_version: expected {version}, found {embedded!r}"
            )
        if not isinstance(artifact.get("sha256"), str) or re.fullmatch(
            r"[0-9a-f]{64}", artifact["sha256"]
        ) is None:
            errors.append(f"{label}.sha256: expected 64 lowercase hexadecimal characters")
        if artifact.get("component") == "teslatlas-sdk-typescript":
            expected_name = f"teslatlas-sdk-{version}.tgz"
            if artifact.get("name") != expected_name:
                errors.append(
                    f"{label}.name: expected {expected_name}, found {artifact.get('name')!r}"
                )
        if artifact.get("status") != "local_candidate":
            errors.append(f"{label}.status: expected local_candidate")
        if artifact.get("distribution") != "not_published":
            errors.append(f"{label}.distribution: expected not_published")
    return errors


def _prepare_ecosystem_release_transition(workspace: Path, version: str) -> None:
    path, text, document = _ecosystem_release_document(workspace)
    old_version = document.get("product_version")
    if not isinstance(old_version, str):
        raise ValueError("ecosystem release product_version must be a string")
    parse_product_version(old_version)
    source_tag = document.get("source_tag")
    if not isinstance(source_tag, dict):
        raise ValueError("ecosystem release source_tag must be an object")
    tag_status = source_tag.get("status")
    tag_name = source_tag.get("name")
    expected_tag = f"v{version}"
    if tag_status == "created":
        if old_version != version or tag_name != expected_tag:
            raise ValueError(
                f"refusing to move created source tag {tag_name!r} to {expected_tag}"
            )
        return
    if tag_status != "not_created":
        raise ValueError(
            f"ecosystem release source_tag.status must be not_created or created, found {tag_status!r}"
        )
    if old_version != version and document.get("status") != "candidate":
        raise ValueError("refusing to change a noncandidate ecosystem release cohort")

    replacements: dict[tuple[str | int, ...], Any] = {
        ("product_version",): version,
        ("source_tag", "name"): expected_tag,
    }
    if old_version != version:
        replacements[("artifacts",)] = []
        replacements[("test_receipt_paths",)] = []
    updated = _replace_json_values(text, replacements)
    if updated != text:
        path.write_text(updated, encoding="utf-8")


def _authority_version(workspace: Path) -> str:
    authority = FIELDS[0]
    try:
        version = authority.read(workspace)
        parse_product_version(version)
    except (OSError, ValueError) as error:
        raise ValueError(f"{authority.label}: {error}") from error
    return version


def check_workspace(workspace: Path) -> tuple[str, list[str]]:
    version = _authority_version(workspace)
    errors: list[str] = []
    for field in FIELDS:
        try:
            actual = field.read(workspace)
            parse_product_version(actual)
            if actual != version:
                errors.append(f"{field.label}: expected {version}, found {actual}")
        except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
            errors.append(f"{field.label}: {error}")
    errors.extend(_check_plist_guards(workspace))
    errors.extend(_check_ecosystem_release(workspace, version))
    errors.extend(_check_compatibility_records(workspace, version))
    return version, errors


def apply_workspace(workspace: Path) -> str:
    version = _authority_version(workspace)
    try:
        _prepare_ecosystem_release_transition(workspace, version)
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        raise ValueError(f"hub/docs/compatibility/ecosystem-release.json: {error}") from error
    for field in FIELDS:
        try:
            field.apply(workspace, version)
        except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
            raise ValueError(f"{field.label}: {error}") from error
    checked_version, errors = check_workspace(workspace)
    if errors:
        raise ValueError("apply did not produce an aligned workspace:\n" + "\n".join(errors))
    return checked_version


def _parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", required=True, type=Path)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--apply", action="store_true")
    mode.add_argument("--admit-g3", action="store_true")
    mode.add_argument("--check-g3", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(sys.argv[1:] if argv is None else argv)
    if not args.workspace.is_absolute():
        print("sync-ecosystem-versions: --workspace must be an absolute path", file=sys.stderr)
        return 2
    workspace = args.workspace.resolve()
    if not workspace.is_dir():
        print(f"sync-ecosystem-versions: workspace does not exist: {workspace}", file=sys.stderr)
        return 2
    try:
        if args.admit_g3:
            version = admit_g3(workspace)
            print(f"ecosystem G3 compatibility for product version {version} was admitted")
            return 0
        if args.check_g3:
            version, errors = check_g3_workspace(workspace)
            if errors:
                for error in errors:
                    print(f"sync-ecosystem-versions: {error}", file=sys.stderr)
                return 1
            print(f"ecosystem G3 compatibility for product version {version} is aligned")
            return 0
        if args.apply:
            version = apply_workspace(workspace)
            print(f"ecosystem product version {version} was aligned")
            return 0
        version, errors = check_workspace(workspace)
    except ValueError as error:
        print(f"sync-ecosystem-versions: {error}", file=sys.stderr)
        return 1
    if errors:
        for error in errors:
            print(f"sync-ecosystem-versions: {error}", file=sys.stderr)
        return 1
    print(f"ecosystem product version {version} is aligned")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
