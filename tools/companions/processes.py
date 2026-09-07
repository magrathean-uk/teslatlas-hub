# SPDX-License-Identifier: AGPL-3.0-only
"""Bounded subprocess execution for bootstrap-owned process groups."""

from __future__ import annotations

import os
import resource
import signal
import subprocess
import time
from pathlib import Path
from typing import Any, Mapping, Optional


class ProcessFailure(RuntimeError):
    """A command failed or left descendants outside its deadline."""


def _group_exists(process_group: int) -> bool:
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _terminate_group(process: subprocess.Popen[Any], grace_seconds: float) -> None:
    """Terminate every member, even when the direct child already exited."""
    process_group = process.pid
    if _group_exists(process_group):
        try:
            os.killpg(process_group, signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + grace_seconds
    while _group_exists(process_group) and time.monotonic() < deadline:
        process.poll()
        time.sleep(0.02)
    if _group_exists(process_group):
        try:
            os.killpg(process_group, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
    deadline = time.monotonic() + grace_seconds
    while _group_exists(process_group) and time.monotonic() < deadline:
        process.poll()
        time.sleep(0.02)
    try:
        process.wait(timeout=grace_seconds)
    except subprocess.TimeoutExpired:
        pass
    if _group_exists(process_group):
        raise ProcessFailure("owned process group could not be drained")


def _close_pipes(process: subprocess.Popen[Any]) -> None:
    for stream in (process.stdout, process.stderr):
        if stream is not None and hasattr(stream, "close"):
            stream.close()


def run_process(
    command: list[str],
    *,
    cwd: Path,
    env: Optional[Mapping[str, str]] = None,
    timeout_seconds: float,
    stdout: Any = subprocess.PIPE,
    stderr: Any = subprocess.PIPE,
    grace_seconds: float = 2.0,
) -> subprocess.CompletedProcess[bytes]:
    """Run one command in a new session and drain its complete process group."""

    def disable_core_dumps() -> None:
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))

    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=None if env is None else dict(env),
            stdin=subprocess.DEVNULL,
            stdout=stdout,
            stderr=stderr,
            start_new_session=True,
            preexec_fn=disable_core_dumps,
        )
    except OSError as error:
        raise ProcessFailure(f"could not start command: {command[0]}") from error
    try:
        if stdout == subprocess.PIPE or stderr == subprocess.PIPE:
            output, errors = process.communicate(timeout=timeout_seconds)
            return_code = process.returncode
        else:
            return_code = process.wait(timeout=timeout_seconds)
            output = errors = None
    except BaseException as error:
        try:
            _terminate_group(process, grace_seconds)
        except ProcessFailure as cleanup_error:
            _close_pipes(process)
            raise cleanup_error from error
        _close_pipes(process)
        if isinstance(error, subprocess.TimeoutExpired):
            raise ProcessFailure(f"command timed out: {command[0]}") from error
        raise
    if _group_exists(process.pid):
        _terminate_group(process, grace_seconds)
        _close_pipes(process)
        raise ProcessFailure(f"command left descendants running: {command[0]}")
    completed = subprocess.CompletedProcess(command, return_code, output, errors)
    if return_code != 0:
        raise ProcessFailure(f"command failed: {command[0]}")
    return completed
