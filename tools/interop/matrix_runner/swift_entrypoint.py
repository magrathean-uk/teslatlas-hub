# SPDX-License-Identifier: AGPL-3.0-only
"""Fixed, hash-bound wrapper for the reviewed Swift installed coordinator."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import importlib.util
from pathlib import Path
import stat
import sys
import threading
from types import MappingProxyType
from typing import Any, Callable, Mapping


class SwiftEntrypointError(RuntimeError):
    pass


class SwiftEntrypointPending(SwiftEntrypointError):
    pass


@dataclass(frozen=True)
class ReviewedSwiftImport:
    name: str
    path: str
    sha256: str


@dataclass(frozen=True)
class ReviewedSwiftAdapter:
    path: str
    sha256: str
    imports: tuple[ReviewedSwiftImport, ...]


_IMPORT_LOCK = threading.Lock()
_REQUIRED_IMPORTS = frozenset({"matrix_contract", "matrix_wire"})


class SwiftLauncherCapability:
    """Only the two methods published by the reviewed Swift coordinator.

    Worker callables are installed by runner source from its fixed launch
    registry.  Session, adapter, or worker JSON cannot add or replace them.
    """

    __slots__ = ("_workers", "_observations")

    def __init__(
        self,
        workers: Mapping[str, Callable[[Mapping[str, str], Callable[[Mapping[str, Any]], Mapping[str, Any]]], Mapping[str, Any]]],
        observations: Mapping[int, Mapping[str, Any]],
    ):
        if set(workers) - {"swift_macos", "swift_linux"} or not all(callable(item) for item in workers.values()):
            raise SwiftEntrypointError("Swift worker registry is invalid")
        if any(type(key) is not int or key <= 0 or not isinstance(value, dict) for key, value in observations.items()):
            raise SwiftEntrypointError("controller observation registry is invalid")
        self._workers = MappingProxyType(dict(workers))
        self._observations = MappingProxyType({key: MappingProxyType(dict(value)) for key, value in observations.items()})

    def run_worker(self, actor_id, worker_config_binding, phase_callback):
        worker = self._workers.get(actor_id)
        if worker is None:
            raise SwiftEntrypointError("Swift worker is not registered")
        if not isinstance(worker_config_binding, dict) or set(worker_config_binding) != {"path", "sha256"}:
            raise SwiftEntrypointError("Swift worker config binding is invalid")
        if not callable(phase_callback):
            raise SwiftEntrypointError("Swift phase callback is invalid")
        return worker(worker_config_binding, phase_callback)

    def controller_observations(self, session_id):
        if not isinstance(session_id, str) or not session_id:
            raise SwiftEntrypointError("Swift observation session is invalid")
        return self._observations


def _read_reviewed(path_value: str, digest: str, label: str) -> tuple[Path, bytes]:
    path = Path(path_value)
    try:
        metadata = path.lstat()
        raw = path.read_bytes()
    except OSError as error:
        raise SwiftEntrypointError(f"reviewed Swift {label} cannot be read") from error
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1 or stat.S_IMODE(metadata.st_mode) & 0o077:
        raise SwiftEntrypointError(f"reviewed Swift {label} is not an owner-only regular file")
    if hashlib.sha256(raw).hexdigest() != digest:
        raise SwiftEntrypointError(f"reviewed Swift {label} digest changed")
    return path, raw


def _module(name: str, path: Path):
    specification = importlib.util.spec_from_file_location(name, path)
    if specification is None or specification.loader is None:
        raise SwiftEntrypointError("reviewed Swift module cannot be loaded")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


def _load_and_run(reviewed: ReviewedSwiftAdapter, session: Path,
                  launcher: SwiftLauncherCapability) -> Any:
    if not isinstance(reviewed.imports, tuple) or {item.name for item in reviewed.imports} != _REQUIRED_IMPORTS or len(reviewed.imports) != 2:
        raise SwiftEntrypointError("reviewed Swift import registry is invalid")
    adapter_path, adapter_raw = _read_reviewed(reviewed.path, reviewed.sha256, "adapter")
    dependencies = []
    for item in reviewed.imports:
        if not isinstance(item, ReviewedSwiftImport):
            raise SwiftEntrypointError("reviewed Swift import registry is invalid")
        path, raw = _read_reviewed(item.path, item.sha256, item.name)
        dependencies.append((item, path, raw))
    prior = {}
    with _IMPORT_LOCK:
        try:
            for item, path, _raw in dependencies:
                prior[item.name] = sys.modules.get(item.name)
                sys.modules[item.name] = _module(item.name, path)
            adapter = _module("_teslatlas_reviewed_swift_matrix", adapter_path)
            if not callable(getattr(adapter, "run_installed", None)):
                raise SwiftEntrypointError("reviewed Swift adapter interface is incomplete")
            result = adapter.run_installed(str(session), launcher)
            if hashlib.sha256(adapter_path.read_bytes()).digest() != hashlib.sha256(adapter_raw).digest():
                raise SwiftEntrypointError("reviewed Swift adapter changed during execution")
            for item, path, raw in dependencies:
                if hashlib.sha256(path.read_bytes()).digest() != hashlib.sha256(raw).digest():
                    raise SwiftEntrypointError(f"reviewed Swift {item.name} changed during execution")
            return result
        except SwiftEntrypointError:
            raise
        except Exception as error:
            raise SwiftEntrypointError("reviewed Swift adapter import or execution failed") from error
        finally:
            sys.modules.pop("_teslatlas_reviewed_swift_matrix", None)
            for item, _path, _raw in dependencies:
                old = prior.get(item.name)
                if old is None:
                    sys.modules.pop(item.name, None)
                else:
                    sys.modules[item.name] = old


def run(session_input_path: Path | str, launcher: SwiftLauncherCapability,
        reviewed: ReviewedSwiftAdapter | None) -> int:
    if reviewed is None:
        raise SwiftEntrypointPending("pending: reviewed Swift adapter is unavailable")
    if not isinstance(launcher, SwiftLauncherCapability):
        raise SwiftEntrypointError("Swift launcher capability is invalid")
    session = Path(session_input_path)
    if not session.is_absolute() or not session.is_file():
        raise SwiftEntrypointError("Swift session input is invalid")
    result = _load_and_run(reviewed, session, launcher)
    if type(result) is not int or result != 0:
        raise SwiftEntrypointError("reviewed Swift adapter failed")
    return result
