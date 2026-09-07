# SPDX-License-Identifier: AGPL-3.0-only
"""Machine-readable noninteractive companion bootstrap interface."""

from __future__ import annotations

import argparse
import errno
import hashlib
import http.client
import json
import os
import signal
import ssl
import stat
import tempfile
import time
import uuid
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Callable, Optional

from .core import (
    BootstrapError,
    Cohort,
    PrefixLock,
    _prefix_path,
    fetch_git_source,
    install_cohort_from_provider,
    load_catalog,
    open_directory_nofollow,
    parse_catalog,
    rollback,
    select_cohort,
    source_manifest_for,
    status,
    validate_selected_sources,
    verify_local_source,
)
from .processes import ProcessFailure, run_process
from .recipes import (
    KNOWN_REPOSITORIES,
    RecipeContext,
    bind_viewer_sdk_artifact,
    build_component,
    check_recipe_environment,
    validate_component_set,
    validate_source_metadata,
)
from .targets import validate_targets


CANONICAL_CATALOG_URL = "https://raw.githubusercontent.com/magrathean-uk/teslatlas-hub/refs/heads/main/tools/companions/catalog-current.json"
CATALOG_SELECTION_SCHEMA = "teslatlas.companion-catalog-selection/v1"
MAX_CATALOG_BYTES = 1024 * 1024
CATALOG_HOST = "raw.githubusercontent.com"
CATALOG_PATH = "/magrathean-uk/teslatlas-hub/refs/heads/main/tools/companions/catalog-current.json"
CATALOG_TIMEOUT_SECONDS = 15


class _CatalogDeadlineExpired(TimeoutError):
    pass


@contextmanager
def _catalog_deadline(seconds: float):
    """Bound every blocking phase of the synchronous HTTP transaction."""
    started = time.monotonic()
    previous_handler = signal.getsignal(signal.SIGALRM)
    previous_timer = signal.getitimer(signal.ITIMER_REAL)

    def expired(_signal: int, _frame: Any) -> None:
        raise _CatalogDeadlineExpired(errno.ETIMEDOUT, "catalog deadline expired")

    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous_handler)
        if previous_timer[0] > 0:
            remaining = max(previous_timer[0] - (time.monotonic() - started), 0.000001)
            signal.setitimer(signal.ITIMER_REAL, remaining, previous_timer[1])


def _fetch_canonical_catalog() -> bytes:
    try:
        with _catalog_deadline(CATALOG_TIMEOUT_SECONDS):
            connection = http.client.HTTPSConnection(
                CATALOG_HOST,
                timeout=CATALOG_TIMEOUT_SECONDS,
                context=ssl.create_default_context(),
            )
            try:
                connection.request(
                    "GET", CATALOG_PATH, headers={"Accept": "application/json"}
                )
                response = connection.getresponse()
                if response.status != 200:
                    raise BootstrapError(
                        "canonical companion catalog response is invalid"
                    )
                length = response.getheader("Content-Length")
                expected_length: Optional[int] = None
                if not getattr(response, "chunked", False) and length is not None:
                    if not length or not length.isascii() or not length.isdecimal():
                        raise BootstrapError(
                            "canonical companion catalog Content-Length is invalid"
                        )
                    expected_length = int(length)
                    if expected_length > MAX_CATALOG_BYTES:
                        raise BootstrapError(
                            "canonical companion catalog exceeds the fixed size limit"
                        )
                if connection.sock is not None:
                    connection.sock.settimeout(CATALOG_TIMEOUT_SECONDS)
                chunks: list[bytes] = []
                remaining_bytes = MAX_CATALOG_BYTES + 1
                while remaining_bytes:
                    chunk = response.read(min(remaining_bytes, 64 * 1024))
                    if not chunk:
                        break
                    chunks.append(chunk)
                    remaining_bytes -= len(chunk)
            finally:
                connection.close()
    except TimeoutError as error:
        raise BootstrapError(
            "canonical companion catalog refresh exceeded its deadline"
        ) from error
    except http.client.HTTPException as error:
        raise BootstrapError(
            "canonical companion catalog response could not be decoded"
        ) from error
    except (OSError, ValueError) as error:
        raise BootstrapError("canonical companion catalog refresh failed") from error
    payload = b"".join(chunks)
    if expected_length is not None and len(payload) != expected_length:
        raise BootstrapError("canonical companion catalog transfer is incomplete")
    if len(payload) > MAX_CATALOG_BYTES:
        raise BootstrapError("canonical companion catalog exceeds the fixed size limit")
    return payload


def _admit_catalog_cache(prefix: Path) -> None:
    """Reject unsafe existing cache ancestors without creating or changing them."""
    prefix = _prefix_path(prefix)
    if not prefix.exists():
        return
    descriptor = open_directory_nofollow(prefix, create=False)
    os.close(descriptor)
    cache = prefix / "catalog-cache"
    if cache.exists() or cache.is_symlink():
        descriptor = open_directory_nofollow(cache, create=False)
        os.close(descriptor)
    generations = cache / "generations"
    if generations.exists() or generations.is_symlink():
        descriptor = open_directory_nofollow(generations, create=False)
        os.close(descriptor)


def _read_regular_at(directory_fd: int, name: str, limit: int) -> bytes:
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    if hasattr(os, "O_NONBLOCK"):
        flags |= os.O_NONBLOCK
    descriptor = os.open(name, flags, dir_fd=directory_fd)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > limit:
            raise BootstrapError("cached companion catalog path is unsafe")
        chunks: list[bytes] = []
        remaining = limit + 1
        while remaining:
            chunk = os.read(descriptor, min(remaining, 64 * 1024))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        payload = b"".join(chunks)
        if len(payload) != metadata.st_size:
            raise BootstrapError("cached companion catalog changed while reading")
        return payload
    finally:
        os.close(descriptor)


def _catalog_selection(prefix: Path, shipped_catalog: Path) -> Path:
    prefix = _prefix_path(prefix)
    _admit_catalog_cache(prefix)
    cache = prefix / "catalog-cache"
    if not cache.exists():
        return shipped_catalog
    cache_fd = open_directory_nofollow(cache, create=False)
    try:
        try:
            raw_selection = _read_regular_at(cache_fd, "current.json", 4096)
        except FileNotFoundError:
            return shipped_catalog
        try:
            selection = json.loads(raw_selection)
        except (UnicodeError, json.JSONDecodeError) as error:
            raise BootstrapError("cached companion catalog selection is invalid") from error
        if (
            not isinstance(selection, dict)
            or set(selection) != {"schema", "origin", "sha256", "bytes", "generation"}
            or selection["schema"] != CATALOG_SELECTION_SCHEMA
            or selection["origin"] != CANONICAL_CATALOG_URL
            or not isinstance(selection["sha256"], str)
            or len(selection["sha256"]) != 64
            or any(character not in "0123456789abcdef" for character in selection["sha256"])
            or selection["generation"] != f"{selection['sha256']}.json"
            or not isinstance(selection["bytes"], int)
            or isinstance(selection["bytes"], bool)
            or not 0 <= selection["bytes"] <= MAX_CATALOG_BYTES
        ):
            raise BootstrapError("cached companion catalog selection is invalid")
        generations_fd = os.open(
            "generations",
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0),
            dir_fd=cache_fd,
        )
        try:
            payload = _read_regular_at(
                generations_fd, selection["generation"], MAX_CATALOG_BYTES
            )
        finally:
            os.close(generations_fd)
        if (
            len(payload) != selection["bytes"]
            or hashlib.sha256(payload).hexdigest() != selection["sha256"]
        ):
            raise BootstrapError("cached companion catalog identity does not match")
        try:
            parse_catalog(json.loads(payload))
        except (UnicodeError, json.JSONDecodeError) as error:
            raise BootstrapError("cached companion catalog is not valid JSON") from error
        return cache / "generations" / selection["generation"]
    finally:
        os.close(cache_fd)


def _promote_catalog(
    prefix: Path,
    payload: bytes,
    checkpoint: Callable[[str], None] = lambda _phase: None,
) -> Path:
    prefix = _prefix_path(prefix)
    try:
        parse_catalog(json.loads(payload))
    except (UnicodeError, json.JSONDecodeError) as error:
        raise BootstrapError("canonical companion catalog is not valid JSON") from error
    digest = hashlib.sha256(payload).hexdigest()
    selection = json.dumps(
        {
            "schema": CATALOG_SELECTION_SCHEMA,
            "origin": CANONICAL_CATALOG_URL,
            "sha256": digest,
            "bytes": len(payload),
            "generation": f"{digest}.json",
        },
        sort_keys=True,
        separators=(",", ":"),
    ).encode() + b"\n"
    with PrefixLock(prefix):
        cache = prefix / "catalog-cache"
        generations = cache / "generations"
        cache_fd = open_directory_nofollow(cache, create=True)
        generations_fd = open_directory_nofollow(generations, create=True)
        try:
            generation_name = f"{digest}.json"
            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
            if hasattr(os, "O_NOFOLLOW"):
                flags |= os.O_NOFOLLOW
            try:
                existing = _read_regular_at(
                    generations_fd, generation_name, MAX_CATALOG_BYTES
                )
            except FileNotFoundError:
                generation_temporary = f".generation.{uuid.uuid4().hex}.tmp"
                generation_fd = os.open(
                    generation_temporary, flags, 0o600, dir_fd=generations_fd
                )
                try:
                    checkpoint("generation-write-opened")
                    with os.fdopen(generation_fd, "wb") as stream:
                        stream.write(payload)
                        stream.flush()
                        os.fsync(stream.fileno())
                    os.link(
                        generation_temporary,
                        generation_name,
                        src_dir_fd=generations_fd,
                        dst_dir_fd=generations_fd,
                        follow_symlinks=False,
                    )
                    os.unlink(generation_temporary, dir_fd=generations_fd)
                    os.fsync(generations_fd)
                except BaseException:
                    try:
                        os.unlink(generation_temporary, dir_fd=generations_fd)
                    except FileNotFoundError:
                        pass
                    raise
            else:
                if existing != payload:
                    raise BootstrapError("cached catalog generation identity collision")
            checkpoint("generation-persisted")
            temporary = f".current.{uuid.uuid4().hex}.tmp"
            pointer_fd = os.open(temporary, flags, 0o600, dir_fd=cache_fd)
            try:
                with os.fdopen(pointer_fd, "wb") as stream:
                    stream.write(selection)
                    stream.flush()
                    os.fsync(stream.fileno())
                checkpoint("selection-persisted")
                os.replace(
                    temporary,
                    "current.json",
                    src_dir_fd=cache_fd,
                    dst_dir_fd=cache_fd,
                )
                os.fsync(cache_fd)
            except BaseException:
                try:
                    os.unlink(temporary, dir_fd=cache_fd)
                except FileNotFoundError:
                    pass
                raise
        finally:
            os.close(generations_fd)
            os.close(cache_fd)
    return generations / f"{digest}.json"


def _refresh_catalog(
    prefix: Path,
    fetch: Callable[[], bytes] = _fetch_canonical_catalog,
    checkpoint: Callable[[str], None] = lambda _phase: None,
) -> Path:
    prefix = _prefix_path(prefix)
    _admit_catalog_cache(prefix)
    payload = fetch()
    if len(payload) > MAX_CATALOG_BYTES:
        raise BootstrapError("canonical companion catalog exceeds the fixed size limit")
    return _promote_catalog(prefix, payload, checkpoint)


class JsonArgumentParser(argparse.ArgumentParser):
    def error(self, message: str) -> None:
        raise BootstrapError(message)


def _parser() -> argparse.ArgumentParser:
    parser = JsonArgumentParser(prog="bootstrap-companions.py", add_help=True)
    parser.add_argument(
        "action",
        choices=("install", "update", "status", "rollback", "dry-run", "manifest"),
    )
    parser.add_argument("--components")
    parser.add_argument("--prefix", type=Path)
    parser.add_argument("--hub-version")
    parser.add_argument("--catalog", type=Path)
    parser.add_argument(
        "--mode", choices=("production", "local-candidate"), default="production"
    )
    parser.add_argument("--local-sources", type=Path)
    parser.add_argument("--source", action="append", default=[])
    parser.add_argument("--output", type=Path)
    parser.add_argument("--node-bin", type=Path)
    parser.add_argument("--ha-config", type=Path)
    parser.add_argument("--edge-target", choices=("local-linux",))
    parser.add_argument("--edge-go-binary", type=Path)
    parser.add_argument("--edge-tool-root", type=Path)
    parser.add_argument("--timeout-seconds", type=int, default=300)
    return parser


def _components(value: Optional[str]) -> tuple[str, ...]:
    if value is None:
        raise BootstrapError("--components is required")
    components = tuple(item.strip() for item in value.split(",") if item.strip())
    if not components or len(components) != len(set(components)):
        raise BootstrapError("--components must be a unique comma-separated set")
    unknown = set(components) - set(KNOWN_REPOSITORIES)
    if unknown:
        raise BootstrapError(f"unknown components: {sorted(unknown)}")
    return components


def _require_path(
    value: Optional[Path], option: str, *, existing: bool = False
) -> Path:
    if value is None:
        raise BootstrapError(f"{option} is required")
    if not value.is_absolute():
        raise BootstrapError(f"{option} must be an absolute path")
    if existing and not value.is_file():
        raise BootstrapError(f"{option} must name an existing file")
    return value


def _write_private_json(path: Path, payload: Any) -> None:
    if path.exists():
        raise BootstrapError("output already exists")
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        json.dump(payload, stream, indent=2, sort_keys=True)
        stream.write("\n")


def _manifest(arguments: argparse.Namespace) -> dict[str, Any]:
    components = _components(arguments.components)
    validate_component_set(components)
    output = _require_path(arguments.output, "--output")
    mappings = {}
    for raw in arguments.source:
        if "=" not in raw:
            raise BootstrapError("--source must use component=/absolute/path")
        name, path_value = raw.split("=", 1)
        if name in mappings:
            raise BootstrapError(f"duplicate --source for {name}")
        path = Path(path_value)
        if name not in components or not path.is_absolute() or not path.is_dir():
            raise BootstrapError(
                "--source must bind each selected component to a directory"
            )
        mappings[name] = path
    if set(mappings) != set(components):
        raise BootstrapError("--source must be supplied for every selected component")
    records = {}
    commits = {}
    for name in components:
        source = mappings[name]
        try:
            repository = (
                (
                    run_process(
                        ["git", "remote", "get-url", "origin"],
                        cwd=source,
                        timeout_seconds=15,
                    ).stdout
                    or b""
                )
                .decode()
                .strip()
            )
            commit = (
                (
                    run_process(
                        ["git", "rev-parse", "HEAD"],
                        cwd=source,
                        timeout_seconds=15,
                    ).stdout
                    or b""
                )
                .decode()
                .strip()
            )
        except ProcessFailure as error:
            raise BootstrapError(
                f"could not inspect Git identity for {name}"
            ) from error
        if repository != KNOWN_REPOSITORIES[name]:
            raise BootstrapError(
                f"local source repository is not allowlisted for {name}"
            )
        records[name] = source_manifest_for(name, source, repository, commit)
        commits[name] = commit
    _write_private_json(output, {"schema_version": 1, "components": records})
    return {
        "status": "manifest-created",
        "output": str(output),
        "components": list(components),
        "commits": commits,
    }


def _load_local_sources(path: Path, components: tuple[str, ...]) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BootstrapError("local source manifest could not be read") from error
    records = value.get("components") if isinstance(value, dict) else None
    if (
        not isinstance(value, dict)
        or set(value) != {"schema_version", "components"}
        or value["schema_version"] != 1
        or not isinstance(records, dict)
        or not set(records).issubset(KNOWN_REPOSITORIES)
        or not set(components).issubset(records)
    ):
        raise BootstrapError("local source manifest component set mismatch")
    return {name: records[name] for name in components}


def _context(arguments: argparse.Namespace) -> RecipeContext:
    if not 1 <= arguments.timeout_seconds <= 1800:
        raise BootstrapError("--timeout-seconds must be between 1 and 1800")
    if arguments.node_bin is not None and (
        not arguments.node_bin.is_absolute() or not arguments.node_bin.is_dir()
    ):
        raise BootstrapError("--node-bin must be an existing absolute directory")
    if (arguments.edge_go_binary is None) != (arguments.edge_tool_root is None):
        raise BootstrapError(
            "--edge-go-binary and --edge-tool-root must be supplied together"
        )
    for value, option in (
        (arguments.edge_go_binary, "--edge-go-binary"),
        (arguments.edge_tool_root, "--edge-tool-root"),
    ):
        if value is not None and not value.is_absolute():
            raise BootstrapError(f"{option} must be an absolute path")
    return RecipeContext(
        ha_config=arguments.ha_config,
        edge_target=arguments.edge_target,
        edge_go_binary=arguments.edge_go_binary,
        edge_tool_root=arguments.edge_tool_root,
        node_bin=arguments.node_bin,
        timeout_seconds=arguments.timeout_seconds,
    )


def _verify_materialized(
    cohort: Cohort,
    records: dict[str, Any],
    context: RecipeContext,
) -> dict[str, Any]:
    validate_selected_sources(cohort, records)
    tools = {}
    with tempfile.TemporaryDirectory(prefix="teslatlas-companion-verify-") as temporary:
        root = Path(temporary)
        for name, component in cohort.components.items():
            tools[name] = check_recipe_environment(name, context)
            destination = root / name
            verify_local_source(records[name], destination)
            validate_source_metadata(
                name, destination, component, cohort.publication_status
            )
    return tools


def _download_sources(
    cohort: Cohort, root: Path, timeout_seconds: int
) -> dict[str, Any]:
    records = {}
    for name, component in cohort.components.items():
        destination = root / name
        records[name] = fetch_git_source(
            component.repository,
            component.commit,
            destination,
            component.source_sha256,
            timeout_seconds=min(timeout_seconds, 300),
        )
    return records


def _operate(arguments: argparse.Namespace) -> dict[str, Any]:
    if arguments.action == "manifest":
        return _manifest(arguments)
    if os.geteuid() == 0:
        raise BootstrapError("companion bootstrap mutations must not run as root")
    prefix = _prefix_path(_require_path(arguments.prefix, "--prefix"))
    if arguments.action == "status":
        return status(prefix)
    if arguments.action == "rollback":
        return rollback(prefix)
    components = _components(arguments.components)
    validate_component_set(components)
    if arguments.hub_version is None:
        raise BootstrapError("--hub-version is required")
    context = _context(arguments)
    local_mode = arguments.mode == "local-candidate"
    if not local_mode and arguments.local_sources is not None:
        raise BootstrapError("production mode does not accept --local-sources")
    if local_mode and arguments.local_sources is None:
        raise BootstrapError("local candidate mode requires --local-sources")
    validate_targets(prefix, components, context)
    shipped_catalog = _require_path(arguments.catalog, "--catalog", existing=True)
    if arguments.action == "update" and not local_mode:
        catalog_path = _refresh_catalog(prefix)
    elif local_mode:
        catalog_path = shipped_catalog
    else:
        catalog_path = _catalog_selection(prefix, shipped_catalog)
    cohort = select_cohort(
        load_catalog(catalog_path),
        components,
        arguments.hub_version,
        allow_candidates=local_mode,
        update=arguments.action == "update",
    )
    installation_mode = "local-unpublished" if local_mode else "production-git"
    targets = validate_targets(prefix, cohort.components, context)
    if arguments.action == "dry-run":
        with tempfile.TemporaryDirectory(
            prefix="teslatlas-companion-download-"
        ) as temporary:
            if local_mode:
                local_path = _require_path(
                    arguments.local_sources, "--local-sources", existing=True
                )
                records = _load_local_sources(local_path, components)
            else:
                records = _download_sources(
                    cohort, Path(temporary), context.timeout_seconds
                )
            tools = _verify_materialized(cohort, records, context)
        return {
            "status": "dry-run",
            "installation_mode": installation_mode,
            "product_version": cohort.product_version,
            "hub_version": arguments.hub_version,
            "components": list(cohort.components),
            "tools": tools,
            "would_activate": True,
        }

    def provide_sources(temporary: Path) -> dict[str, Any]:
        if local_mode:
            local_path = _require_path(
                arguments.local_sources, "--local-sources", existing=True
            )
            return _load_local_sources(local_path, components)
        return _download_sources(cohort, temporary, context.timeout_seconds)

    def check_environment() -> None:
        for name in cohort.components:
            check_recipe_environment(name, context)

    sdk_output: Optional[Path] = None

    def build(name: str, source: Path, output: Path) -> dict[str, Any]:
        nonlocal sdk_output
        if name == "viewer":
            if sdk_output is None:
                raise BootstrapError(
                    "Viewer SDK dependency was not built in this operation"
                )
            validate_source_metadata(
                name,
                source,
                cohort.components[name],
                cohort.publication_status,
            )
            sdk_binding = bind_viewer_sdk_artifact(
                source,
                sdk_output,
                cohort.components[name],
                cohort.components["sdk-typescript"],
            )
        else:
            sdk_binding = None
        built = build_component(
            name,
            source,
            output,
            cohort.components[name],
            context,
            cohort.publication_status,
        )
        if sdk_binding is not None:
            built["verification"]["cohort_sdk"] = sdk_binding
        if name == "sdk-typescript":
            sdk_output = output
        return built

    return install_cohort_from_provider(
        prefix,
        cohort,
        provide_sources,
        build,
        installation_mode=installation_mode,
        target_binding=targets,
        validate_component=lambda name, source: validate_source_metadata(
            name,
            source,
            cohort.components[name],
            cohort.publication_status,
        ),
        check_environment=check_environment,
        hub_version=arguments.hub_version,
    )


def main(argv: Optional[list[str]] = None) -> int:
    """Run one operation and emit exactly one JSON result."""
    try:
        arguments = _parser().parse_args(argv)
        result = _operate(arguments)
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    except BootstrapError as error:
        print(
            json.dumps(
                {"status": "error", "error": str(error)},
                sort_keys=True,
                separators=(",", ":"),
            )
        )
        return 2
    except OSError as error:
        detail = error.strerror or error.__class__.__name__
        print(
            json.dumps(
                {"status": "error", "error": f"filesystem operation failed: {detail}"},
                sort_keys=True,
                separators=(",", ":"),
            )
        )
        return 2
    except KeyboardInterrupt:
        print('{"error":"operation interrupted","status":"error"}')
        return 130


def install_signal_handlers() -> None:
    def interrupted(_signal: int, _frame: Any) -> None:
        raise KeyboardInterrupt

    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGTERM, interrupted)
