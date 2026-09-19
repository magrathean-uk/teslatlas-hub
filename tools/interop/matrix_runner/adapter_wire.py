# SPDX-License-Identifier: AGPL-3.0-only
"""Closed private wire primitives for installed matrix adapters."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import stat
import sys
import threading
import time
from types import MappingProxyType, ModuleType
from typing import Any, Mapping


HEX64 = re.compile(r"^[0-9a-f]{64}$")
MAX_FRAME_BYTES = 1_048_576
MAX_EVIDENCE_BYTES = 8_388_608
READY_FIELDS = frozenset({
    "schema_version", "type", "session_id", "cell_id",
    "session_input_sha256", "instance_nonce", "sequence", "phase",
    "observation", "evidence",
})
ACK_FIELDS = frozenset({
    "schema_version", "type", "session_id", "cell_id",
    "session_input_sha256", "instance_nonce", "sequence", "ready_sha256",
    "phase", "status", "action", "result",
})


class WireError(RuntimeError):
    """A closed wire or immutable binding failed validation."""


class ContractPending(WireError):
    """The adapter has no independently reviewed registry entry."""


@dataclass(frozen=True)
class ReviewedContract:
    manifest_path: str
    manifest_sha256: str
    validator_path: str
    validator_sha256: str


@dataclass(frozen=True)
class LoadedContract:
    manifest: Mapping[str, Any]
    module: ModuleType


@dataclass(frozen=True)
class AdmittedReady:
    value: Mapping[str, Any]
    evidence_bytes: bytes


def _unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise WireError("duplicate JSON key")
        result[key] = value
    return result


def _finite_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed):
        raise WireError("nonfinite JSON number")
    return parsed


def strict_json(raw: bytes) -> Any:
    if not isinstance(raw, bytes):
        raise WireError("JSON input must be bytes")
    try:
        text = raw.decode("utf-8")
        decoder = json.JSONDecoder(
            object_pairs_hook=_unique,
            parse_constant=lambda _value: (_ for _ in ()).throw(WireError("nonfinite JSON number")),
            parse_float=_finite_float,
        )
        value, end = decoder.raw_decode(text)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise WireError("invalid JSON") from error
    if text[end:].strip():
        raise WireError("trailing JSON value")
    return value


def sha256_bytes(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def sha256_file(path: Path | str, *, maximum: int = MAX_EVIDENCE_BYTES,
                private: bool = True, deadline=None) -> str:
    return sha256_bytes(read_safe_file(path, label="file", maximum=maximum,
                                      private=private, deadline=deadline))


def _canonical_absolute(value: Any, label: str) -> Path:
    if not isinstance(value, str) or not value.startswith("/") or any(item in value for item in ("\x00", "\n", "\r")):
        raise WireError(f"{label} path is invalid")
    path = Path(value)
    if str(path.resolve(strict=False)) != value:
        raise WireError(f"{label} path is not canonical")
    return path


def _file_identity(metadata):
    return (metadata.st_dev, metadata.st_ino, metadata.st_mode, metadata.st_uid,
            metadata.st_nlink, metadata.st_size, metadata.st_mtime_ns, metadata.st_ctime_ns)


def read_safe_file(path: Path | str, *, label: str, maximum: int,
                   private: bool = True, deadline=None) -> bytes:
    """Read only admitted descriptor bytes, including during first binding.

    NONBLOCK makes a FIFO substituted immediately before open harmless.
    The size cap applies to every read, so post-stat growth cannot allocate
    an unbounded buffer. Rechecking both inode and directory entry rejects
    replacement, mode/owner/link changes and same-size writes.
    """
    path = _canonical_absolute(str(path), label)
    if type(maximum) is not int or not 0 < maximum <= 1_073_741_824:
        raise WireError(f"{label} bound is invalid")
    end = time.monotonic() + 10

    def remaining():
        if time.monotonic() >= end:
            raise WireError(f"{label} read deadline expired")
        if deadline is not None:
            deadline.remaining()

    def admit(metadata):
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1 or metadata.st_uid != os.getuid():
            raise WireError(f"{label} is not an owned exclusive regular file")
        if private and stat.S_IMODE(metadata.st_mode) & 0o077:
            raise WireError(f"{label} is not owner-only")
        if metadata.st_size > maximum:
            raise WireError(f"{label} exceeds its bound")

    fd = None
    try:
        remaining()
        metadata = path.lstat()
        admit(metadata)
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
        opened = os.fstat(fd)
        admit(opened)
        if _file_identity(opened) != _file_identity(metadata):
            raise WireError(f"{label} changed before reading")
        buffer = bytearray()
        while True:
            remaining()
            chunk = os.read(fd, min(65_536, maximum + 1 - len(buffer)))
            if not chunk:
                break
            buffer.extend(chunk)
            if len(buffer) > maximum:
                raise WireError(f"{label} exceeds its bound")
        if _file_identity(os.fstat(fd)) != _file_identity(metadata):
            raise WireError(f"{label} changed while reading")
        after = path.lstat()
    except OSError as error:
        raise WireError(f"{label} cannot be read") from error
    finally:
        if fd is not None:
            os.close(fd)
    remaining()
    if len(buffer) != metadata.st_size or _file_identity(after) != _file_identity(metadata):
        raise WireError(f"{label} changed while reading")
    return bytes(buffer)


def read_bound_file(binding: Mapping[str, Any], *, label: str, maximum: int,
                    private: bool = True, deadline=None) -> bytes:
    if not isinstance(binding, Mapping) or set(binding) != {"path", "sha256"}:
        raise WireError(f"{label} binding is invalid")
    path = _canonical_absolute(binding["path"], label)
    if not isinstance(binding["sha256"], str) or not HEX64.fullmatch(binding["sha256"]):
        raise WireError(f"{label} digest is invalid")
    raw = read_safe_file(path, label=label, maximum=maximum, private=private, deadline=deadline)
    if sha256_bytes(raw) != binding["sha256"]:
        raise WireError(f"{label} digest changed")
    return raw


def file_binding(path: Path | str, *, maximum: int = MAX_EVIDENCE_BYTES,
                 private: bool = True, deadline=None) -> dict[str, str]:
    candidate = _canonical_absolute(str(Path(path)), "file")
    raw = read_safe_file(candidate, label="file", maximum=maximum, private=private, deadline=deadline)
    return {"path": str(candidate), "sha256": sha256_bytes(raw)}


def _exact(value: Any, fields: set[str] | frozenset[str], label: str) -> None:
    if not isinstance(value, dict) or set(value) != set(fields):
        raise WireError(f"{label} shape is invalid")


def _typed_equal(left: Any, right: Any) -> bool:
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return set(left) == set(right) and all(_typed_equal(left[key], right[key]) for key in left)
    if isinstance(left, list):
        return len(left) == len(right) and all(_typed_equal(a, b) for a, b in zip(left, right))
    return left == right


def _identity(value: Mapping[str, Any], *, session_id: str, cell_id: str,
              session_input_sha256: str, instance_nonce: str, sequence: int) -> None:
    if (
        type(value["schema_version"]) is not int or value["schema_version"] != 1 or value["session_id"] != session_id
        or value["cell_id"] != cell_id or value["session_input_sha256"] != session_input_sha256
        or value["instance_nonce"] != instance_nonce or type(value["sequence"]) is not int
        or value["sequence"] != sequence or not 1 <= sequence <= 128
    ):
        raise WireError("coordination identity is invalid")


def validate_ready(
    value: Any, *, session_id: str, cell_id: str, session_input_sha256: str,
    instance_nonce: str, sequence: int, allowed_phases: set[str],
    observation: Mapping[str, Any], deadline=None,
) -> AdmittedReady:
    _exact(value, READY_FIELDS, "ready")
    _identity(value, session_id=session_id, cell_id=cell_id,
              session_input_sha256=session_input_sha256,
              instance_nonce=instance_nonce, sequence=sequence)
    if value["type"] != "ready" or value["phase"] not in allowed_phases:
        raise WireError("ready phase is invalid")
    _exact(value["observation"], {"session_sequence", "proof_sha256"}, "ready observation")
    if not _typed_equal(value["observation"], observation):
        raise WireError("ready observation is stale")
    if type(value["observation"]["session_sequence"]) is not int or value["observation"]["session_sequence"] <= 0:
        raise WireError("ready observation sequence is invalid")
    if not isinstance(value["observation"]["proof_sha256"], str) or not HEX64.fullmatch(value["observation"]["proof_sha256"]):
        raise WireError("ready proof digest is invalid")
    raw = read_bound_file(value["evidence"], label="ready evidence", maximum=MAX_EVIDENCE_BYTES, deadline=deadline)
    return AdmittedReady(MappingProxyType(dict(value)), raw)


def write_exclusive_json(path: Path | str, value: Mapping[str, Any]) -> dict[str, str]:
    target = _canonical_absolute(str(Path(path)), "coordination output")
    raw = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8") + b"\n"
    target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    except OSError as error:
        raise WireError("coordination output already exists or is unsafe") from error
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    return {"path": str(target), "sha256": sha256_bytes(raw)}


def write_ack(
    path: Path | str, *, session_id: str, cell_id: str,
    session_input_sha256: str, instance_nonce: str, sequence: int,
    ready_binding: Mapping[str, Any], phase: str, status: str, action: str,
    result: Mapping[str, str] | None, deadline=None,
) -> dict[str, Any]:
    if deadline is not None:
        deadline.remaining()
    ready_raw = read_bound_file(ready_binding, label="ready record", maximum=65_536, deadline=deadline)
    if status not in {"accepted", "rejected"} or action not in {"advance_once", "close_completed", "abort"}:
        raise WireError("acknowledgement disposition is invalid")
    if (status == "accepted") != (action != "abort"):
        raise WireError("acknowledgement status and action conflict")
    value = {
        "schema_version": 1, "type": "ack", "session_id": session_id,
        "cell_id": cell_id, "session_input_sha256": session_input_sha256,
        "instance_nonce": instance_nonce, "sequence": sequence,
        "ready_sha256": sha256_bytes(ready_raw), "phase": phase,
        "status": status, "action": action, "result": result,
    }
    _exact(value, ACK_FIELDS, "ack")
    write_exclusive_json(path, value)
    if deadline is not None:
        deadline.remaining()
    return value


_IMPORT_LOCK = threading.RLock()


def execute_reviewed_module(name: str, path: Path | str, raw: bytes) -> ModuleType:
    """Execute the exact admitted bytes with dataclass-compatible registration.

    Source loaders and bytecode caches never participate. A caller needing
    sibling module imports installs the returned module only for that scoped
    execution; the previous global entry is always restored here.
    """
    specification = importlib.util.spec_from_file_location(name, path)
    if specification is None:
        raise WireError("reviewed module cannot be loaded")
    module = importlib.util.module_from_spec(specification)
    with _IMPORT_LOCK:
        previous = sys.modules.get(name)
        try:
            sys.modules[name] = module
            exec(compile(raw, str(path), "exec", dont_inherit=True), module.__dict__)
        finally:
            if previous is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = previous
    return module


def deep_freeze(value: Any) -> Any:
    if isinstance(value, Mapping):
        return MappingProxyType({key: deep_freeze(item) for key, item in value.items()})
    if isinstance(value, (tuple, list)):
        return tuple(deep_freeze(item) for item in value)
    if isinstance(value, (str, int, float, bool, bytes)) or value is None:
        return value
    raise WireError("immutable admission value has an unsupported type")


def _reviewed_validator_path(manifest_validator: Any, entry: ReviewedContract) -> Path:
    _exact(manifest_validator, {"path", "sha256"}, "reviewed validator binding")
    if manifest_validator["sha256"] != entry.validator_sha256:
        raise WireError("reviewed validator binding is mismatched")
    source_path = _canonical_absolute(entry.validator_path, "reviewed adapter validator")
    value = manifest_validator["path"]
    if not isinstance(value, str) or not value or any(item in value for item in ("\x00", "\n", "\r")):
        raise WireError("reviewed validator path is invalid")
    if value.startswith("/"):
        if value != entry.validator_path:
            raise WireError("reviewed validator binding is mismatched")
        return source_path
    relative = Path(value)
    if relative.is_absolute() or relative.as_posix() != value or any(part in {"", ".", ".."} for part in relative.parts):
        raise WireError("reviewed validator relative path is invalid")
    manifest_path = _canonical_absolute(entry.manifest_path, "reviewed adapter manifest")
    candidate = manifest_path.parent / relative
    try:
        resolved = candidate.resolve(strict=False)
        resolved.relative_to(manifest_path.parent)
    except (OSError, RuntimeError, ValueError) as error:
        raise WireError("reviewed validator relative path is invalid") from error
    if resolved != candidate or resolved != source_path:
        raise WireError("reviewed validator binding is mismatched")
    return resolved


def load_reviewed_contract(adapter_id: str, registry: Mapping[str, ReviewedContract], *, private: bool = True) -> LoadedContract:
    entry = registry.get(adapter_id)
    if entry is None:
        raise ContractPending("pending: reviewed adapter contract is unavailable")
    manifest_raw = read_bound_file({"path": entry.manifest_path, "sha256": entry.manifest_sha256}, label="reviewed adapter manifest", maximum=MAX_FRAME_BYTES, private=private)
    manifest = strict_json(manifest_raw)
    _exact(manifest, {"schema_version", "adapter_id", "revision", "required_cases", "actors", "cases", "raw_schemas", "phases", "validator"}, "reviewed adapter manifest")
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != 1 or manifest["adapter_id"] != adapter_id or type(manifest["revision"]) is not int or manifest["revision"] <= 0:
        raise WireError("reviewed adapter manifest identity is invalid")
    validator_path = _reviewed_validator_path(manifest["validator"], entry)
    validator_raw = read_bound_file({"path": str(validator_path), "sha256": entry.validator_sha256}, label="reviewed adapter validator", maximum=MAX_FRAME_BYTES, private=private)
    try:
        module = execute_reviewed_module("_teslatlas_matrix_contract_" + adapter_id,
                                         validator_path, validator_raw)
    except Exception as error:
        raise WireError("reviewed validator import failed") from error
    if any(not hasattr(module, name) for name in {"ADAPTER_ID", "CONTRACT_REVISION", "DECISION_CODES", "admit_case"}):
        raise WireError("reviewed validator interface is incomplete")
    if module.ADAPTER_ID != adapter_id or module.CONTRACT_REVISION != manifest["revision"] or not isinstance(module.DECISION_CODES, frozenset) or not callable(module.admit_case):
        raise WireError("reviewed validator interface is invalid")
    if read_bound_file({"path": str(validator_path), "sha256": entry.validator_sha256},
                       label="reviewed adapter validator", maximum=MAX_FRAME_BYTES, private=private) != validator_raw:
        raise WireError("reviewed validator changed during import")
    return LoadedContract(deep_freeze(manifest), module)


def _staged(value: Any, label: str) -> tuple[Mapping[str, Any], Mapping[str, Any]]:
    _exact(value, {"id", "root", "local"}, label)
    if not isinstance(value["id"], str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", value["id"]):
        raise WireError(f"{label} id is invalid")
    for binding in (value["root"], value["local"]):
        if not isinstance(binding, dict) or set(binding) != {"path", "sha256"}:
            raise WireError(f"{label} binding is invalid")
        _canonical_absolute(binding["path"], label)
        if not isinstance(binding["sha256"], str) or not HEX64.fullmatch(binding["sha256"]):
            raise WireError(f"{label} digest is invalid")
    if value["root"]["sha256"] != value["local"]["sha256"]:
        raise WireError(f"{label} staged digests differ")
    return value["root"], value["local"]


def validate_profile_inputs(manifest: Any, members: Any, expected_digest: str, deadline=None) -> None:
    """Admit the real 18-member profile and its SHA256SUMS manifest."""
    root_manifest, local_manifest = _staged(manifest, "profile manifest")
    if root_manifest["sha256"] != expected_digest or local_manifest["sha256"] != expected_digest:
        raise WireError("profile manifest digest does not match admitted profile")
    if Path(local_manifest["path"]).name != "SHA256SUMS":
        raise WireError("profile manifest is not SHA256SUMS")
    if not isinstance(members, list) or len(members) != 18:
        raise WireError("profile member set must contain exactly 18 files")
    local_paths = []
    local_bindings = {}
    ids = []
    for index, member in enumerate(members):
        _root, local = _staged(member, f"profile member {index}")
        path = Path(local["path"])
        local_paths.append(path)
        local_bindings[path.name] = local
        ids.append(member["id"])
    if len(set(ids)) != 18 or len(set(local_paths)) != 18 or len(local_bindings) != 18:
        raise WireError("profile members are duplicated")
    if local_manifest["path"] not in {str(path) for path in local_paths}:
        raise WireError("profile members omit SHA256SUMS")
    manifest_raw = read_bound_file(local_manifest, label="profile SHA256SUMS", maximum=1_048_576, deadline=deadline)
    try:
        text = manifest_raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise WireError("profile SHA256SUMS is not UTF-8") from error
    declared = {}
    for line in text.splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([^/\x00\r\n]+)", line)
        if match is None or match.group(2) == "SHA256SUMS" or match.group(2) in declared:
            raise WireError("profile SHA256SUMS line is invalid")
        declared[match.group(2)] = match.group(1)
    content_names = set(local_bindings) - {"SHA256SUMS"}
    if set(declared) != content_names or len(declared) != 17:
        raise WireError("profile SHA256SUMS does not enumerate 17 content members")
    for name, digest in declared.items():
        binding = local_bindings[name]
        if binding["sha256"] != digest:
            raise WireError("profile member digest differs from SHA256SUMS")
        read_bound_file(binding, label="profile member", maximum=1_048_576, deadline=deadline)
    profile = strict_json(read_bound_file(local_bindings["profile.json"], label="profile.json", maximum=1_048_576, deadline=deadline))
    if not isinstance(profile, dict) or profile.get("profile_id") != "hub-http-v1@1.0.0":
        raise WireError("profile.json identity is invalid")
