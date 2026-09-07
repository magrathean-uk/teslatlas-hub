# SPDX-License-Identifier: AGPL-3.0-only
"""Runner-local exclusive lease for one registered installed-host resource."""
import fcntl
import json
import os
import stat
import time
from pathlib import Path

from .contract import ContractError


LOCK_ROOT = Path("/Users/bolyki/.codex/artifacts/teslatlas-interop/installed-host-resource-locks")


class HostLease:
    def __init__(self, registered, clock=time.time, lock_root=None):
        self.registered = registered
        self.clock = clock
        self.directory = Path(lock_root) if lock_root is not None else LOCK_ROOT
        self.path = self.directory / (registered.registration["lease"]["resource_id"] + ".lock")
        self.file = None
        self.record = None

    def acquire(self, minimum_seconds):
        lease = self.registered.registration["lease"]
        if lease["expires_at_unix"] <= int(self.clock() + minimum_seconds):
            raise ContractError("registered resource lease does not cover the bounded operation")
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = self.directory.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_mode & 0o077:
            raise ContractError("host lease directory is not private")
        self.file = self.path.open("a+b", buffering=0)
        os.chmod(str(self.path), 0o600)
        try:
            fcntl.flock(self.file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            self.file.close()
            self.file = None
            raise ContractError("registered host resource already has an active controller") from error
        self.file.seek(0)
        existing_raw = self.file.read()
        if existing_raw:
            try:
                existing = json.loads(existing_raw.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                self.release()
                raise ContractError("registered host resource lease record is invalid") from error
            if existing.get("quarantined") is True:
                self.release()
                raise ContractError("registered host resource is durably quarantined")
        self.record = {
            "schema_version": 1,
            "session_id": self.registered.config["session_id"],
            "host_id": self.registered.config["host_id"],
            "lease_id": lease["lease_id"],
            "resource_id": lease["resource_id"],
            "expires_at_unix": lease["expires_at_unix"],
            "quarantined": False,
            "quarantine_errors": [],
        }
        raw = json.dumps(self.record, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
        self.file.seek(0)
        self.file.truncate()
        self.file.write(raw)
        os.fsync(self.file.fileno())
        return self

    def quarantine(self, errors):
        if self.file is None or self.file.closed or self.record is None:
            raise ContractError("cannot quarantine an unheld host lease")
        self.record["quarantined"] = True
        self.record["quarantine_errors"] = list(errors)
        raw = json.dumps(self.record, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
        self.file.seek(0)
        self.file.truncate()
        self.file.write(raw)
        os.fsync(self.file.fileno())

    def assert_valid(self, minimum_seconds=0):
        if self.file is None or self.file.closed:
            raise ContractError("registered host resource lease is not held")
        if self.record["expires_at_unix"] <= int(self.clock() + minimum_seconds):
            raise ContractError("registered host resource lease expired during operation")
        self.file.seek(0)
        try:
            observed = json.loads(self.file.read().decode("utf-8"))
        finally:
            self.file.seek(0, os.SEEK_END)
        if observed != self.record:
            raise ContractError("registered host resource lease record changed")

    def release(self):
        if self.file is not None and not self.file.closed:
            fcntl.flock(self.file.fileno(), fcntl.LOCK_UN)
            self.file.close()
