# SPDX-License-Identifier: AGPL-3.0-only
"""Bounded byte bridge for a runner-selected Docker stdio coordinator.

The caller supplies only a command from the fixed launch registry.  This
module does not interpret job JSON or construct Docker arguments.
"""

from __future__ import annotations

from dataclasses import dataclass
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

from .adapter_wire import MAX_FRAME_BYTES, strict_json


class DockerPipeError(RuntimeError):
    pass


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
                chunk = os.read(fd, min(65_536, MAX_FRAME_BYTES + 1 - len(buffer)))
                if not chunk:
                    if buffer:
                        raise DockerPipeError(label + " closed with a partial frame")
                    return None
                buffer.extend(chunk)
                continue
            if process is not None:
                continue
            raise DockerPipeError("Docker pipe deadline expired")
        chunk = os.read(fd, min(65_536, MAX_FRAME_BYTES + 1 - len(buffer)))
        if not chunk:
            if buffer:
                raise DockerPipeError(label + " closed with a partial frame")
            return None
        buffer.extend(chunk)


def _terminate(process: subprocess.Popen) -> None:
    if process.poll() is not None:
        process.wait()
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        process.terminate()
    try:
        process.wait(timeout=0.75)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            process.kill()
        process.wait(timeout=1)


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
    deadline = time.monotonic() + timeout_seconds
    to_client = to_broker = 0
    try:
        broker.connect(str(socket_path))
        process = subprocess.Popen(
            tuple(argv), cwd=str(cwd), env=dict(environment), stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=stderr_fd, start_new_session=True,
        )
        if process.stdin is None or process.stdout is None:
            raise DockerPipeError("Docker pipe descriptors are unavailable")
        broker_buffer, client_buffer = bytearray(), bytearray()
        greeting = _frame(broker.fileno(), broker_buffer, deadline, "broker")
        if greeting is None:
            raise DockerPipeError("broker closed before greeting")
        process.stdin.write(greeting)
        process.stdin.flush()
        to_client += 1
        while True:
            request = _frame(process.stdout.fileno(), client_buffer, deadline, "client stdout", process)
            if request is None:
                code = process.wait(timeout=max(0.001, deadline - time.monotonic()))
                if code != 0 or to_broker == 0:
                    raise DockerPipeError("client stdout closed prematurely")
                closed = _frame(broker.fileno(), broker_buffer, deadline, "broker")
                if closed is None:
                    return DockerPipeOutcome(code, to_client, to_broker)
                raise DockerPipeError("client stdout closed prematurely")
            broker.sendall(request)
            to_broker += 1
            reply = _frame(broker.fileno(), broker_buffer, deadline, "broker")
            if reply is None:
                raise DockerPipeError("broker closed before reply")
            process.stdin.write(reply)
            process.stdin.flush()
            to_client += 1
    except DockerPipeError:
        raise
    except (OSError, subprocess.SubprocessError, TimeoutError) as error:
        raise DockerPipeError("Docker pipe transport failed") from error
    finally:
        if process is not None and process.poll() is None:
            _terminate(process)
        if process is not None:
            for stream in (process.stdin, process.stdout):
                if stream is not None:
                    stream.close()
        broker.close()
        os.close(stderr_fd)
