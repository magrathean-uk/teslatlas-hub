# SPDX-License-Identifier: AGPL-3.0-only
"""Bounded byte bridge for a runner-selected Docker stdio coordinator.

The caller supplies only a command from the fixed launch registry.  This
module does not interpret job JSON or construct Docker arguments.
"""

from __future__ import annotations

from dataclasses import dataclass
import errno
import json
import os
from pathlib import Path
import select
import signal
import socket
import stat
import subprocess
import time
from typing import Mapping, Sequence
import threading

from .adapter_wire import MAX_FRAME_BYTES, strict_json
from .process_generation import OwnedProcess
try:
    from ..installed_hosts.bounded import Deadline
except ImportError:  # pragma: no cover - top-level matrix_runner entrypoint
    from installed_hosts.bounded import Deadline


class DockerPipeError(RuntimeError):
    pass


MAX_STDERR_BYTES = 8_388_608


@dataclass(frozen=True)
class DockerPipeOutcome:
    exit_code: int
    frames_to_client: int
    frames_to_broker: int


def _frame(fd: int, buffer: bytearray, deadline: float, label: str,
           process: subprocess.Popen | None = None) -> bytes | None:
    while True:
        newline = buffer.find(b"\n")
        if newline >= 0:
            if newline > MAX_FRAME_BYTES:
                raise DockerPipeError(label + " frame exceeds bound")
            raw = bytes(buffer[:newline + 1])
            del buffer[:newline + 1]
            try:
                value = strict_json(raw)
            except Exception as error:
                raise DockerPipeError(label + " frame is invalid") from error
            if not isinstance(value, dict):
                raise DockerPipeError(label + " frame is invalid")
            return raw
        if len(buffer) > MAX_FRAME_BYTES:
            raise DockerPipeError(label + " frame exceeds bound")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise DockerPipeError("Docker pipe deadline expired")
        ready, _, _ = select.select([fd], [], [], min(remaining, 0.1 if process is not None else remaining))
        if not ready:
            if process is not None and process.poll() is not None:
                try:
                    chunk = os.read(fd, min(65_536, MAX_FRAME_BYTES + 1 - len(buffer)))
                except BlockingIOError:
                    continue
                if not chunk:
                    if buffer:
                        raise DockerPipeError(label + " closed with a partial frame")
                    return None
                buffer.extend(chunk)
                continue
            if process is not None:
                continue
            raise DockerPipeError("Docker pipe deadline expired")
        try:
            chunk = os.read(fd, min(65_536, MAX_FRAME_BYTES + 1 - len(buffer)))
        except BlockingIOError:
            continue
        if not chunk:
            if buffer:
                raise DockerPipeError(label + " closed with a partial frame")
            return None
        buffer.extend(chunk)


def _terminate(process: subprocess.Popen, deadline=None) -> None:
    """Bounded fallback for a launcher whose generation could not be read."""
    limit = deadline if isinstance(deadline, Deadline) else Deadline(45)
    try:
        owner = OwnedProcess(process, limit)
        owner.cleanup(limit)
        return
    except BaseException:
        # Keep the bounded leader fallback when the platform observer cannot
        # enumerate a generation; never use an unscoped kill command.
        pass
    if process.poll() is not None:
        process.wait(timeout=max(0.001, limit.remaining()))
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        process.terminate()
    try:
        process.wait(timeout=min(0.75, max(0.001, limit.remaining())))
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            process.kill()
        process.wait(timeout=max(0.001, limit.remaining()))


def _connect(broker: socket.socket, path: str, deadline: float) -> None:
    broker.setblocking(False)
    result = broker.connect_ex(path)
    if result in (0, errno.EISCONN):
        return
    if result not in (errno.EINPROGRESS, errno.EALREADY, errno.EWOULDBLOCK):
        raise DockerPipeError("Docker broker connection failed")
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise DockerPipeError("Docker pipe deadline expired")
        _readable, writable, exceptional = select.select([], [broker], [broker], min(remaining, 0.1))
        if exceptional:
            raise DockerPipeError("Docker broker connection failed")
        if writable:
            error = broker.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR)
            if error:
                raise DockerPipeError("Docker broker connection failed")
            return


def _write_all(fd: int, raw: bytes, deadline: float, label: str) -> None:
    offset = 0
    while offset < len(raw):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise DockerPipeError("Docker pipe deadline expired")
        _readable, writable, _exceptional = select.select([], [fd], [], min(remaining, 0.1))
        if not writable:
            continue
        try:
            count = os.write(fd, raw[offset:])
        except BlockingIOError:
            continue
        except OSError as error:
            raise DockerPipeError(label + " write failed") from error
        if count <= 0:
            raise DockerPipeError(label + " write closed")
        offset += count


def run(*, socket_path: Path | str, argv: Sequence[str], cwd: Path | str,
        environment: Mapping[str, str], stderr_path: Path | str,
        timeout_seconds: int) -> DockerPipeOutcome:
    """Relay strict NDJSON frames without inspecting or repairing them."""
    socket_path, cwd, stderr_path = Path(socket_path), Path(cwd), Path(stderr_path)
    if (
        not socket_path.is_absolute() or not cwd.is_absolute()
        or not stderr_path.is_absolute() or not cwd.is_dir()
        or not isinstance(argv, tuple) or not argv or not Path(argv[0]).is_absolute()
        or not all(isinstance(item, str) and item for item in argv)
        or type(timeout_seconds) is not int or not 1 <= timeout_seconds <= 3600
        or not isinstance(environment, dict)
        or not all(isinstance(key, str) and key and isinstance(value, str) for key, value in environment.items())
    ):
        raise DockerPipeError("Docker pipe launch contract is invalid")
    try:
        socket_metadata = socket_path.lstat()
        parent_metadata = stderr_path.parent.stat()
    except OSError as error:
        raise DockerPipeError("Docker pipe boundary is missing") from error
    if not stat.S_ISSOCK(socket_metadata.st_mode) or socket_metadata.st_uid != os.getuid() or socket_metadata.st_mode & 0o077:
        raise DockerPipeError("broker socket ownership is invalid")
    if parent_metadata.st_uid != os.getuid() or parent_metadata.st_mode & 0o077 or os.path.lexists(stderr_path):
        raise DockerPipeError("Docker stderr boundary is invalid")
    stderr_fd = os.open(stderr_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    broker = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    process = None
    owner = None
    completed = False
    stderr_thread = None
    stderr_pipe = None
    stderr_error = []
    deadline = time.monotonic() + timeout_seconds
    to_client = to_broker = 0
    try:
        _connect(broker, str(socket_path), deadline)
        process = subprocess.Popen(
            tuple(argv), cwd=str(cwd), env=dict(environment), stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
        )
        if process.stdin is None or process.stdout is None or process.stderr is None:
            raise DockerPipeError("Docker pipe descriptors are unavailable")
        # Acquire generation ownership immediately.  If the direct leader
        # exits while a descendant still holds a pipe, cleanup must retain the
        # descendant identity rather than falling back to a recycled PID.
        owner = OwnedProcess(process, Deadline(max(0.001, deadline - time.monotonic())))
        os.set_blocking(process.stdin.fileno(), False)
        os.set_blocking(process.stdout.fileno(), False)
        stderr_pipe = process.stderr

        def drain_stderr():
            total = 0
            try:
                while True:
                    chunk = stderr_pipe.read(65_536)
                    if not chunk:
                        return
                    if total < MAX_STDERR_BYTES:
                        accepted = chunk[:MAX_STDERR_BYTES - total]
                        if accepted:
                            os.write(stderr_fd, accepted)
                    total += len(chunk)
                    if total > MAX_STDERR_BYTES:
                        stderr_error.append("Docker stderr exceeds bound")
            except (OSError, ValueError):
                return

        stderr_thread = threading.Thread(target=drain_stderr, name="docker-pipe-stderr", daemon=True)
        stderr_thread.start()
        broker_buffer, client_buffer = bytearray(), bytearray()
        greeting = _frame(broker.fileno(), broker_buffer, deadline, "broker")
        if greeting is None:
            raise DockerPipeError("broker closed before greeting")
        _write_all(process.stdin.fileno(), greeting, deadline, "Docker client stdin")
        to_client += 1
        while True:
            request = _frame(process.stdout.fileno(), client_buffer, deadline, "client stdout", process)
            if request is None:
                code = process.wait(timeout=max(0.001, deadline - time.monotonic()))
                if code != 0 or to_broker == 0:
                    raise DockerPipeError("client stdout closed prematurely")
                owner.require_absent(Deadline(max(0.001, deadline - time.monotonic())))
                closed = _frame(broker.fileno(), broker_buffer, deadline, "broker")
                if closed is None:
                    if stderr_thread is not None:
                        stderr_thread.join(timeout=1)
                    if stderr_error:
                        raise DockerPipeError(stderr_error[0])
                    completed = True
                    return DockerPipeOutcome(code, to_client, to_broker)
                raise DockerPipeError("client stdout closed prematurely")
            _write_all(broker.fileno(), request, deadline, "Docker broker")
            to_broker += 1
            reply = _frame(broker.fileno(), broker_buffer, deadline, "broker")
            if reply is None:
                raise DockerPipeError("broker closed before reply")
            _write_all(process.stdin.fileno(), reply, deadline, "Docker client stdin")
            to_client += 1
            if stderr_error:
                raise DockerPipeError(stderr_error[0])
    except DockerPipeError:
        raise
    except (OSError, subprocess.SubprocessError, TimeoutError) as error:
        raise DockerPipeError("Docker pipe transport failed") from error
    finally:
        if process is not None and not completed:
            if owner is not None:
                try:
                    owner.cleanup(Deadline(45))
                except BaseException:
                    _terminate(process, Deadline(45))
            else:
                _terminate(process, Deadline(45))
        if process is not None:
            if stderr_pipe is not None:
                stderr_pipe.close()
            if stderr_thread is not None:
                stderr_thread.join(timeout=1)
            for stream in (process.stdin, process.stdout):
                if stream is not None:
                    stream.close()
        broker.close()
        os.close(stderr_fd)
