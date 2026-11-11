#!/usr/bin/env python3
"""Build an identity-bound invocation for the synthetic receiver actor.

The producer manifest is the only source of the VIN.  The helper must use the
runtime-VIN contract; helpers with a compiled VIN are rejected before launch.
This module never opens a socket or reads the contents of certificate/key
inputs.
"""

from __future__ import annotations

import json
from pathlib import Path
import re
import stat
from typing import Any


class InvocationError(ValueError):
    """The helper or its identity-bound invocation is not safe to use."""


VIN = re.compile(r"^[A-HJ-NPR-Z0-9]{17}$")
TOKEN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
HELPER_CONTRACT_MARKER = b"teslatlas-synthetic-vehicle-identity-v2"
EMBEDDED_VIN = re.compile(rb"5YJ[A-HJ-NPR-Z0-9]{14}")


def _regular(
    path: Path,
    label: str,
    *,
    executable: bool = False,
    owner_only: bool = True,
) -> None:
    if not path.is_absolute() or path.resolve(strict=False) != path:
        raise InvocationError(f"{label} path is not canonical")
    try:
        metadata = path.lstat()
    except OSError as error:
        raise InvocationError(f"{label} cannot be read") from error
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise InvocationError(f"{label} is not a regular single-link file")
    if owner_only and metadata.st_mode & 0o077:
        raise InvocationError(f"{label} is group/world accessible")
    if executable and not metadata.st_mode & 0o111:
        raise InvocationError(f"{label} is not executable")


def _manifest_vin(manifest_path: Path) -> str:
    _regular(manifest_path, "producer manifest", owner_only=False)
    try:
        value: Any = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise InvocationError("producer manifest is invalid") from error
    if not isinstance(value, dict) or not isinstance(value.get("producer"), dict):
        raise InvocationError("producer manifest has no producer object")
    vin = value["producer"].get("vin")
    if not isinstance(vin, str) or VIN.fullmatch(vin) is None:
        raise InvocationError("producer VIN is invalid")
    return vin


def _helper_contract(helper_path: Path) -> None:
    _regular(helper_path, "synthetic helper", executable=True)
    try:
        raw = helper_path.read_bytes()
    except OSError as error:
        raise InvocationError("synthetic helper cannot be read") from error
    if HELPER_CONTRACT_MARKER not in raw:
        raise InvocationError("synthetic helper lacks runtime identity contract")
    if EMBEDDED_VIN.search(raw) is not None:
        raise InvocationError("synthetic helper embeds a producer VIN")


def build_invocation(
    *,
    manifest_path: Path,
    helper_path: Path,
    endpoint: str,
    certificate_path: Path,
    key_path: Path,
    server_ca_path: Path,
    transaction_id: str,
    created_at_ms: int,
) -> list[str]:
    """Return the only permitted actor argv for a sealed producer manifest."""

    vin = _manifest_vin(manifest_path)
    _helper_contract(helper_path)
    if not endpoint.startswith("wss://") or any(c in endpoint for c in "\x00\r\n"):
        raise InvocationError("receiver endpoint is invalid")
    if TOKEN.fullmatch(transaction_id) is None:
        raise InvocationError("transaction id is invalid")
    if not isinstance(created_at_ms, int) or isinstance(created_at_ms, bool) or created_at_ms <= 0:
        raise InvocationError("created-at-ms is invalid")

    inputs = (certificate_path, key_path, server_ca_path)
    if len({str(path) for path in inputs}) != len(inputs):
        raise InvocationError("TLS input paths must be distinct")
    for label, path in zip(("client certificate", "client key", "server CA"), inputs):
        _regular(path, label)

    return [
        str(helper_path),
        "-endpoint",
        endpoint,
        "-cert",
        str(certificate_path),
        "-key",
        str(key_path),
        "-server-ca",
        str(server_ca_path),
        "-vin",
        vin,
        "-txid",
        transaction_id,
        "-created-at-ms",
        str(created_at_ms),
    ]
