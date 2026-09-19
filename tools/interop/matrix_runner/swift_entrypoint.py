# SPDX-License-Identifier: AGPL-3.0-only
"""Fixed, hash-bound wrapper for the reviewed Swift installed coordinator."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
from pathlib import Path
import sys
import threading
from types import MappingProxyType
from typing import Any, Callable, Mapping

from .adapter_wire import (WireError, deep_freeze, execute_reviewed_module,
                           read_bound_file, read_safe_file, strict_json)


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

    __slots__ = ("_workers", "_observations", "_session_id", "_deadline", "_issued", "_launched")

    def __init__(
        self,
        workers: Mapping[str, Callable[[Mapping[str, str], Callable[[Mapping[str, Any]], Mapping[str, Any]]], Mapping[str, Any]]],
        observations: Callable[[], Mapping[int, Mapping[str, Any]]],
        *, session_id: str | None = None, deadline=None,
    ):
        if set(workers) != {"swift_macos", "swift_linux"} or not all(callable(item) for item in workers.values()):
            raise SwiftEntrypointError("Swift worker registry is invalid")
        if not callable(observations) or not isinstance(session_id, str) or not session_id or not callable(getattr(deadline, "remaining", None)):
            raise SwiftEntrypointError("controller observation registry is invalid")
        self._workers = MappingProxyType(dict(workers))
        self._observations = observations
        self._session_id = session_id
        self._deadline = deadline
        self._issued = {}
        self._launched = set()

    def remaining_cell_ms(self, actor_id):
        if actor_id not in self._workers or actor_id in self._launched:
            raise SwiftEntrypointError("Swift worker deadline actor is invalid")
        try:
            remaining = int(self._deadline.remaining() * 1000)
        except TimeoutError as error:
            raise SwiftEntrypointError("Swift cell deadline expired") from error
        if remaining <= 0:
            raise SwiftEntrypointError("Swift cell deadline expired")
        self._issued[actor_id] = remaining
        return remaining

    def run_worker(self, actor_id, worker_config_binding, phase_callback):
        worker = self._workers.get(actor_id)
        if worker is None:
            raise SwiftEntrypointError("Swift worker is not registered")
        if not isinstance(worker_config_binding, dict) or set(worker_config_binding) != {"path", "sha256"}:
            raise SwiftEntrypointError("Swift worker config binding is invalid")
        if not callable(phase_callback):
            raise SwiftEntrypointError("Swift phase callback is invalid")
        try:
            config = strict_json(read_bound_file(worker_config_binding, label="Swift worker config",
                                maximum=1_048_576, deadline=self._deadline))
            self._deadline.remaining()
        except (WireError, TimeoutError) as error:
            raise SwiftEntrypointError("Swift worker config cannot be admitted") from error
        if (not isinstance(config, dict) or config.get("session_id") != self._session_id
                or config.get("actor_id") != actor_id or actor_id in self._launched
                or type(config.get("remaining_cell_ms")) is not int
                or config["remaining_cell_ms"] != self._issued.get(actor_id)):
            raise SwiftEntrypointError("Swift worker config lacks the issued remaining cell budget")
        self._launched.add(actor_id)
        return worker(worker_config_binding, phase_callback)

    def controller_observations(self, session_id):
        if session_id != self._session_id:
            raise SwiftEntrypointError("Swift observation session is invalid")
        try:
            self._deadline.remaining()
            observations = self._observations()
            self._deadline.remaining()
        except Exception as error:
            raise SwiftEntrypointError("Swift controller observations unavailable") from error
        if (not isinstance(observations, Mapping) or not observations
                or any(type(key) is not int or key <= 0 or not isinstance(value, Mapping)
                       or value.get("session_id") != self._session_id
                       for key, value in observations.items())):
            raise SwiftEntrypointError("Swift controller observations have a foreign identity")
        return deep_freeze(observations)


def _read_reviewed(path_value: str, digest: str, label: str) -> tuple[Path, bytes]:
    path = Path(path_value)
    try:
        raw = read_bound_file({"path": path_value, "sha256": digest},
                              label="reviewed Swift " + label, maximum=1_048_576)
    except (OSError, WireError) as error:
        raise SwiftEntrypointError(f"reviewed Swift {label} cannot be read") from error
    return path, raw


def _module(name: str, path: Path, raw: bytes):
    return execute_reviewed_module(name, path, raw)


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
            prior["_teslatlas_reviewed_swift_matrix"] = sys.modules.get("_teslatlas_reviewed_swift_matrix")
            for item, path, raw in dependencies:
                prior[item.name] = sys.modules.get(item.name)
                sys.modules[item.name] = _module(item.name, path, raw)
            adapter = _module("_teslatlas_reviewed_swift_matrix", adapter_path, adapter_raw)
            sys.modules[adapter.__name__] = adapter
            if not callable(getattr(adapter, "run_installed", None)):
                raise SwiftEntrypointError("reviewed Swift adapter interface is incomplete")
            result = adapter.run_installed(str(session), launcher)
            if _read_reviewed(str(adapter_path), reviewed.sha256, "adapter")[1] != adapter_raw:
                raise SwiftEntrypointError("reviewed Swift adapter changed during execution")
            for item, path, raw in dependencies:
                if _read_reviewed(str(path), item.sha256, item.name)[1] != raw:
                    raise SwiftEntrypointError(f"reviewed Swift {item.name} changed during execution")
            return result
        except SwiftEntrypointError:
            raise
        except Exception as error:
            raise SwiftEntrypointError("reviewed Swift adapter import or execution failed") from error
        finally:
            for name, old in prior.items():
                if old is None:
                    sys.modules.pop(name, None)
                else:
                    sys.modules[name] = old


def run(session_input_path: Path | str, launcher: SwiftLauncherCapability,
        reviewed: ReviewedSwiftAdapter | None) -> int:
    if reviewed is None:
        raise SwiftEntrypointPending("pending: reviewed Swift adapter is unavailable")
    if not isinstance(launcher, SwiftLauncherCapability):
        raise SwiftEntrypointError("Swift launcher capability is invalid")
    session = Path(session_input_path)
    try:
        read_safe_file(session, label="Swift session input", maximum=1_048_576)
    except WireError as error:
        raise SwiftEntrypointError("Swift session input is invalid") from error
    result = _load_and_run(reviewed, session, launcher)
    if type(result) is not int or result != 0:
        raise SwiftEntrypointError("reviewed Swift adapter failed")
    return result
