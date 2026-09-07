# SPDX-License-Identifier: AGPL-3.0-only
"""Monotonic, bounded I/O primitives for controller and SSH exchanges."""
import os
from pathlib import Path
import selectors
import subprocess
import time


class Deadline:
    def __init__(self, seconds, clock=time.monotonic):
        if isinstance(seconds, bool) or not isinstance(seconds, (int, float)) or seconds <= 0:
            raise ValueError("deadline seconds must be positive")
        self._clock = clock
        self.end = clock() + float(seconds)

    def remaining(self, cap=None):
        value = self.end - self._clock()
        if value <= 0:
            raise TimeoutError("monotonic operation deadline expired")
        return min(value, cap) if cap is not None else value


def durable_bytes(path, raw):
    """Atomic private evidence handoff, including the directory entry."""
    target = Path(path)
    temporary = target.with_name(target.name + ".tmp")
    fd = os.open(str(temporary), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        with os.fdopen(fd, "wb") as handle:
            os.fchmod(handle.fileno(), 0o600)
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(str(temporary), str(target))
        directory = os.open(str(target.parent), os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except BaseException:
        # No completion flag is advanced when the durable handoff fails.
        raise


def cleanup_error(resource, error):
    """Bounded stage details, retained independently of row success."""
    value = {"resource": resource, "error": type(error).__name__, "message": str(error)[:2048]}
    nested = getattr(error, "cleanup_errors", None)
    if nested:
        value["stages"] = [item if isinstance(item, dict) else cleanup_error("transport-stage", item) for item in nested[:32]]
    return value


class IncrementalLineReader:
    """A strict UTF-8/JSON caller can consume one bounded NDJSON frame at a time."""

    def __init__(self, fd, maximum):
        self.fd = fd
        self.maximum = maximum
        self.buffer = bytearray()

    def read(self, deadline):
        while True:
            newline = self.buffer.find(b"\n")
            if newline >= 0:
                raw = bytes(self.buffer[: newline + 1])
                del self.buffer[: newline + 1]
                return raw
            if len(self.buffer) > self.maximum:
                raise ValueError("bounded line exceeds maximum")
            ready, _, _ = __import__("select").select([self.fd], [], [], deadline.remaining())
            if not ready:
                raise TimeoutError("bounded line read timed out")
            chunk = os.read(self.fd, min(65536, self.maximum + 1 - len(self.buffer)))
            if not chunk:
                if self.buffer:
                    raise ValueError("unterminated bounded line")
                return None
            self.buffer.extend(chunk)

    def wait_readable(self, deadline):
        """Wait for the first byte without starting a later frame deadline."""
        if self.buffer:
            return
        ready, _, _ = __import__("select").select([self.fd], [], [], deadline.remaining())
        if not ready:
            raise TimeoutError("bounded idle wait timed out")


def write_all(fd, raw, deadline):
    previous = os.get_blocking(fd)
    os.set_blocking(fd, False)
    view = memoryview(raw)
    try:
        while view:
            _, ready, _ = __import__("select").select([], [fd], [], deadline.remaining())
            if not ready:
                raise TimeoutError("bounded write timed out")
            try:
                count = os.write(fd, view[:65536])
            except BlockingIOError:
                continue
            if count <= 0:
                raise BrokenPipeError("bounded write made no progress")
            view = view[count:]
    finally:
        os.set_blocking(fd, previous)


def run_capped(argv, deadline, input_bytes=None, maximum=262_144, allowed_status=(0,)):
    """Run one fixed argv while limiting accumulated stdout and stderr bytes."""
    if not isinstance(argv, (list, tuple)) or not argv or any(not isinstance(item, str) or not item for item in argv):
        raise ValueError("fixed argv must be a non-empty string vector")
    if input_bytes is not None and not isinstance(input_bytes, bytes):
        raise ValueError("command input must be bytes")
    reserve = min(0.2, deadline.remaining() * 0.2)
    work_deadline = Deadline(deadline.remaining() - reserve)
    process = subprocess.Popen(
        list(argv), stdin=subprocess.PIPE if input_bytes is not None else subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
    )
    selector = selectors.DefaultSelector()
    stdout_fd = process.stdout.fileno()
    stderr_fd = process.stderr.fileno()
    outputs = {stdout_fd: bytearray(), stderr_fd: bytearray()}
    os.set_blocking(stdout_fd, False)
    os.set_blocking(stderr_fd, False)
    selector.register(process.stdout, selectors.EVENT_READ)
    selector.register(process.stderr, selectors.EVENT_READ)
    pending = memoryview(input_bytes or b"")
    if process.stdin is not None:
        os.set_blocking(process.stdin.fileno(), False)
        if pending:
            selector.register(process.stdin, selectors.EVENT_WRITE)
        else:
            process.stdin.close()
    try:
        while selector.get_map():
            events = selector.select(work_deadline.remaining())
            if not events:
                raise TimeoutError("fixed command timed out")
            for key, mask in events:
                if process.stdin is not None and key.fileobj is process.stdin:
                    try:
                        count = os.write(process.stdin.fileno(), pending[:65536])
                    except BlockingIOError:
                        continue
                    except BrokenPipeError:
                        count = len(pending)
                    pending = pending[count:]
                    if not pending:
                        selector.unregister(process.stdin)
                        process.stdin.close()
                elif mask & selectors.EVENT_READ:
                    chunk = os.read(key.fd, 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        key.fileobj.close()
                    else:
                        value = outputs[key.fd]
                        if len(value) + len(chunk) > maximum:
                            raise RuntimeError("fixed command output exceeded bound")
                        value.extend(chunk)
        status = process.wait(timeout=work_deadline.remaining())
    except BaseException:
        process.kill()
        try:
            # Reaping is a separate finite cancellation obligation. It must
            # still run after the command deadline itself is exhausted.
            process.wait(timeout=deadline.remaining())
        except (subprocess.TimeoutExpired, TimeoutError):
            pass
        for handle in (process.stdin, process.stdout, process.stderr):
            if handle is not None and not handle.closed:
                handle.close()
        selector.close()
        raise
    selector.close()
    stdout = bytes(outputs[stdout_fd])
    stderr = bytes(outputs[stderr_fd])
    if status not in allowed_status:
        raise RuntimeError("fixed command failed with status {}".format(status))
    return subprocess.CompletedProcess(list(argv), status, stdout, stderr)
