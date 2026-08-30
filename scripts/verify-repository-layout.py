#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Verify the governed repository documentation layout and local links."""

from __future__ import annotations

import argparse
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import stat
import subprocess
import sys


DOC_CATEGORIES = {
    "architecture",
    "assets",
    "brand",
    "governance",
    "guides",
    "legal",
    "maintainers",
    "operations",
    "releases",
}
REQUIRED_PATHS = {
    ".github/CODE_OF_CONDUCT.md",
    ".github/CONTRIBUTING.md",
    ".github/README.md",
    ".github/SECURITY.md",
    ".github/SUPPORT.md",
    "docs/index.md",
}
SOURCE_DOMAINS = {
    "api",
    "application",
    "auth",
    "collection",
    "geo",
    "import",
    "platform",
    "runtime",
    "storage",
    "sync",
}
FORBIDDEN_TOOL_ROOTS = {
    ".agents",
    ".claude",
    ".codex",
    ".cursor",
    ".grok",
    ".idea",
    ".vscode",
}
FORBIDDEN_TOOL_FILES = {"AGENTS.md", "CLAUDE.md", "GROK.md"}
MAX_RUST_SOURCE_LINES = 3_000
DOC_NAME_RE = re.compile(r"^[a-z0-9]+(?:[.-][a-z0-9]+)*\.md$")
RUST_NAME_RE = re.compile(r"^[a-z][a-z0-9]*(?:_[a-z0-9]+)*\.rs$")
MARKDOWN_LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
HTML_LINK_RE = re.compile(r'(?:href|src)="([^"]+)"')
REMOTE_PREFIXES = ("#", "http://", "https://", "mailto:")


class LayoutError(RuntimeError):
    pass


def tracked_paths(repo: Path) -> set[str]:
    try:
        result = subprocess.run(
            ["git", "ls-files", "-z"],
            cwd=repo,
            check=True,
            capture_output=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        raise LayoutError("cannot read tracked repository paths") from exc
    try:
        paths = {
            item.decode("utf-8")
            for item in result.stdout.split(b"\0")
            if item
        }
    except UnicodeDecodeError as exc:
        raise LayoutError("tracked paths must be UTF-8") from exc
    if not paths:
        raise LayoutError("repository has no tracked paths")
    return paths


def tracked_directories(paths: set[str]) -> set[str]:
    directories: set[str] = set()
    for path in paths:
        for parent in PurePosixPath(path).parents:
            value = parent.as_posix()
            if value != ".":
                directories.add(value)
    return directories


def regular_file(repo: Path, relative: str) -> Path:
    path = repo / relative
    try:
        metadata = os.lstat(path)
    except OSError as exc:
        raise LayoutError(f"tracked documentation is missing: {relative}") from exc
    if not stat.S_ISREG(metadata.st_mode):
        raise LayoutError(f"tracked documentation must be a regular file: {relative}")
    return path


def local_target(raw: str) -> str | None:
    value = raw.strip().split(maxsplit=1)[0].strip("<>")
    if not value or value.startswith(REMOTE_PREFIXES):
        return None
    return value.split("#", 1)[0] or None


def verify_links(repo: Path, paths: set[str]) -> list[str]:
    directories = tracked_directories(paths)
    errors: list[str] = []
    for relative in sorted(path for path in paths if path.endswith(".md")):
        source = regular_file(repo, relative)
        try:
            text = source.read_text(encoding="utf-8", errors="strict")
        except UnicodeError:
            errors.append(f"Markdown is not UTF-8: {relative}")
            continue
        for pattern in (MARKDOWN_LINK_RE, HTML_LINK_RE):
            for match in pattern.finditer(text):
                target = local_target(match.group(1))
                if target is None:
                    continue
                resolved = posixpath.normpath(
                    posixpath.join(posixpath.dirname(relative), target)
                )
                line = text.count("\n", 0, match.start()) + 1
                if resolved == ".." or resolved.startswith("../"):
                    errors.append(
                        f"local link escapes repository: {relative}:{line}: {target}"
                    )
                elif resolved not in paths and resolved not in directories:
                    errors.append(
                        f"broken or case-mismatched local link: "
                        f"{relative}:{line}: {target}"
                    )
    return errors


def verify(repo: Path) -> None:
    paths = tracked_paths(repo)
    errors: list[str] = []
    root_markdown = sorted(
        path for path in paths if "/" not in path and path.endswith(".md")
    )
    if root_markdown:
        errors.append("tracked root Markdown is forbidden: " + ", ".join(root_markdown))
    missing = sorted(REQUIRED_PATHS - paths)
    if missing:
        errors.append("required repository documents are missing: " + ", ".join(missing))
    forbidden_tools = sorted(
        path
        for path in paths
        if PurePosixPath(path).parts[0] in FORBIDDEN_TOOL_ROOTS
        or path in FORBIDDEN_TOOL_FILES
    )
    if forbidden_tools:
        errors.append("tool-specific repository metadata is forbidden: " + ", ".join(forbidden_tools))
    source_domains = {
        PurePosixPath(path).parts[1]
        for path in paths
        if path.startswith("src/") and len(PurePosixPath(path).parts) >= 3
    }
    missing_domains = sorted(SOURCE_DOMAINS - source_domains)
    unknown_domains = sorted(source_domains - SOURCE_DOMAINS)
    if missing_domains:
        errors.append("required source domains are missing: " + ", ".join(missing_domains))
    if unknown_domains:
        errors.append("unknown source domains: " + ", ".join(unknown_domains))
    flat_rust = sorted(
        path for path in paths if path.startswith("src/") and path.count("/") == 1 and path != "src/lib.rs"
    )
    if flat_rust:
        errors.append("flat source modules are forbidden: " + ", ".join(flat_rust))
    for relative in sorted(path for path in paths if path.startswith("src/") and path.endswith(".rs")):
        parts = PurePosixPath(relative).parts
        if not RUST_NAME_RE.fullmatch(parts[-1]):
            errors.append(f"Rust filename is not lowercase snake_case: {relative}")
        for directory in parts[1:-1]:
            if not re.fullmatch(r"[a-z][a-z0-9]*(?:_[a-z0-9]+)*", directory):
                errors.append(f"Rust source directory is not lowercase snake_case: {relative}")
                break
        source = repo / relative
        try:
            line_count = len(source.read_text(encoding="utf-8", errors="strict").splitlines())
        except (OSError, UnicodeError):
            errors.append(f"Rust source is missing or not UTF-8: {relative}")
            continue
        if line_count > MAX_RUST_SOURCE_LINES:
            errors.append(
                f"Rust source exceeds {MAX_RUST_SOURCE_LINES} lines: {relative} ({line_count})"
            )
    for relative in sorted(
        path for path in paths if path.startswith("docs/") and path.endswith(".md")
    ):
        parts = PurePosixPath(relative).parts
        if len(parts) == 2 and relative != "docs/index.md":
            errors.append(f"uncategorised documentation path: {relative}")
        elif len(parts) >= 3 and parts[1] not in DOC_CATEGORIES:
            errors.append(f"unknown documentation category: {relative}")
        if relative != "docs/index.md" and not DOC_NAME_RE.fullmatch(parts[-1]):
            errors.append(f"documentation filename is not lowercase kebab-case: {relative}")
        try:
            regular_file(repo, relative)
        except LayoutError as exc:
            errors.append(str(exc))
    errors.extend(verify_links(repo, paths))
    if errors:
        raise LayoutError("\n".join(errors))
    markdown_count = sum(path.endswith(".md") for path in paths)
    print(
        f"repository layout passed: {len(paths)} tracked files, "
        f"{markdown_count} Markdown files, zero root Markdown, "
        f"{len(SOURCE_DOMAINS)} source domains, Rust files <= {MAX_RUST_SOURCE_LINES} lines"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    args = parser.parse_args()
    try:
        verify(args.repo.resolve())
    except LayoutError as exc:
        print(f"repository-layout: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc


if __name__ == "__main__":
    main()
