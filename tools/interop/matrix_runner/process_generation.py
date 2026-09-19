# SPDX-License-Identifier: AGPL-3.0-only
"""Owned local process generations, including descendants after leader exit."""

from __future__ import annotations

import ctypes
import errno
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import time


class ProcessCleanupError(RuntimeError):
    pass


def _snapshot(deadline):
    """Observe non-zombie processes using native precise start identities."""
    deadline.remaining()
    rows = {}
    if sys.platform == "darwin":
        libc = ctypes.CDLL(None, use_errno=True)
        needed = libc.proc_listpids(1, 0, None, 0)
        if needed <= 0 or needed > 1_048_576:
            raise ProcessCleanupError("cannot bound local process inventory")
        values = (ctypes.c_int * (needed // 4 + 1024))()
        length = libc.proc_listpids(1, 0, values, ctypes.sizeof(values))
        if length <= 0 or length >= ctypes.sizeof(values):
            raise ProcessCleanupError("local process inventory changed beyond its bound")
        for pid in values[:length // 4]:
            deadline.remaining()
            if pid <= 0:
                continue
            info = ctypes.create_string_buffer(136)
            if libc.proc_pidinfo(pid, 3, 0, info, 136) < 136:
                # Other users' processes and concurrently reaped processes
                # carry no authority for this owned generation.
                continue
            raw = info.raw
            status, observed, parent, uid = (struct.unpack_from("I", raw, offset)[0]
                                             for offset in (4, 12, 16, 20))
            group = struct.unpack_from("I", raw, 100)[0]
            seconds, micros = struct.unpack_from("QQ", raw, 120)
            if observed != pid or not seconds:
                raise ProcessCleanupError("local process identity changed while reading")
            if uid == os.getuid() and status != 5:
                rows[pid] = (parent, group, (seconds, micros))
    elif sys.platform.startswith("linux"):
        for directory in Path("/proc").iterdir():
            deadline.remaining()
            if not directory.name.isdigit():
                continue
            try:
                if directory.stat().st_uid != os.getuid():
                    continue
                with (directory / "stat").open("rb") as stream:
                    raw = stream.read(8193)
                if len(raw) > 8192:
                    raise ProcessCleanupError("local process stat exceeds bound")
                tail = raw[raw.rfind(b")") + 2:].split()
                if tail[0] == b"Z":
                    continue
                rows[int(directory.name)] = (int(tail[1]), int(tail[2]), (int(tail[19]),))
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
    else:
        raise ProcessCleanupError("local process generation observer is unavailable")
    deadline.remaining()
    return rows


class OwnedProcess:
    """Track the source-fixed child's dedicated process group and seen lineage.

    Launchers must use start_new_session=True. Independently observed child
    generations remain owned after reparenting or a worker group change.
    Unknown or replaced PIDs never gain authority from a retained number.
    """

    def __init__(self, process, deadline):
        if not isinstance(process, subprocess.Popen) or type(process.pid) is not int:
            raise ProcessCleanupError("launcher returned no local process generation")
        self.process = process
        self.group = process.pid
        self.generations = {}
        self.group_retired = False
        self.refresh(deadline)

    def refresh(self, deadline):
        rows = _snapshot(deadline)
        leader = rows.get(self.process.pid)
        if leader is not None and leader[1] != self.group and self.process.poll() is None:
            raise ProcessCleanupError("adapter was not launched in a dedicated process group")
        known = {pid for pid, generation in self.generations.items()
                 if pid in rows and rows[pid][2] == generation}
        if not self.group_retired:
            known.update(pid for pid, row in rows.items() if row[1] == self.group)
        changed = True
        while changed:
            children = {pid for pid, row in rows.items() if row[0] in known}
            changed = not children.issubset(known)
            known.update(children)
        for pid in known:
            generation = rows[pid][2]
            if pid in self.generations and self.generations[pid] != generation:
                raise ProcessCleanupError("owned PID generation changed")
            self.generations[pid] = generation
        if not any(row[1] == self.group for row in rows.values()):
            self.group_retired = True
        return {pid: generation for pid, generation in self.generations.items()
                if pid in rows and rows[pid][2] == generation}

    def _signal(self, sig, deadline):
        for pid in self.refresh(deadline):
            # Revalidate immediately before each signal; never signal a
            # recycled generation on the strength of group membership alone.
            rows = _snapshot(deadline)
            if pid not in rows or rows[pid][2] != self.generations[pid]:
                continue
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass

    def cleanup(self, deadline):
        if self.refresh(deadline):
            self._signal(signal.SIGTERM, deadline)
            grace = min(time.monotonic() + 0.75, deadline.end)
            while self.refresh(deadline) and time.monotonic() < grace:
                time.sleep(min(0.01, deadline.remaining()))
            if self.refresh(deadline):
                self._signal(signal.SIGKILL, deadline)
        while self.refresh(deadline):
            time.sleep(min(0.01, deadline.remaining()))
        try:
            self.process.wait(timeout=deadline.remaining())
        except subprocess.TimeoutExpired as error:
            raise ProcessCleanupError("owned leader could not be reaped") from error

    def require_absent(self, deadline):
        # The leader is expected to consume the acknowledgement and exit
        # after this method is called.  Wait for that exact Popen child first;
        # only then can a retained descendant be classified as a leak.
        try:
            self.process.wait(timeout=deadline.remaining())
        except subprocess.TimeoutExpired as error:
            raise ProcessCleanupError("owned adapter leader did not exit") from error
        if self.refresh(deadline):
            raise ProcessCleanupError("owned adapter descendants remain after child exit")
