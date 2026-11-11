"""Bounded opt-in tooling commands. OS lifecycle acceptance is a separate gate."""

from __future__ import annotations

import os
import selectors
import signal
import subprocess
import time

INSPECTION_SECONDS = 60.0
BUILD_SECONDS = 900.0
CLEANUP_SECONDS = 15.0
SHUTDOWN_SECONDS = 5.0
MAX_OUTPUT_BYTES = 8 * 1024 * 1024


class CommandError(RuntimeError):
    pass


def _signal_group(process, action):
    try:
        os.killpg(process.pid, action)
    except ProcessLookupError:
        pass


def run(command, *, cwd=None, timeout=INSPECTION_SECONDS, text=False,
        maximum_output=MAX_OUTPUT_BYTES):
    """Capture bounded output and bound execution, pipe drainage and shutdown.

    Children start in a private session. Failure sends TERM and then KILL to
    that group. Neither this helper nor process exit proves Docker settlement.
    """
    if timeout <= 0 or maximum_output <= 0:
        raise ValueError("command budgets must be positive")
    if signal.getsignal(signal.SIGCHLD) != signal.SIG_DFL:
        raise CommandError("owned command requires default SIGCHLD ownership")
    process = subprocess.Popen(command, cwd=cwd, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, start_new_session=True)
    output = [bytearray(), bytearray()]
    deadline = time.monotonic() + timeout
    selector = selectors.DefaultSelector()
    try:
        for index, pipe in enumerate((process.stdout, process.stderr)):
            os.set_blocking(pipe.fileno(), False)
            selector.register(pipe, selectors.EVENT_READ, index)
        # Do not poll/reap the leader while descendants may retain pipes. Its
        # unreaped PID reserves the group identity through failure escalation.
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise CommandError(f"command exceeded {timeout:g}s execution/pipe budget: {command[0]}")
            for key, _ in selector.select(min(remaining, 0.1)):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                if sum(map(len, output)) + len(chunk) > maximum_output:
                    raise CommandError(f"command output exceeds {maximum_output} bytes: {command[0]}")
                output[key.data].extend(chunk)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise CommandError(f"command exceeded {timeout:g}s execution budget: {command[0]}")
        status = process.wait(timeout=remaining)
    except BaseException as error:
        try:
            _signal_group(process, signal.SIGTERM)
            time.sleep(SHUTDOWN_SECONDS)
            # Also release inherited pipes held by descendants after child exit.
            _signal_group(process, signal.SIGKILL)
            process.wait(timeout=SHUTDOWN_SECONDS)
        except BaseException as cleanup_error:
            error.add_note(f"owned command shutdown failed: {cleanup_error}")
        raise
    finally:
        selector.close()
        process.stdout.close()
        process.stderr.close()
    stdout, stderr = (bytes(part) for part in output)
    if text:
        stdout = stdout.decode("utf-8", errors="replace")
        stderr = stderr.decode("utf-8", errors="replace")
    return subprocess.CompletedProcess(command, status, stdout, stderr)
