# SPDX-License-Identifier: AGPL-3.0-only
"""Strict catalog, source, and transactional-prefix primitives."""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
import re
import shutil
import stat
import tempfile
import uuid
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Optional

from .processes import ProcessFailure, run_process
from .recipes import (
    EXPECTED_PROFILES,
    KNOWN_REPOSITORIES,
    RECIPE_REVISION,
    SDK_TARBALL_SHA256,
    VIEWER_ASSET_MANIFEST_SHA256,
    VIEWER_PACKAGE_SHA256,
)

HEX_40 = re.compile(r"[0-9a-f]{40}\Z")
HEX_64 = re.compile(r"[0-9a-f]{64}\Z")
PRODUCT_VERSION = re.compile(r"(\d{4})\.(\d{1,2})\.(\d+)\Z")
EXCLUDED_NAMES = {
    ".DS_Store",
    ".build",
    ".git",
    ".pytest_cache",
    ".ruff_cache",
    ".venv",
    "AGENTS.md",
    "__pycache__",
    "coverage",
    "dist",
    "node_modules",
    "target",
}
MAX_SOURCE_FILES = 50_000
MAX_SOURCE_BYTES = 1_073_741_824


class BootstrapError(RuntimeError):
    """A fail-closed bootstrap error suitable for one-line JSON reporting."""


class CatalogError(BootstrapError):
    """The moving catalog failed structural or admission validation."""


def _prefix_path(prefix: Path) -> Path:
    """Normalize dot segments and reject every unsafe existing path component."""
    expanded = prefix.expanduser()
    normalized = Path(os.path.abspath(expanded))
    current = Path(normalized.anchor)
    parts = normalized.parts[1:]
    for position, part in enumerate(parts):
        current /= part
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            break
        if stat.S_ISLNK(metadata.st_mode):
            if position == 0 and metadata.st_uid == 0:
                resolved = current.resolve(strict=True)
                if resolved.is_absolute() and resolved.is_dir():
                    current = resolved
                    continue
            if position == len(parts) - 1:
                raise BootstrapError("installation prefix must not be a symlink")
            raise BootstrapError("installation prefix has a symlink ancestor")
        if not stat.S_ISDIR(metadata.st_mode):
            raise BootstrapError("installation prefix ancestor is not a directory")
    return current.joinpath(*parts[position + 1 :]) if parts else current


def open_directory_nofollow(path: Path, *, create: bool) -> int:
    """Open an absolute directory tree without following any symlink component."""
    normalized = _prefix_path(path)
    flags = os.O_RDONLY
    if hasattr(os, "O_DIRECTORY"):
        flags |= os.O_DIRECTORY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(normalized.anchor, flags)
    try:
        for part in normalized.parts[1:]:
            try:
                child = os.open(part, flags, dir_fd=descriptor)
            except FileNotFoundError:
                if not create:
                    raise
                try:
                    os.mkdir(part, 0o700, dir_fd=descriptor)
                except FileExistsError:
                    pass
                child = os.open(part, flags, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        if not stat.S_ISDIR(os.fstat(descriptor).st_mode):
            raise BootstrapError("installation prefix ancestor is not a directory")
        return descriptor
    except OSError as error:
        os.close(descriptor)
        raise BootstrapError("installation prefix could not be opened safely") from error


@dataclass(frozen=True)
class Profile:
    id: str
    revision: str
    sha256: str


@dataclass(frozen=True)
class Component:
    name: str
    repository: str
    commit: str
    source_sha256: str
    product_version: str
    profile: Profile
    artifacts: Mapping[str, str]


@dataclass(frozen=True)
class Cohort:
    product_version: str
    publication_status: str
    admitted_hub_versions: tuple[str, ...]
    components: Mapping[str, Component]


@dataclass(frozen=True)
class Catalog:
    cohorts: tuple[Cohort, ...]


def _exact_keys(value: Mapping[str, Any], allowed: set[str], label: str) -> None:
    unknown = set(value) - allowed
    missing = allowed - set(value)
    if unknown or missing:
        raise CatalogError(
            f"{label} fields mismatch: missing={sorted(missing)}, "
            f"unknown={sorted(unknown)}"
        )


def _version_key(value: str) -> tuple[int, int, int]:
    match = PRODUCT_VERSION.fullmatch(value) if isinstance(value, str) else None
    if match is None:
        raise CatalogError("product_version must use YEAR.WEEK.REVISION")
    year, week, revision = (int(part) for part in match.groups())
    if not 1 <= week <= 53 or revision < 1:
        raise CatalogError("product_version has an invalid week or revision")
    return year, week, revision


def parse_catalog(value: Any) -> Catalog:
    """Parse a catalog without accepting commands or unknown fields."""
    if not isinstance(value, dict):
        raise CatalogError("catalog must be an object")
    _exact_keys(value, {"schema_version", "cohorts"}, "catalog")
    if value["schema_version"] != 1 or not isinstance(value["cohorts"], list):
        raise CatalogError("unsupported catalog schema")
    cohorts: list[Cohort] = []
    seen_versions: set[str] = set()
    for raw_cohort in value["cohorts"]:
        if not isinstance(raw_cohort, dict):
            raise CatalogError("cohort must be an object")
        _exact_keys(
            raw_cohort,
            {
                "product_version",
                "publication_status",
                "admitted_hub_versions",
                "components",
            },
            "cohort",
        )
        version = raw_cohort["product_version"]
        _version_key(version)
        if version in seen_versions:
            raise CatalogError("duplicate cohort product_version")
        seen_versions.add(version)
        publication = raw_cohort["publication_status"]
        if publication not in {"published", "local-unpublished"}:
            raise CatalogError("invalid publication_status")
        admissions = raw_cohort["admitted_hub_versions"]
        if (
            not isinstance(admissions, list)
            or len(admissions) != len(set(admissions))
            or any(not isinstance(item, str) for item in admissions)
        ):
            raise CatalogError("admitted_hub_versions must be unique strings")
        for admitted in admissions:
            _version_key(admitted)
        raw_components = raw_cohort["components"]
        if not isinstance(raw_components, dict) or not raw_components:
            raise CatalogError("cohort components must be a nonempty object")
        components: dict[str, Component] = {}
        for name, raw_component in raw_components.items():
            if name not in KNOWN_REPOSITORIES:
                raise CatalogError(f"unknown component {name!r}")
            if not isinstance(raw_component, dict):
                raise CatalogError("component must be an object")
            required_component_fields = {
                "repository",
                "commit",
                "source_sha256",
                "product_version",
                "profile",
            }
            unknown = set(raw_component) - (required_component_fields | {"artifacts"})
            missing = required_component_fields - set(raw_component)
            if unknown or missing:
                raise CatalogError(
                    f"component {name} fields mismatch: missing={sorted(missing)}, "
                    f"unknown={sorted(unknown)}"
                )
            if raw_component["repository"] != KNOWN_REPOSITORIES[name]:
                raise CatalogError(f"component {name} repository is not allowlisted")
            if (
                not isinstance(raw_component["commit"], str)
                or HEX_40.fullmatch(raw_component["commit"]) is None
            ):
                raise CatalogError(
                    f"component {name} commit must be a full immutable hash"
                )
            if (
                not isinstance(raw_component["source_sha256"], str)
                or HEX_64.fullmatch(raw_component["source_sha256"]) is None
            ):
                raise CatalogError(f"component {name} source_sha256 is invalid")
            if raw_component["product_version"] != version:
                raise CatalogError(f"component {name} product_version mixes cohorts")
            raw_profile = raw_component["profile"]
            if not isinstance(raw_profile, dict):
                raise CatalogError(f"component {name} profile must be an object")
            _exact_keys(raw_profile, {"id", "revision", "sha256"}, "profile")
            expected_profile = EXPECTED_PROFILES[name]
            if raw_profile != expected_profile:
                raise CatalogError(
                    f"component {name} profile does not match its recipe"
                )
            artifacts = raw_component.get("artifacts", {})
            if not isinstance(artifacts, dict):
                raise CatalogError(f"component {name} artifacts must be an object")
            expected_artifact_fields: set[str]
            if name == "sdk-typescript":
                expected_artifact_fields = {"package_filename", "package_sha256"}
            elif name == "home-assistant":
                expected_artifact_fields = {
                    "payload_manifest_sha256",
                    "selection_receipt_sha256",
                }
            elif name == "viewer":
                expected_artifact_fields = {
                    "package_filename",
                    "package_sha256",
                    "asset_manifest_sha256",
                    "sdk_package_filename",
                    "sdk_package_sha256",
                }
            else:
                expected_artifact_fields = set()
            if set(artifacts) != expected_artifact_fields:
                if name == "home-assistant":
                    raise CatalogError(
                        "component home-assistant target provenance fields mismatch"
                    )
                raise CatalogError(f"component {name} artifact fields mismatch")
            for key, artifact_value in artifacts.items():
                if key.endswith("sha256"):
                    if (
                        not isinstance(artifact_value, str)
                        or HEX_64.fullmatch(artifact_value) is None
                    ):
                        raise CatalogError(f"component {name} artifact hash is invalid")
                elif (
                    not isinstance(artifact_value, str)
                    or Path(artifact_value).name != artifact_value
                    or artifact_value in {"", ".", ".."}
                ):
                    raise CatalogError(f"component {name} artifact name is invalid")
            expected_package_name = {
                "sdk-typescript": f"teslatlas-sdk-{version}.tgz",
                "viewer": f"teslatlas-viewer-{version}.tgz",
            }.get(name)
            if (
                expected_package_name is not None
                and artifacts.get("package_filename") != expected_package_name
            ):
                raise CatalogError(
                    f"component {name} artifact filename does not match its version"
                )
            if name == "viewer" and artifacts["sdk_package_filename"] != (
                f"teslatlas-sdk-{version}.tgz"
            ):
                raise CatalogError("viewer SDK artifact filename mixes cohorts")
            current_artifacts = {
                "sdk-typescript": {
                    "package_filename": "teslatlas-sdk-2026.36.2.tgz",
                    "package_sha256": SDK_TARBALL_SHA256,
                },
                "viewer": {
                    "package_filename": "teslatlas-viewer-2026.36.2.tgz",
                    "package_sha256": VIEWER_PACKAGE_SHA256,
                    "asset_manifest_sha256": VIEWER_ASSET_MANIFEST_SHA256,
                    "sdk_package_filename": "teslatlas-sdk-2026.36.2.tgz",
                    "sdk_package_sha256": SDK_TARBALL_SHA256,
                },
            }
            if (
                version == "2026.36.2"
                and name in current_artifacts
                and artifacts != current_artifacts[name]
            ):
                raise CatalogError(
                    f"component {name} does not match current reviewed artifacts"
                )
            components[name] = Component(
                name=name,
                repository=raw_component["repository"],
                commit=raw_component["commit"],
                source_sha256=raw_component["source_sha256"],
                product_version=version,
                profile=Profile(**raw_profile),
                artifacts=dict(sorted(artifacts.items())),
            )
        if "viewer" in components:
            sdk = components.get("sdk-typescript")
            viewer = components["viewer"]
            if sdk is None:
                raise CatalogError("viewer requires sdk-typescript in the same cohort")
            if (
                viewer.artifacts["sdk_package_filename"]
                != sdk.artifacts["package_filename"]
                or viewer.artifacts["sdk_package_sha256"]
                != sdk.artifacts["package_sha256"]
            ):
                raise CatalogError("viewer SDK artifact does not match its cohort SDK")
        cohorts.append(Cohort(version, publication, tuple(admissions), components))
    return Catalog(tuple(cohorts))


def load_catalog(path: Path) -> Catalog:
    try:
        return parse_catalog(json.loads(path.read_text(encoding="utf-8")))
    except (OSError, json.JSONDecodeError) as error:
        raise CatalogError("catalog could not be read") from error


def select_cohort(
    catalog: Catalog,
    components: Iterable[str],
    hub_version: str,
    *,
    allow_candidates: bool,
    update: bool,
) -> Cohort:
    """Select one complete cohort without inferring compatibility from CalVer."""
    _version_key(hub_version)
    selected_names = tuple(sorted(set(components)))
    if not selected_names:
        raise CatalogError("at least one component is required")
    unknown = set(selected_names) - set(KNOWN_REPOSITORIES)
    if unknown:
        raise CatalogError(f"unknown components: {sorted(unknown)}")
    eligible: list[Cohort] = []
    same_unpublished = False
    for cohort in catalog.cohorts:
        if any(name not in cohort.components for name in selected_names):
            continue
        same = cohort.product_version == hub_version
        explicitly_admitted = hub_version in cohort.admitted_hub_versions
        if not same and not (update and explicitly_admitted):
            continue
        if cohort.publication_status != "published" and not allow_candidates:
            if same:
                same_unpublished = True
            continue
        eligible.append(cohort)
    if not eligible:
        if same_unpublished:
            raise CatalogError("selected cohort is unpublished")
        raise CatalogError("no complete compatible cohort is available")
    selected = max(eligible, key=lambda cohort: _version_key(cohort.product_version))
    return Cohort(
        selected.product_version,
        selected.publication_status,
        selected.admitted_hub_versions,
        {name: selected.components[name] for name in selected_names},
    )


def _iter_source_files(root: Path) -> list[Path]:
    if root.is_symlink() or not root.is_dir():
        raise BootstrapError("source root is missing or unsafe")
    files: list[Path] = []
    total = 0
    for directory, names, filenames in os.walk(root, followlinks=False):
        base = Path(directory)
        kept_directories = []
        for name in sorted(names):
            if name in EXCLUDED_NAMES:
                continue
            path = base / name
            if not stat.S_ISDIR(path.lstat().st_mode):
                raise BootstrapError(f"source contains unsupported file type: {path}")
            kept_directories.append(name)
        names[:] = kept_directories
        for filename in sorted(filenames):
            if filename in EXCLUDED_NAMES:
                continue
            path = base / filename
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode):
                raise BootstrapError(f"source contains unsupported file type: {path}")
            total += metadata.st_size
            files.append(path)
            if len(files) > MAX_SOURCE_FILES or total > MAX_SOURCE_BYTES:
                raise BootstrapError("source exceeds bootstrap bounds")
    return files


def _source_digest(files: list[dict[str, Any]]) -> str:
    encoded = json.dumps(files, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def source_manifest_for(
    component: str, source: Path, repository: str, commit: str
) -> dict[str, Any]:
    """Create the complete manifest used to bind a local candidate copy."""
    source = source.resolve(strict=True)
    if (
        component not in KNOWN_REPOSITORIES
        or repository != KNOWN_REPOSITORIES[component]
    ):
        raise BootstrapError("local source repository is not allowlisted")
    if HEX_40.fullmatch(commit) is None:
        raise BootstrapError("local source commit must be a full immutable hash")
    files = []
    for path in _iter_source_files(source):
        relative = path.relative_to(source).as_posix()
        raw = path.read_bytes()
        files.append(
            {
                "path": relative,
                "sha256": hashlib.sha256(raw).hexdigest(),
                "size": len(raw),
                "executable": bool(path.stat().st_mode & 0o111),
            }
        )
    return {
        "component": component,
        "path": str(source),
        "repository": repository,
        "commit": commit,
        "source_sha256": _source_digest(files),
        "files": files,
    }


def verify_local_source(record: Mapping[str, Any], destination: Path) -> dict[str, Any]:
    """Verify every listed byte and copy only that immutable local snapshot."""
    required = {"component", "path", "repository", "commit", "source_sha256", "files"}
    if not isinstance(record, dict) or set(record) != required:
        raise BootstrapError("local source manifest fields mismatch")
    component = record["component"]
    if (
        component not in KNOWN_REPOSITORIES
        or record["repository"] != KNOWN_REPOSITORIES[component]
    ):
        raise BootstrapError("local source repository is not allowlisted")
    if (
        not isinstance(record["commit"], str)
        or HEX_40.fullmatch(record["commit"]) is None
    ):
        raise BootstrapError("local source commit is not immutable")
    source = Path(record["path"])
    if not source.is_absolute() or not source.is_dir():
        raise BootstrapError("local source path must be an existing absolute directory")
    observed = source_manifest_for(
        component, source, record["repository"], record["commit"]
    )
    if (
        observed["files"] != record["files"]
        or observed["source_sha256"] != record["source_sha256"]
    ):
        raise BootstrapError(
            f"local source {component} changed after manifest creation"
        )
    if destination.exists():
        raise BootstrapError("source destination already exists")
    destination.mkdir(parents=True, mode=0o700)
    for entry in record["files"]:
        relative = Path(entry["path"])
        if relative.is_absolute() or ".." in relative.parts:
            raise BootstrapError("local source manifest contains an unsafe path")
        source_path = source / relative
        if not stat.S_ISREG(source_path.lstat().st_mode):
            raise BootstrapError(f"local source {component} changed while copying")
        raw = source_path.read_bytes()
        if (
            len(raw) != entry["size"]
            or hashlib.sha256(raw).hexdigest() != entry["sha256"]
        ):
            raise BootstrapError(f"local source {component} changed while copying")
        output = destination / relative
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(raw)
        output.chmod(0o755 if entry["executable"] else 0o644)
    return observed


def fetch_git_source(
    repository: str,
    commit: str,
    destination: Path,
    expected_source_sha256: str,
    *,
    allowed_repositories: Optional[set[str]] = None,
    timeout_seconds: int = 120,
) -> dict[str, Any]:
    """Fetch one exact commit and verify the complete detached source tree."""
    allowed = allowed_repositories or set(KNOWN_REPOSITORIES.values())
    if repository not in allowed:
        raise BootstrapError("Git repository is not allowlisted")
    if HEX_40.fullmatch(commit) is None:
        raise BootstrapError("Git commit must be a full immutable hash")
    if destination.exists():
        raise BootstrapError("Git destination already exists")
    destination.mkdir(parents=True, mode=0o700)
    environment = dict(os.environ, GIT_TERMINAL_PROMPT="0")
    commands = [
        ["git", "init", "-q"],
        ["git", "remote", "add", "origin", repository],
        ["git", "fetch", "--no-tags", "--depth", "1", "origin", commit],
        ["git", "checkout", "-q", "--detach", "FETCH_HEAD"],
    ]
    try:
        for command in commands:
            run_process(
                command,
                cwd=destination,
                env=environment,
                timeout_seconds=timeout_seconds,
            )
        completed = run_process(
            ["git", "rev-parse", "HEAD"],
            cwd=destination,
            env=environment,
            timeout_seconds=timeout_seconds,
        )
        observed_commit = (completed.stdout or b"").decode().strip()
    except ProcessFailure as error:
        raise BootstrapError("immutable Git source fetch failed") from error
    if observed_commit != commit:
        raise BootstrapError("Git checkout did not resolve to the requested commit")
    component = next(
        (name for name, url in KNOWN_REPOSITORIES.items() if url == repository),
        "protocol",
    )
    observed = source_manifest_for(
        component, destination, KNOWN_REPOSITORIES.get(component, repository), commit
    )
    if observed["source_sha256"] != expected_source_sha256:
        raise BootstrapError("Git source digest mismatch")
    return observed


class PrefixLock:
    """One nonblocking process lock for a complete installation prefix."""

    def __init__(self, prefix: Path) -> None:
        self.prefix = _prefix_path(prefix)
        self._stream: Optional[Any] = None
        self._directory_fd: Optional[int] = None

    def __enter__(self) -> "PrefixLock":
        self._directory_fd = open_directory_nofollow(self.prefix, create=True)
        flags = os.O_RDWR | os.O_CREAT
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        try:
            descriptor = os.open(
                ".bootstrap.lock", flags, 0o600, dir_fd=self._directory_fd
            )
        except OSError as error:
            os.close(self._directory_fd)
            self._directory_fd = None
            raise BootstrapError("installation lock path is unsafe") from error
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            os.close(descriptor)
            os.close(self._directory_fd)
            self._directory_fd = None
            raise BootstrapError("installation lock path is unsafe")
        self._stream = os.fdopen(descriptor, "a+")
        os.fchmod(self._stream.fileno(), 0o600)
        try:
            fcntl.flock(self._stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            self._stream.close()
            self._stream = None
            os.close(self._directory_fd)
            self._directory_fd = None
            raise BootstrapError("installation prefix is busy") from error
        return self

    def __exit__(self, *_args: Any) -> None:
        if self._stream is not None:
            fcntl.flock(self._stream.fileno(), fcntl.LOCK_UN)
            self._stream.close()
        if self._directory_fd is not None:
            os.close(self._directory_fd)
            self._directory_fd = None


def _write_json(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        json.dump(payload, stream, indent=2, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    _fsync_directory(path.parent)


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def active_release_id(prefix: Path) -> Optional[str]:
    active = prefix / "active"
    if not active.is_symlink():
        return None
    target = os.readlink(active)
    path = Path(target)
    if path.is_absolute() or len(path.parts) != 2 or path.parts[0] != "releases":
        raise BootstrapError("active release link is invalid")
    return path.parts[1]


def _activate(prefix: Path, release_id: str) -> None:
    release = prefix / "releases" / release_id
    if not release.is_dir():
        raise BootstrapError("release to activate is missing")
    temporary = prefix / f".active.{uuid.uuid4().hex}"
    temporary.symlink_to(Path("releases") / release_id)
    os.replace(temporary, prefix / "active")
    _fsync_directory(prefix)


def _restore_active(prefix: Path, release_id: Optional[str]) -> None:
    active = prefix / "active"
    if release_id is None:
        if active.is_symlink():
            active.unlink()
            _fsync_directory(prefix)
        elif active.exists():
            raise BootstrapError("active path cannot be restored safely")
    else:
        _activate(prefix, release_id)


def _history(prefix: Path) -> list[str]:
    path = prefix / "history.json"
    if not path.exists():
        return []
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BootstrapError("installation history is unreadable") from error
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise BootstrapError("installation history is invalid")
    return value


def _transaction_checkpoint(_phase: str) -> None:
    """Test hook placed after every durable transaction boundary."""


def _apply_external(prefix: Path, entries: Iterable[Mapping[str, Any]]) -> None:
    from .targets import apply_external_state

    apply_external_state(entries, prefix)


def _recover_transaction(prefix: Path) -> Optional[str]:
    journal_path = prefix / "transaction.json"
    if not journal_path.exists():
        return None
    try:
        journal = json.loads(journal_path.read_text(encoding="utf-8"))
        if (
            journal.get("schema_version") != 1
            or journal.get("phase")
            not in {"intent", "targets", "history", "active", "committed"}
            or not isinstance(journal.get("before"), dict)
            or not isinstance(journal.get("after"), dict)
        ):
            raise BootstrapError("transaction journal is invalid")
        committed = journal["phase"] == "committed"
        state = journal["after"] if committed else journal["before"]
        state = _verify_recovery_state(prefix, state, restoring_before=not committed)
        _apply_external(prefix, state["external"])
        _write_json(prefix / "history.json", state["history"])
        _restore_active(prefix, state["active"])
        journal_path.unlink()
        _fsync_directory(prefix)
        return "completed" if journal["phase"] == "committed" else "rolled-back"
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise BootstrapError("transaction recovery failed") from error


def _commit_transition(
    prefix: Path,
    release_id: str,
    history: list[str],
    external_plan: Optional[Mapping[str, Any]] = None,
) -> None:
    """Durably commit active/history/managed targets as one recoverable unit."""
    journal_path = prefix / "transaction.json"
    if journal_path.exists():
        raise BootstrapError("unrecovered transaction journal remains")
    external_plan = external_plan or {"before": [], "after": []}
    journal = {
        "schema_version": 1,
        "operation_id": uuid.uuid4().hex,
        "phase": "intent",
        "before": {
            "active": active_release_id(prefix),
            "history": _history(prefix),
            "external": list(external_plan.get("before", [])),
        },
        "after": {
            "active": release_id,
            "history": history,
            "external": list(external_plan.get("after", [])),
        },
    }
    _write_json(journal_path, journal)
    _transaction_checkpoint("intent")
    try:
        _apply_external(prefix, journal["after"]["external"])
        journal["phase"] = "targets"
        _write_json(journal_path, journal)
        _transaction_checkpoint("targets")
        _write_json(prefix / "history.json", history)
        journal["phase"] = "history"
        _write_json(journal_path, journal)
        _transaction_checkpoint("history")
        _activate(prefix, release_id)
        journal["phase"] = "active"
        _write_json(journal_path, journal)
        _transaction_checkpoint("active")
        journal["phase"] = "committed"
        _write_json(journal_path, journal)
        _transaction_checkpoint("committed")
        journal_path.unlink()
        _fsync_directory(prefix)
    except BaseException:
        _recover_transaction(prefix)
        raise


def _switch_activation(prefix: Path, release_id: str, history: list[str]) -> None:
    """Compatibility wrapper using the durable transaction protocol."""
    _commit_transition(prefix, release_id, history)


def _iter_output_files(root: Path) -> list[Path]:
    if root.is_symlink() or not root.is_dir():
        raise BootstrapError("output root is missing or unsafe")
    files: list[Path] = []
    total = 0
    for directory, names, filenames in os.walk(root, followlinks=False):
        base = Path(directory)
        names[:] = sorted(names)
        for name in names:
            path = base / name
            if path.is_symlink() or not stat.S_ISDIR(path.lstat().st_mode):
                raise BootstrapError(
                    f"output contains unsupported link or type: {path}"
                )
        for filename in sorted(filenames):
            path = base / filename
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode):
                raise BootstrapError(
                    f"output contains unsupported link or type: {path}"
                )
            total += metadata.st_size
            files.append(path)
            if len(files) > MAX_SOURCE_FILES or total > MAX_SOURCE_BYTES:
                raise BootstrapError("output exceeds bootstrap bounds")
    return files


def _directory_manifest(root: Path) -> dict[str, Any]:
    if not root.exists():
        raise BootstrapError("component recipe produced no output")
    files = []
    for path in _iter_output_files(root):
        raw = path.read_bytes()
        files.append(
            {
                "path": path.relative_to(root).as_posix(),
                "sha256": hashlib.sha256(raw).hexdigest(),
                "size": len(raw),
                "executable": bool(path.stat().st_mode & 0o111),
            }
        )
    return {
        "schema_version": 2,
        "algorithm": "sha256-path-size-mode-v1",
        "sha256": _source_digest(files),
        "files": files,
    }


def _read_receipt(prefix: Path, release_id: str) -> dict[str, Any]:
    try:
        value = json.loads(
            (prefix / "releases" / release_id / "receipt.json").read_text(
                encoding="utf-8"
            )
        )
    except (OSError, json.JSONDecodeError) as error:
        raise BootstrapError("installation receipt is unreadable") from error
    if not isinstance(value, dict):
        raise BootstrapError("installation receipt is invalid")
    return value


def _verify_source_snapshot(
    name: str, source_path: Path, stored_source: Mapping[str, Any]
) -> None:
    observed_files = []
    for path in _iter_source_files(source_path):
        raw = path.read_bytes()
        observed_files.append(
            {
                "path": path.relative_to(source_path).as_posix(),
                "sha256": hashlib.sha256(raw).hexdigest(),
                "size": len(raw),
                "executable": bool(path.stat().st_mode & 0o111),
            }
        )
    if stored_source.get("files") != observed_files or stored_source.get(
        "source_sha256"
    ) != _source_digest(observed_files):
        raise BootstrapError(f"retained source bytes changed: {name}")


def _verify_ha_target_provenance(
    receipt: Mapping[str, Any], components: Mapping[str, Any]
) -> None:
    component = components.get("home-assistant")
    if component is None:
        return
    if not isinstance(component, Mapping):
        raise BootstrapError("retained Home Assistant target provenance is invalid")
    artifacts = component.get("artifacts")
    targets = receipt.get("targets")
    if not isinstance(artifacts, Mapping) or not isinstance(targets, Mapping):
        raise BootstrapError("retained Home Assistant target provenance is invalid")
    for key in ("payload_manifest_sha256", "selection_receipt_sha256"):
        artifact_value = artifacts.get(key)
        target_value = targets.get(f"ha_{key}")
        if (
            not isinstance(artifact_value, str)
            or HEX_64.fullmatch(artifact_value) is None
            or target_value != artifact_value
        ):
            raise BootstrapError("retained Home Assistant target provenance changed")


def _verify_release(prefix: Path, release_id: str) -> dict[str, Any]:
    """Recompute every retained source and output identity before reuse."""
    release = prefix / "releases" / release_id
    receipt = _read_receipt(prefix, release_id)
    if receipt.get("schema_version") != 2:
        raise BootstrapError("retained release uses an incomplete receipt schema")
    components = receipt.get("components")
    if not isinstance(components, dict) or not components:
        raise BootstrapError("retained release component receipt is invalid")
    for name, component_receipt in sorted(components.items()):
        if not isinstance(name, str) or not isinstance(component_receipt, dict):
            raise BootstrapError("retained release component receipt is invalid")
        component_root = release / "components" / name
        source_manifest_path = component_root / "source-manifest.json"
        try:
            stored_source = json.loads(source_manifest_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise BootstrapError(
                f"retained source manifest is unreadable: {name}"
            ) from error
        source_receipt = component_receipt.get("source")
        if not isinstance(source_receipt, dict):
            raise BootstrapError(f"retained source receipt is invalid: {name}")
        if hashlib.sha256(source_manifest_path.read_bytes()).hexdigest() != (
            source_receipt.get("manifest_sha256")
        ):
            raise BootstrapError(f"retained source manifest changed: {name}")
        source_path = component_root / "source"
        _verify_source_snapshot(name, source_path, stored_source)
        expected_output = component_receipt.get("outputs")
        if (
            not isinstance(expected_output, dict)
            or expected_output.get("schema_version") != 2
            or expected_output.get("algorithm") != "sha256-path-size-mode-v1"
            or expected_output != _directory_manifest(component_root / "output")
        ):
            raise BootstrapError(f"retained output bytes changed: {name}")
    _verify_ha_target_provenance(receipt, components)
    return receipt


def _verify_recovery_state(
    prefix: Path, state: Mapping[str, Any], *, restoring_before: bool
) -> dict[str, Any]:
    """Validate a selected recovery state completely before its first mutation."""
    if not isinstance(state, Mapping) or set(state) != {
        "active",
        "history",
        "external",
    }:
        raise BootstrapError("transaction recovery payload is invalid")
    active = state["active"]
    if active is not None and (
        not isinstance(active, str)
        or Path(active).name != active
        or active in {"", ".", ".."}
    ):
        raise BootstrapError("transaction recovery release is invalid")
    history = state["history"]
    if not isinstance(history, list) or any(
        not isinstance(item, str) or Path(item).name != item or item in {"", ".", ".."}
        for item in history
    ):
        raise BootstrapError("transaction recovery history is invalid")
    external = state["external"]
    if not isinstance(external, list):
        raise BootstrapError("transaction recovery targets are invalid")
    receipt = None if active is None else _verify_release(prefix, active)
    from .targets import validate_recovery_target_plan

    validated_external = validate_recovery_target_plan(
        prefix,
        active,
        receipt,
        external,
        restoring_before=restoring_before,
    )
    return {
        "active": active,
        "history": list(history),
        "external": validated_external,
    }


def validate_selected_sources(
    cohort: Cohort, records: Mapping[str, Mapping[str, Any]]
) -> None:
    """Bind selected records and cross-component dependencies before any build."""
    from .recipes import validate_component_set

    validate_component_set(cohort.components)
    for name, component in sorted(cohort.components.items()):
        record = records.get(name)
        if record is None:
            raise BootstrapError(f"local source manifest is missing {name}")
        if (
            record.get("repository") != component.repository
            or record.get("commit") != component.commit
            or record.get("source_sha256") != component.source_sha256
        ):
            raise BootstrapError(f"local source manifest does not match {name}")


def _preserve_failure(
    prefix: Path,
    staging: Path,
    error: BaseException,
    active_before: Optional[str],
    input_sha256: str,
) -> Optional[Path]:
    """Retain bounded logs and a private failure marker outside disposable staging."""
    try:
        failure_root = prefix / "failures" / staging.name
        failure_root.mkdir(parents=True, mode=0o700)
        for log in sorted((staging / "components").glob("*/build.log")):
            destination = failure_root / f"{log.parent.name}.log"
            shutil.copy2(log, destination)
            destination.chmod(0o600)
        marker = failure_root / "failure.json"
        _write_json(
            marker,
            {
                "schema_version": 1,
                "status": "failed",
                "error_type": error.__class__.__name__,
                "error": str(error),
                "active_release_before": active_before,
                "input_sha256": input_sha256,
            },
        )
        return marker
    except OSError:
        return None


def _component_payload(component: Component) -> dict[str, Any]:
    payload = asdict(component)
    payload.pop("name")
    return payload


def installation_input_sha256(
    cohort: Cohort,
    installation_mode: str,
    target_binding: Optional[Mapping[str, str]] = None,
    hub_version: Optional[str] = None,
) -> str:
    """Hash immutable source selection separately from the resulting receipt."""
    payload = {
        "product_version": cohort.product_version,
        "hub_version": hub_version or cohort.product_version,
        "publication_status": cohort.publication_status,
        "admitted_hub_versions": sorted(cohort.admitted_hub_versions),
        "installation_mode": installation_mode,
        "recipe_revision": RECIPE_REVISION,
        "targets": dict(sorted((target_binding or {}).items())),
        "components": {
            name: _component_payload(component)
            for name, component in sorted(cohort.components.items())
        },
    }
    return hashlib.sha256(
        json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def is_identical_active(
    prefix: Path,
    cohort: Cohort,
    installation_mode: str,
    target_binding: Optional[Mapping[str, str]] = None,
    hub_version: Optional[str] = None,
) -> bool:
    """Verify the complete active release before reporting an identical input."""
    prefix = _prefix_path(prefix)
    binding = _bind_component_target_provenance(cohort, target_binding)
    with PrefixLock(prefix):
        _recover_transaction(prefix)
        current = active_release_id(prefix)
        if current is None:
            return False
        receipt = _verify_release(prefix, current)
        return receipt.get("input_sha256") == installation_input_sha256(
            cohort, installation_mode, binding, hub_version
        )


def _validate_install_request(cohort: Cohort, installation_mode: str) -> None:
    if os.geteuid() == 0:
        raise BootstrapError("companion builds must run as an unprivileged user")
    if installation_mode not in {"local-unpublished", "production-git"}:
        raise BootstrapError("invalid installation mode")
    if (
        installation_mode == "production-git"
        and cohort.publication_status != "published"
    ):
        raise BootstrapError("production Git mode requires a published cohort")
    if (
        installation_mode == "local-unpublished"
        and cohort.publication_status != "local-unpublished"
    ):
        raise BootstrapError("local candidate mode requires an unpublished cohort")
    from .recipes import validate_component_set

    validate_component_set(cohort.components)


def _bind_component_target_provenance(
    cohort: Cohort, target_binding: Optional[Mapping[str, str]]
) -> dict[str, str]:
    """Bind target-sensitive component identities from the selected cohort."""
    binding = dict(sorted((target_binding or {}).items()))
    home_assistant = cohort.components.get("home-assistant")
    if home_assistant is None:
        return binding
    for key in ("payload_manifest_sha256", "selection_receipt_sha256"):
        value = home_assistant.artifacts.get(key)
        target_key = f"ha_{key}"
        if not isinstance(value, str):
            raise BootstrapError("Home Assistant target provenance is incomplete")
        existing = binding.get(target_key)
        if existing is not None and existing != value:
            raise BootstrapError("Home Assistant target provenance conflicts with cohort")
        binding[target_key] = value
    return dict(sorted(binding.items()))


def _install_materialized_locked(
    prefix: Path,
    cohort: Cohort,
    local_sources: Mapping[str, Mapping[str, Any]],
    build_component: Callable[[str, Path, Path], Mapping[str, Any]],
    *,
    installation_mode: str,
    binding: Mapping[str, str],
    validate_component: Optional[Callable[[str, Path], Mapping[str, Any]]],
    hub_version: Optional[str],
    input_sha256: str,
    current: Optional[str],
    current_receipt: Optional[dict[str, Any]],
    current_verified: bool,
) -> dict[str, Any]:
    staging_root = prefix / ".staging"
    releases = prefix / "releases"
    staging_root.mkdir(exist_ok=True, mode=0o700)
    releases.mkdir(exist_ok=True, mode=0o700)
    staging = staging_root / uuid.uuid4().hex
    staging.mkdir(mode=0o700)
    base_release_id = f"{cohort.product_version}-{input_sha256[:16]}"
    release_id = base_release_id
    destination = releases / release_id
    try:
        materialized = {}
        for name, component in sorted(cohort.components.items()):
            record = local_sources[name]
            component_root = staging / "components" / name
            source_path = component_root / "source"
            observed = verify_local_source(record, source_path)
            materialized[name] = (component_root, source_path, observed)
        if validate_component is not None:
            for name, (_component_root, source_path, _observed) in sorted(
                materialized.items()
            ):
                validate_component(name, source_path)
        component_receipts = {}
        for name, (component_root, source_path, observed) in sorted(
            materialized.items()
        ):
            component = cohort.components[name]
            output_path = component_root / "output"
            build = dict(build_component(name, source_path, output_path))
            _verify_source_snapshot(name, source_path, observed)
            source_manifest_path = component_root / "source-manifest.json"
            _write_json(source_manifest_path, observed)
            component_receipts[name] = {
                "product_version": component.product_version,
                "profile": asdict(component.profile),
                "artifacts": dict(component.artifacts),
                "source": {
                    "transport": (
                        "git-immutable"
                        if installation_mode == "production-git"
                        else "local-content-bound"
                    ),
                    "public_availability": (
                        "verified"
                        if installation_mode == "production-git"
                        else "pending"
                    ),
                    "repository": component.repository,
                    "commit": component.commit,
                    "sha256": component.source_sha256,
                    "manifest_sha256": hashlib.sha256(
                        source_manifest_path.read_bytes()
                    ).hexdigest(),
                },
                "dependencies": build.get("dependencies", {}),
                "commands": build.get("commands", []),
                "tools": build.get("tools", {}),
                "verification": build.get("verification", {}),
                "outputs": _directory_manifest(output_path),
            }
        receipt = {
            "schema_version": 2,
            "status": "installed",
            "installation_mode": installation_mode,
            "product_version": cohort.product_version,
            "hub_version": hub_version or cohort.product_version,
            "admitted_hub_versions": sorted(cohort.admitted_hub_versions),
            "input_sha256": input_sha256,
            "recipe_revision": RECIPE_REVISION,
            "targets": binding,
            "components": component_receipts,
        }
        _write_json(staging / "receipt.json", receipt)
        if destination.exists():
            try:
                existing = _verify_release(prefix, release_id)
            except BootstrapError:
                release_id = f"{base_release_id}-repair-{uuid.uuid4().hex[:8]}"
                destination = releases / release_id
                os.replace(staging, destination)
                _fsync_directory(releases)
            else:
                if existing.get("input_sha256") != input_sha256:
                    raise BootstrapError("release identifier collision")
                shutil.rmtree(staging)
        else:
            os.replace(staging, destination)
            _fsync_directory(releases)

        from .targets import prepare_target_transition

        plan = prepare_target_transition(
            prefix,
            release_id,
            cohort.components,
            binding,
            current_receipt,
        )
        history = _history(prefix)
        if current is not None and current != release_id and current_verified:
            history.append(current)
        _commit_transition(prefix, release_id, history, plan)
        return {
            "status": "installed",
            "active_release": release_id,
            "previous_release": current,
            "input_sha256": input_sha256,
            "receipt": str(destination / "receipt.json"),
            "external_actions": plan["actions"],
        }
    except BaseException as error:
        evidence = _preserve_failure(prefix, staging, error, current, input_sha256)
        if staging.exists():
            shutil.rmtree(staging)
        if evidence is not None and isinstance(error, BootstrapError):
            raise BootstrapError(f"{error}; failure evidence: {evidence}") from error
        raise


def install_cohort_from_provider(
    prefix: Path,
    cohort: Cohort,
    source_provider: Callable[[Path], Mapping[str, Mapping[str, Any]]],
    build_component: Callable[[str, Path, Path], Mapping[str, Any]],
    *,
    installation_mode: str = "local-unpublished",
    target_binding: Optional[Mapping[str, str]] = None,
    validate_component: Optional[Callable[[str, Path], Mapping[str, Any]]] = None,
    check_environment: Optional[Callable[[], None]] = None,
    hub_version: Optional[str] = None,
) -> dict[str, Any]:
    """No-op or materialize and install while holding one operation lock."""
    _validate_install_request(cohort, installation_mode)
    prefix = _prefix_path(prefix)
    binding = _bind_component_target_provenance(cohort, target_binding)
    with PrefixLock(prefix):
        _recover_transaction(prefix)
        input_sha256 = installation_input_sha256(
            cohort, installation_mode, binding, hub_version
        )
        current = active_release_id(prefix)
        current_receipt: Optional[dict[str, Any]] = None
        current_verified = False
        if current is not None:
            try:
                current_receipt = _verify_release(prefix, current)
                current_verified = True
            except BootstrapError:
                current_receipt = _read_receipt(prefix, current)
            if current_verified and current_receipt.get("input_sha256") == input_sha256:
                from .targets import prepare_target_transition

                plan = prepare_target_transition(
                    prefix, current, cohort.components, binding, current_receipt
                )
                _commit_transition(prefix, current, _history(prefix), plan)
                return {
                    "status": "no-op",
                    "active_release": current,
                    "input_sha256": input_sha256,
                    "external_actions": plan["actions"],
                }
        if check_environment is not None:
            check_environment()
        with tempfile.TemporaryDirectory(
            prefix="teslatlas-companion-materialize-"
        ) as temporary:
            records = source_provider(Path(temporary))
            validate_selected_sources(cohort, records)
            return _install_materialized_locked(
                prefix,
                cohort,
                records,
                build_component,
                installation_mode=installation_mode,
                binding=binding,
                validate_component=validate_component,
                hub_version=hub_version,
                input_sha256=input_sha256,
                current=current,
                current_receipt=current_receipt,
                current_verified=current_verified,
            )


def install_cohort(
    prefix: Path,
    cohort: Cohort,
    local_sources: Mapping[str, Mapping[str, Any]],
    build_component: Callable[[str, Path, Path], Mapping[str, Any]],
    *,
    installation_mode: str = "local-unpublished",
    target_binding: Optional[Mapping[str, str]] = None,
    validate_component: Optional[Callable[[str, Path], Mapping[str, Any]]] = None,
    hub_version: Optional[str] = None,
) -> dict[str, Any]:
    """Install already available content-bound records under one operation lock."""
    return install_cohort_from_provider(
        prefix,
        cohort,
        lambda _temporary: local_sources,
        build_component,
        installation_mode=installation_mode,
        target_binding=target_binding,
        validate_component=validate_component,
        hub_version=hub_version,
    )


def rollback(prefix: Path) -> dict[str, Any]:
    """Select the newest verified release and reconcile its managed targets."""
    prefix = _prefix_path(prefix)
    with PrefixLock(prefix):
        _recover_transaction(prefix)
        current = active_release_id(prefix)
        history = _history(prefix)
        if current is None:
            raise BootstrapError("no prior working release is available")
        current_receipt = _read_receipt(prefix, current)
        target: Optional[str] = None
        target_receipt: Optional[dict[str, Any]] = None
        while history:
            candidate = history.pop()
            if candidate == current:
                continue
            try:
                target_receipt = _verify_release(prefix, candidate)
            except BootstrapError:
                continue
            target = candidate
            break
        if target is None or target_receipt is None:
            raise BootstrapError("no prior verified working release is available")
        from .targets import prepare_target_transition

        plan = prepare_target_transition(
            prefix,
            target,
            target_receipt["components"],
            target_receipt.get("targets", {}),
            current_receipt,
        )
        _commit_transition(prefix, target, history, plan)
        return {
            "status": "rolled-back",
            "active_release": target,
            "previous_release": current,
            "external_actions": plan["actions"],
        }


def status(prefix: Path) -> dict[str, Any]:
    """Recover an interrupted transaction, then return verified active state."""
    if os.geteuid() == 0:
        raise BootstrapError("companion status recovery must not run as root")
    prefix = _prefix_path(prefix)
    with PrefixLock(prefix):
        recovered = _recover_transaction(prefix)
        current = active_release_id(prefix)
        if current is None:
            result = {"status": "not-installed", "active_release": None}
            if recovered is not None:
                result["recovery"] = recovered
            return result
        receipt = _verify_release(prefix, current)
        receipt_path = prefix / "releases" / current / "receipt.json"
        result = {
            "status": "installed",
            "active_release": current,
            "receipt": str(receipt_path),
            "installation_mode": receipt.get("installation_mode"),
            "product_version": receipt.get("product_version"),
            "hub_version": receipt.get("hub_version"),
            "input_sha256": receipt.get("input_sha256"),
            "components": sorted(receipt.get("components", {})),
        }
        if recovered is not None:
            result["recovery"] = recovered
        return result
