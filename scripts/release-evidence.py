#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Cargo metadata, SBOM and notice generation loaded by scripts/legal-bundle.py."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess


SCHEMA = "teslatlas.release-evidence/v1"
MAX_LEGAL_FILE_BYTES = 16 * 1024 * 1024
MAX_CARGO_LOCK_BYTES = 32 * 1024 * 1024


class GateError(RuntimeError):
    pass


@dataclass(frozen=True)
class ArtifactWitness:
    path: Path
    relative_path: str
    size: int
    digest: str
    device: int
    inode: int
    mtime_ns: int


def run(args: list[str], cwd: Path, *, env: dict[str, str] | None = None,
        capture: bool = True) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            args, cwd=cwd, env=env, text=True, capture_output=capture, check=True
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        detail = ""
        if isinstance(exc, subprocess.CalledProcessError):
            detail = (exc.stderr or exc.stdout or "").strip()
        raise GateError(f"command failed: {' '.join(args)}{': ' + detail if detail else ''}") from exc


def capture_artifact(repo: Path, path: Path) -> ArtifactWitness:
    try:
        before = os.lstat(path)
    except OSError as exc:
        raise GateError(f"artifact is unavailable: {path}") from exc
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
        raise GateError(f"artifact must be a regular, non-symlink file: {path}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise GateError(f"artifact cannot be safely opened: {path}") from exc
    try:
        opened = os.fstat(descriptor)
        if (
            (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)
            or opened.st_nlink != 1
        ):
            raise GateError(f"artifact changed while opening: {path}")
        digest = hashlib.sha256()
        while True:
            block = os.read(descriptor, 1024 * 1024)
            if not block:
                break
            digest.update(block)
        after = os.fstat(descriptor)
        if (
            after.st_size != opened.st_size
            or after.st_nlink != 1
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
        ):
            raise GateError(f"artifact changed while reading: {path}")
    finally:
        os.close(descriptor)
    try:
        current = os.lstat(path)
    except OSError as exc:
        raise GateError(f"artifact changed after reading: {path}") from exc
    if (
        (current.st_dev, current.st_ino) != (opened.st_dev, opened.st_ino)
        or current.st_nlink != 1
        or current.st_size != opened.st_size
        or current.st_mtime_ns != opened.st_mtime_ns
    ):
        raise GateError(f"artifact changed after reading: {path}")
    return ArtifactWitness(
        path,
        relative(repo, path),
        opened.st_size,
        digest.hexdigest(),
        opened.st_dev,
        opened.st_ino,
        opened.st_mtime_ns,
    )


def relative(repo: Path, path: Path) -> str:
    return Path(os.path.relpath(path, repo)).as_posix()


def regular_file(path: Path, label: str) -> None:
    if not path.is_file() or path.is_symlink():
        raise GateError(f"{label} must be a regular, non-symlink file: {path}")


def strict_utf8_document(data: bytes, label: str) -> str:
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise GateError(f"{label} is not UTF-8") from exc
    if "\0" in text or "\r" in text or not text.endswith("\n"):
        raise GateError(f"{label} is not canonical UTF-8 text")
    if "'''" in text or '\"\"\"' in text:
        raise GateError(f"{label} uses unsupported multiline TOML strings")
    return text


def witness_bytes(expected: ArtifactWitness, label: str, maximum: int) -> bytes:
    if expected.size <= 0 or expected.size > maximum:
        raise GateError(f"{label} size is invalid")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(expected.path, flags)
    except OSError as exc:
        raise GateError(f"{label} cannot be safely opened") from exc
    try:
        opened = os.fstat(descriptor)
        if (
            opened.st_dev != expected.device
            or opened.st_ino != expected.inode
            or opened.st_size != expected.size
            or opened.st_mtime_ns != expected.mtime_ns
        ):
            raise GateError(f"{label} changed before reading")
        data = bytearray()
        digest = hashlib.sha256()
        while len(data) <= maximum:
            block = os.read(descriptor, min(1024 * 1024, maximum + 1 - len(data)))
            if not block:
                break
            data.extend(block)
            digest.update(block)
        after = os.fstat(descriptor)
        if (
            len(data) != expected.size
            or len(data) > maximum
            or digest.hexdigest() != expected.digest
            or after.st_size != opened.st_size
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
            or after.st_nlink != 1
        ):
            raise GateError(f"{label} changed while reading")
    finally:
        os.close(descriptor)
    return bytes(data)


def cargo_metadata(repo: Path) -> dict:
    env = os.environ.copy()
    env["CARGO_NET_OFFLINE"] = "true"
    result = run(
        ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1",
         "--manifest-path", str(repo / "Cargo.toml")], repo, env=env,
    )
    try:
        metadata = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise GateError("cargo metadata did not return JSON") from exc

    return metadata


def cargo_lock_checksums(repo: Path) -> dict[tuple[str, str, str], str]:
    lock_path = repo / "Cargo.lock"
    witness = capture_artifact(repo, lock_path)
    data = witness_bytes(witness, "Cargo lockfile", MAX_CARGO_LOCK_BYTES)
    text = strict_utf8_document(data, "Cargo lockfile")
    lock_version: int | None = None
    packages: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    in_package = False

    def finish_package() -> None:
        nonlocal current
        if current is not None:
            packages.append(current)
            current = None

    for raw_line in text.splitlines():
        stripped = raw_line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped == "[[package]]":
            finish_package()
            current = {}
            in_package = True
            continue
        if re.fullmatch(r"\[\[?[^\[\]\r\n]+\]\]?", stripped):
            finish_package()
            in_package = False
            continue
        if not in_package:
            version_match = re.fullmatch(r"version\s*=\s*([0-9]+)", stripped)
            if version_match is not None:
                if lock_version is not None:
                    raise GateError("Cargo lockfile has duplicate format versions")
                lock_version = int(version_match.group(1))
            elif re.match(r"version(?:\s|=)", stripped):
                raise GateError("Cargo lockfile format version is invalid")
            continue
        assert current is not None
        field_match = re.fullmatch(
            r'(name|version|source|checksum)\s*=\s*"([^"\\\x00-\x1f]+)"',
            stripped,
        )
        if field_match is not None:
            field, value = field_match.groups()
            if field in current:
                raise GateError(f"Cargo lockfile package has duplicate field: {field}")
            current[field] = value
        elif re.match(r"(?:name|version|source|checksum)(?:\s|=)", stripped):
            raise GateError("Cargo lockfile package identity field is invalid")
    finish_package()
    if lock_version not in {3, 4}:
        raise GateError("Cargo lockfile format version is unsupported")
    if not packages:
        raise GateError("Cargo lockfile contains no packages")
    checksums: dict[tuple[str, str, str], str] = {}
    for package in packages:
        name = package.get("name")
        version = package.get("version")
        source = package.get("source")
        checksum = package.get("checksum")
        if not all(isinstance(value, str) and value for value in (name, version)):
            raise GateError("Cargo lockfile package identity is invalid")
        if source is None:
            continue
        if not isinstance(source, str):
            raise GateError("Cargo lockfile package source is invalid")
        if not source.startswith("registry+"):
            continue
        if not isinstance(checksum, str) or not re.fullmatch(r"[0-9a-f]{64}", checksum):
            raise GateError(f"registry dependency has no valid lockfile checksum: {name}")
        key = (name, version, source)
        if key in checksums and checksums[key] != checksum:
            raise GateError(f"Cargo lockfile has conflicting checksums: {name}")
        checksums[key] = checksum
    return checksums


def normalize_spdx_expression(expression: str, package_name: str) -> str:
    if expression == "NOASSERTION":
        return expression
    # Cargo historically used this exact dual-license spelling. Do not guess
    # the meaning of any other legacy separator.
    stripped = expression.strip()
    legacy_normalizations = {
        "MIT/Apache-2.0": "MIT OR Apache-2.0",
        "Apache-2.0 / MIT": "Apache-2.0 OR MIT",
        "Unlicense/MIT": "Unlicense OR MIT",
    }
    candidate = legacy_normalizations.get(stripped, stripped)
    if "/" in candidate or "," in candidate or "+" in candidate:
        raise GateError(f"dependency has ambiguous legacy license expression: {package_name}")
    tokens = re.findall(r"\(|\)|AND|OR|WITH|[A-Za-z0-9][A-Za-z0-9.-]*", candidate)
    if not tokens or "".join(tokens) != re.sub(r"\s+", "", candidate):
        raise GateError(f"dependency has invalid SPDX license expression: {package_name}")
    index = 0

    def parse_primary() -> None:
        nonlocal index
        if index >= len(tokens):
            raise ValueError
        if tokens[index] == "(":
            index += 1
            parse_or()
            if index >= len(tokens) or tokens[index] != ")":
                raise ValueError
            index += 1
            return
        if tokens[index] in {"AND", "OR", "WITH", ")"}:
            raise ValueError
        index += 1

    def parse_with() -> None:
        nonlocal index
        parse_primary()
        if index < len(tokens) and tokens[index] == "WITH":
            index += 1
            if index >= len(tokens) or tokens[index] in {"(", ")", "AND", "OR", "WITH"}:
                raise ValueError
            index += 1

    def parse_and() -> None:
        nonlocal index
        parse_with()
        while index < len(tokens) and tokens[index] == "AND":
            index += 1
            parse_with()

    def parse_or() -> None:
        nonlocal index
        parse_and()
        while index < len(tokens) and tokens[index] == "OR":
            index += 1
            parse_and()

    try:
        parse_or()
    except ValueError as exc:
        raise GateError(f"dependency has invalid SPDX license expression: {package_name}") from exc
    if index != len(tokens):
        raise GateError(f"dependency has invalid SPDX license expression: {package_name}")
    return " ".join(tokens).replace("( ", "(").replace(" )", ")")


def package_license_material(
    package: dict, repo: Path, license_expression: str
) -> tuple[str, str, list[str], list[str], str]:
    local_paths = package_local_legal_paths(package, repo)
    if local_paths:
        names: list[str] = []
        texts: list[str] = []
        sources: list[str] = []
        notice_sources: list[str] = []
        copyright_texts: list[str] = []
        for path in local_paths:
            try:
                text = dependency_legal_text(path, package, repo)
            except (OSError, UnicodeError) as exc:
                raise GateError(
                    f"dependency legal text is unreadable: {package.get('name')}"
                ) from exc
            names.append(path.name)
            texts.append(f"===== {path.name} =====\n{text}")
            sources.append(path.name)
            lowered = path.name.lower()
            if lowered.startswith(("notice", "copyright")):
                notice_sources.append(path.name)
            if lowered.startswith("copyright"):
                copyright_texts.append(text.rstrip())
        copyright_evidence = "\n\n".join(copyright_texts) or "NOASSERTION"
        return (
            " + ".join(names),
            "\n".join(texts),
            sources,
            notice_sources,
            copyright_evidence,
        )

    corpus = repo / "LICENSES"
    if not corpus.is_dir() or corpus.is_symlink():
        raise GateError(
            f"dependency license text is unavailable and SPDX corpus is missing: "
            f"{package.get('name')}"
        )
    identifiers = spdx_identifiers(license_expression)
    if not identifiers:
        raise GateError(f"dependency license text is unavailable: {package.get('name')}")
    names: list[str] = []
    texts: list[str] = []
    sources: list[str] = []
    for identifier in identifiers:
        path = corpus / f"{identifier}.txt"
        regular_file(path, f"SPDX corpus license {identifier}")
        try:
            text = path.read_text(encoding="utf-8", errors="strict")
        except (OSError, UnicodeError) as exc:
            raise GateError(f"SPDX corpus license is unreadable: {identifier}") from exc
        names.append(path.name)
        texts.append(text)
        sources.append(f"LICENSES/{path.name}")
    return " + ".join(names), "".join(texts), sources, [], "NOASSERTION"


def package_sort(package: dict) -> tuple[str, str, str]:
    return (package.get("name", ""), package.get("version", ""), package.get("source", ""))


def spdx_identifiers(expression: str) -> list[str]:
    identifiers: list[str] = []
    for token in re.findall(r"\(|\)|AND|OR|WITH|[A-Za-z0-9][A-Za-z0-9.-]*", expression):
        if token not in {"(", ")", "AND", "OR", "WITH", "NOASSERTION"} \
                and token not in identifiers:
            identifiers.append(token)
    return identifiers


def package_local_legal_paths(package: dict, repo: Path) -> list[Path]:
    manifest = package.get("manifest_path")
    if not manifest:
        return []
    manifest_path = Path(manifest)
    if not manifest_path.is_absolute():
        manifest_path = repo / manifest_path
    try:
        manifest_metadata = os.lstat(manifest_path)
        root = manifest_path.parent
        root_metadata = os.lstat(root)
        root_resolved = root.resolve(strict=True)
        manifest_resolved = manifest_path.resolve(strict=True)
    except OSError as exc:
        raise GateError(
            f"dependency manifest path is unsafe: {package.get('name')}"
        ) from exc
    if (
        not stat.S_ISREG(manifest_metadata.st_mode)
        or manifest_metadata.st_nlink != 1
        or not stat.S_ISDIR(root_metadata.st_mode)
        or manifest_resolved.parent != root_resolved
    ):
        raise GateError(f"dependency manifest path is unsafe: {package.get('name')}")
    is_project = root_resolved == repo.resolve()
    if is_project:
        exact = repo / "LICENSE"
        regular_file(exact, "project license")
        if os.lstat(exact).st_nlink != 1:
            raise GateError("project license must not be hard-linked")
        return [exact]
    declared = package.get("license_file")
    candidates: list[Path] = []
    if declared:
        if not isinstance(declared, str):
            raise GateError(f"dependency license_file is invalid: {package.get('name')}")
        declared_path = Path(declared)
        if (
            declared_path.is_absolute()
            or not declared_path.parts
            or any(part in {"", ".", ".."} for part in declared_path.parts)
        ):
            raise GateError(f"dependency license_file escapes package root: {package.get('name')}")
        candidates.append(root / declared_path)
    try:
        candidates.extend(sorted(
            path for path in root.iterdir()
            if re.match(
                r"(?i)^(license|licence|copying|notice|copyright)([-_.].*)?$",
                path.name,
            )
        ))
    except OSError as exc:
        raise GateError(f"dependency package root is unreadable: {package.get('name')}") from exc
    selected: list[Path] = []
    for candidate in candidates:
        try:
            metadata = os.lstat(candidate)
            resolved = candidate.resolve(strict=True)
        except OSError as exc:
            raise GateError(f"dependency legal path is unsafe: {package.get('name')}") from exc
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or resolved.parent != root_resolved
        ):
            raise GateError(f"dependency legal path is unsafe: {package.get('name')}")
        if resolved not in selected:
            selected.append(resolved)
    return selected


def dependency_legal_text(path: Path, package: dict, repo: Path) -> str:
    """Read a package legal file without following links or accepting replacement."""
    label = str(package.get("name", "unknown"))
    try:
        before = os.lstat(path)
    except OSError as exc:
        raise GateError(f"dependency legal text is unreadable: {label}") from exc
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_nlink != 1
        or before.st_size <= 0
        or before.st_size > MAX_LEGAL_FILE_BYTES
    ):
        raise GateError(f"dependency legal text is unsafe or oversized: {label}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise GateError(f"dependency legal text is unreadable: {label}") from exc
    try:
        opened = os.fstat(descriptor)
        if (
            (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino)
            or opened.st_nlink != 1
        ):
            raise GateError(f"dependency legal text changed while opening: {label}")
        data = bytearray()
        while len(data) <= MAX_LEGAL_FILE_BYTES:
            block = os.read(
                descriptor,
                min(1024 * 1024, MAX_LEGAL_FILE_BYTES + 1 - len(data)),
            )
            if not block:
                break
            data.extend(block)
        after = os.fstat(descriptor)
        if (
            len(data) != opened.st_size
            or after.st_size != opened.st_size
            or after.st_mtime_ns != opened.st_mtime_ns
            or after.st_ctime_ns != opened.st_ctime_ns
            or after.st_nlink != 1
        ):
            raise GateError(f"dependency legal text changed while reading: {label}")
    finally:
        os.close(descriptor)
    try:
        current = os.lstat(path)
    except OSError as exc:
        raise GateError(f"dependency legal text changed after reading: {label}") from exc
    if (
        (current.st_dev, current.st_ino) != (opened.st_dev, opened.st_ino)
        or current.st_size != opened.st_size
        or current.st_mtime_ns != opened.st_mtime_ns
        or current.st_nlink != 1
    ):
        raise GateError(f"dependency legal text changed after reading: {label}")
    try:
        return bytes(data).decode("utf-8", errors="strict")
    except UnicodeError as exc:
        raise GateError(f"dependency legal text is not UTF-8: {label}") from exc


def sbom_and_notices(metadata: dict, repo: Path) -> tuple[dict, dict, str]:
    project_notices = repo / "docs/legal/third-party-notices.md"
    regular_file(project_notices, "project notices")
    packages = sorted(metadata.get("packages", []), key=package_sort)
    if not packages:
        raise GateError("cargo metadata contains no packages")
    ids = {package["id"]: f"SPDXRef-Package-{index:04d}" for index, package in enumerate(packages, 1)}
    spdx_packages = []
    inventory = []
    license_texts: dict[str, tuple[str, str]] = {}
    lock_checksums = cargo_lock_checksums(repo)
    for package in packages:
        original_license_expression = package.get("license") or "NOASSERTION"
        if not isinstance(original_license_expression, str):
            raise GateError(f"dependency has invalid license metadata: {package.get('name')}")
        license_expression = normalize_spdx_expression(
            original_license_expression, str(package.get("name", "unknown"))
        )
        if license_expression == "NOASSERTION" and not package.get("license_file"):
            raise GateError(f"dependency has no declared license: {package.get('name')}")
        source = package.get("source") or "NOASSERTION"
        checksum = package.get("checksum")
        source_key = package.get("source")
        if source_key and not checksum and isinstance(source_key, str):
            checksum = lock_checksums.get(
                (str(package.get("name", "")), str(package.get("version", "")), source_key)
            )
        if checksum is not None and (
            not isinstance(checksum, str) or not re.fullmatch(r"[0-9a-f]{64}", checksum)
        ):
            raise GateError(f"dependency has an invalid Cargo checksum: {package.get('name')}")
        if package.get("source") and not checksum and not package["source"].startswith("git+"):
            raise GateError(f"dependency has no Cargo checksum: {package.get('name')}")
        (
            license_name,
            text,
            license_sources,
            package_notice_sources,
            copyright_evidence,
        ) = package_license_material(
            package, repo, license_expression
        )
        text_hash = hashlib.sha256(text.encode()).hexdigest()
        license_texts.setdefault(text_hash, (license_name, text))
        package_id = ids[package["id"]]
        spdx_package = {
            "SPDXID": package_id,
            "name": package["name"],
            "versionInfo": package["version"],
            "downloadLocation": source,
            "licenseConcluded": license_expression,
            "licenseDeclared": license_expression,
            "copyrightText": copyright_evidence,
            "filesAnalyzed": False,
            "annotations": [{"annotationType": "OTHER", "annotator": "Tool: teslatlas-release-evidence",
                              "annotationDate": "1970-01-01T00:00:00Z",
                              "comment": f"license-text-sha256={text_hash}"}],
        }
        if checksum:
            spdx_package["checksums"] = [{"algorithm": "SHA256", "checksumValue": checksum}]
        if package.get("repository"):
            spdx_package["externalRefs"] = [{
                "referenceCategory": "PACKAGE-MANAGER", "referenceType": "purl",
                "referenceLocator": f"pkg:cargo/{package['name']}@{package['version']}",
            }]
        spdx_packages.append(spdx_package)
        inventory_record = {
            "name": package["name"], "version": package["version"], "source": source,
            "checksum": checksum, "license": license_expression, "license_text_sha256": text_hash,
            "license_text_sources": license_sources,
            "package_notice": (
                "captured" if package_notice_sources else "absent-in-crate-archive"
            ),
            "package_notice_sources": package_notice_sources,
            "authors": package.get("authors") or [],
            "repository": package.get("repository"),
            "package_id": package_id,
        }
        if original_license_expression != license_expression:
            inventory_record["license_original"] = original_license_expression
        inventory.append(inventory_record)

    relationships = []
    resolve_root = (metadata.get("resolve") or {}).get("root")
    root_package_id = resolve_root if resolve_root in ids else None
    workspace_members = metadata.get("workspace_members") or []
    if root_package_id is None:
        root_package_id = next((item for item in workspace_members if item in ids), None)
    if root_package_id is None:
        root_package_id = next((package["id"] for package in packages if package.get("name") == "teslatlas-hub"), None)
    if root_package_id is None:
        raise GateError("cargo metadata has no workspace root package")
    root_id = ids[root_package_id]
    relationships.append({"spdxElementId": "SPDXRef-DOCUMENT", "relationshipType": "DESCRIBES",
                          "relatedSpdxElement": root_id})
    resolve = metadata.get("resolve") or {}
    for node in resolve.get("nodes", []):
        source_id = ids.get(node.get("id"))
        if not source_id:
            continue
        for dependency in node.get("deps", []):
            target_id = ids.get(dependency.get("pkg"))
            if target_id:
                relationships.append({"spdxElementId": source_id, "relationshipType": "DEPENDS_ON",
                                      "relatedSpdxElement": target_id})
    relationships.sort(key=lambda item: (item["spdxElementId"], item["relatedSpdxElement"]))
    spdx = {
        "spdxVersion": "SPDX-2.3", "dataLicense": "CC0-1.0", "SPDXID": "SPDXRef-DOCUMENT",
        "name": "Teslatlas Hub dependency SBOM", "documentNamespace": "urn:teslatlas:sbom",
        "creationInfo": {"created": "1970-01-01T00:00:00Z", "creators": ["Tool: teslatlas-release-evidence"]},
        "packages": spdx_packages, "relationships": relationships,
    }
    notice_lines = [
        "# Generated dependency notices", "",
        "Generated from offline `cargo metadata --locked --all-features`.",
        "This file contains the exact dependency inventory and captured license texts.", "",
        "## Project notices", "", project_notices.read_text(encoding="utf-8"), "",
        "## Dependency inventory", "",
    ]
    for item in inventory:
        notice_lines.extend([f"### {item['name']} {item['version']}", "",
                             f"- Source: `{item['source']}`",
                             f"- License: `{item['license']}`",
                             f"- License text source(s): `{', '.join(item['license_text_sources'])}`",
                             f"- Package notice: `{item['package_notice']}`",
                             f"- Package notice source(s): `{', '.join(item['package_notice_sources']) or 'none'}`",
                             f"- License text SHA-256: `{item['license_text_sha256']}`", ""])
    notice_lines.extend(["## License texts", ""])
    for text_hash, (name, text) in sorted(license_texts.items()):
        notice_lines.extend([f"### {name} ({text_hash})", "", "```text", text.rstrip(), "```", ""])
    return spdx, {"schema": SCHEMA, "packages": inventory}, "\n".join(notice_lines)
