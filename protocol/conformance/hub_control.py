"""Bounded client for the private installed-host broker data protocol.

This module deliberately knows no service commands, host names, package layout,
or controller implementation.  It authenticates one sequenced NDJSON
attachment and exposes only the five closed lifecycle operations.
"""

from __future__ import annotations

import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import socket
import stat
import threading
import time
import uuid


MAX_FRAME_BYTES = 1_048_576
HEX64 = re.compile(r"^[0-9a-f]{64}$")
OPERATIONS = {"verify", "stop", "start", "pair", "revoke"}
ERROR_CODES = {"invalid-request", "operation-failed"}
GREETING_DEADLINE = 10.0
DEADLINES = {
    "verify": 30.0,
    "stop": 60.0,
    "start": 60.0,
    "pair": 160.0,
    "revoke": 160.0,
}


class ControlProtocolError(Exception):
    """A fixed-label local failure; peer data is never included in messages."""


class BrokerOperationError(ControlProtocolError):
    def __init__(self, code: str):
        self.code = code
        super().__init__("broker operation failed: " + code)


def _exact(value, keys, label):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ControlProtocolError(label + " shape is invalid")
    return value


def _strict_json(raw: bytes):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate member")
            result[key] = value
        return result

    def reject_constant(_value):
        raise ValueError("non-finite number")

    def finite(value):
        result = float(value)
        if not math.isfinite(result):
            raise ValueError("overflowing number")
        return result

    text = raw.decode("utf-8", "strict")
    return json.loads(
        text,
        object_pairs_hook=unique,
        parse_constant=reject_constant,
        parse_float=finite,
    )


def _canonical_uuid(value, label):
    if not isinstance(value, str) or len(value) != 36:
        raise ValueError(label + " must be a canonical UUID")
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError) as error:
        raise ValueError(label + " must be a canonical UUID") from error
    if str(parsed) != value or parsed.version != 4:
        raise ValueError(label + " must be a canonical UUID")
    return value


def _digest(value, label):
    if not isinstance(value, str) or HEX64.fullmatch(value) is None:
        raise ValueError(label + " must be a lowercase SHA-256 digest")
    return value


def validate_host_session(value):
    """Validate the only authority the adapter accepts from its caller."""
    if not isinstance(value, dict) or set(value) != {
        "schema_version",
        "kind",
        "broker_socket",
        "session_id",
        "registration_sha256",
    }:
        raise ValueError("host_session shape is invalid")
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise ValueError("host_session schema_version must equal 1")
    if value["kind"] != "installed-host":
        raise ValueError("host_session kind is invalid")
    broker_socket = value["broker_socket"]
    if not isinstance(broker_socket, str) or not broker_socket or len(broker_socket) > 1024 or "\x00" in broker_socket:
        raise ValueError("host_session broker_socket is invalid")
    pure_path = PurePosixPath(broker_socket)
    if not pure_path.is_absolute() or str(pure_path) != broker_socket or ".." in pure_path.parts:
        raise ValueError("host_session broker_socket must be absolute")
    _canonical_uuid(value["session_id"], "host_session session_id")
    _digest(value["registration_sha256"], "host_session registration_sha256")
    return dict(value)


class ControlClient:
    """Single-use, one-attachment broker client with strict request ordering."""

    def __init__(self, descriptor, connection, challenge, read_buffer, deadlines):
        self.descriptor = descriptor
        self._connection = connection
        self._challenge = challenge
        self._read_buffer = read_buffer
        self._deadlines = deadlines
        self._sequence = 0
        self._closed = False
        self._request_lock = threading.Lock()

    @classmethod
    def connect(cls, value, *, connect_timeout=GREETING_DEADLINE, _deadline_overrides=None):
        descriptor = validate_host_session(value)
        if type(connect_timeout) not in (int, float) or not math.isfinite(connect_timeout) or connect_timeout <= 0:
            raise ValueError("broker greeting deadline must be positive")
        deadlines = dict(DEADLINES)
        if _deadline_overrides is not None:
            if not isinstance(_deadline_overrides, dict) or not set(_deadline_overrides).issubset(OPERATIONS):
                raise ValueError("broker deadline override is invalid")
            for operation, timeout in _deadline_overrides.items():
                if type(timeout) not in (int, float) or not math.isfinite(timeout) or timeout <= 0:
                    raise ValueError("broker deadline override is invalid")
                deadlines[operation] = float(timeout)
        deadline = time.monotonic() + float(connect_timeout)
        path = descriptor["broker_socket"]
        try:
            metadata = os.stat(path, follow_symlinks=False)
        except OSError as error:
            raise ControlProtocolError("broker socket is unavailable") from error
        if (
            not stat.S_ISSOCK(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or metadata.st_mode & 0o077
        ):
            raise ControlProtocolError("broker socket ownership or mode is invalid")
        connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            connection.settimeout(cls._remaining(deadline))
            connection.connect(path)
            read_buffer = bytearray()
            greeting = cls._read_frame(connection, read_buffer, deadline)
            greeting = _exact(
                greeting,
                ("schema_version", "type", "session_id", "sequence", "challenge"),
                "broker greeting",
            )
            if (
                type(greeting["schema_version"]) is not int
                or greeting["schema_version"] != 1
                or greeting["type"] != "challenge"
                or greeting["session_id"] != descriptor["session_id"]
                or type(greeting["sequence"]) is not int
                or greeting["sequence"] != 0
                or not isinstance(greeting["challenge"], str)
                or HEX64.fullmatch(greeting["challenge"]) is None
            ):
                raise ControlProtocolError("broker greeting identity is invalid")
            return cls(descriptor, connection, greeting["challenge"], read_buffer, deadlines)
        except (OSError, TimeoutError):
            connection.close()
            raise ControlProtocolError("broker greeting transport failed") from None
        except BaseException:
            connection.close()
            raise

    @staticmethod
    def _remaining(deadline):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("broker total deadline expired")
        return remaining

    @classmethod
    def _read_frame(cls, connection, read_buffer, deadline):
        while True:
            newline = read_buffer.find(b"\n")
            if newline >= 0:
                if newline > MAX_FRAME_BYTES:
                    raise ControlProtocolError("broker frame exceeds bound")
                raw = bytes(read_buffer[:newline])
                del read_buffer[:newline + 1]
                break
            if len(read_buffer) > MAX_FRAME_BYTES:
                raise ControlProtocolError("broker frame exceeds bound")
            connection.settimeout(cls._remaining(deadline))
            try:
                chunk = connection.recv(min(65_536, MAX_FRAME_BYTES + 1 - len(read_buffer)))
            except socket.timeout:
                raise TimeoutError("broker total deadline expired") from None
            if not chunk:
                raise ControlProtocolError("broker attachment closed")
            read_buffer.extend(chunk)
        try:
            value = _strict_json(raw)
            cls._remaining(deadline)
            return value
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError, TypeError):
            raise ControlProtocolError("broker frame is not strict UTF-8 JSON") from None

    def _fail_closed(self, error):
        self.close()
        raise error

    def request(self, op, *, device_id=None):
        if self._closed:
            raise ControlProtocolError("broker attachment is closed")
        if op not in OPERATIONS:
            raise ValueError("unsupported broker operation")
        if op == "revoke":
            _canonical_uuid(device_id, "revoke device_id")
        elif device_id is not None:
            raise ValueError("device_id is valid only for revoke")
        if not self._request_lock.acquire(False):
            raise ControlProtocolError("overlapping broker operation is forbidden")
        try:
            self._sequence += 1
            request = {
                "schema_version": 1,
                "session_id": self.descriptor["session_id"],
                "sequence": self._sequence,
                "challenge": self._challenge,
                "op": op,
            }
            if op == "revoke":
                request["device_id"] = device_id
            raw = json.dumps(request, separators=(",", ":"), allow_nan=False).encode() + b"\n"
            if len(raw) > MAX_FRAME_BYTES:
                raise ControlProtocolError("broker request exceeds bound")
            deadline = time.monotonic() + self._deadlines[op]
            try:
                self._connection.settimeout(self._remaining(deadline))
                self._connection.sendall(raw)
                reply = self._read_frame(self._connection, self._read_buffer, deadline)
            except (OSError, TimeoutError, ControlProtocolError) as error:
                self._fail_closed(
                    error
                    if isinstance(error, ControlProtocolError)
                    else ControlProtocolError("broker operation transport failed")
                )
            if not isinstance(reply, dict) or reply.get("type") not in {"reply", "error"}:
                self._fail_closed(ControlProtocolError("broker reply type is invalid"))
            payload_key = "result" if reply["type"] == "reply" else "error"
            try:
                _exact(
                    reply,
                    ("schema_version", "type", "session_id", "sequence", "challenge", payload_key),
                    "broker reply",
                )
                sequence_valid = (
                    reply["sequence"] == self._sequence
                    if reply["type"] == "reply"
                    else reply["sequence"] in {self._sequence - 1, self._sequence}
                )
                if (
                    type(reply["schema_version"]) is not int
                    or reply["schema_version"] != 1
                    or reply["session_id"] != self.descriptor["session_id"]
                    or type(reply["sequence"]) is not int
                    or not sequence_valid
                    or not isinstance(reply["challenge"], str)
                    or HEX64.fullmatch(reply["challenge"]) is None
                    or reply["challenge"] == self._challenge
                ):
                    raise ControlProtocolError("broker reply identity is invalid")
                self._challenge = reply["challenge"]
                if reply["type"] == "error":
                    error_value = _exact(reply["error"], ("code",), "broker error")
                    if error_value["code"] not in ERROR_CODES:
                        raise ControlProtocolError("broker error code is invalid")
                    raise BrokerOperationError(error_value["code"])
                result = reply["result"]
                if op == "stop":
                    _exact(result, ("stopped", "events"), "stop result")
                    if result["stopped"] is not True or not isinstance(result["events"], list):
                        raise ControlProtocolError("stop result is invalid")
                else:
                    _exact(
                        result,
                        ("descriptor", "proof", "invitation", "expired_invitation", "events"),
                        "running result",
                    )
                    if not all(isinstance(result[key], dict) for key in ("descriptor", "proof", "invitation", "expired_invitation")) or not isinstance(result["events"], list):
                        raise ControlProtocolError("running result is invalid")
                self._remaining(deadline)
                return result
            except BrokerOperationError as error:
                self._fail_closed(error)
            except (ControlProtocolError, TypeError, TimeoutError) as error:
                self._fail_closed(
                    error
                    if isinstance(error, ControlProtocolError)
                    else ControlProtocolError(
                        "broker operation transport failed"
                        if isinstance(error, TimeoutError)
                        else "broker reply is invalid"
                    )
                )
        finally:
            self._request_lock.release()

    def close(self):
        if self._closed:
            return
        self._closed = True
        self._connection.close()

    def __enter__(self):
        return self

    def __exit__(self, _kind, _value, _traceback):
        self.close()
