#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Fixed coordinator for the Edge v2 installed matrix lane.

The coordinator is intentionally small and boring.  The root runner owns
processes, stores, credentials and transport observers.  This file owns one
broker attachment and the reviewed phase order only; it never accepts a
command, executable, environment, target path or fault point from session
JSON.  Worker evidence is read from deterministic actor/phase paths and is
bound to the immutable SessionInput before the final evidence barrier.
"""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import re
import socket
import stat
import sys
import time
from typing import Any, Mapping


HERE = Path(__file__).resolve().parent
CONTRACT_DIR = HERE / "edge_contract"
MANIFEST_PATH = CONTRACT_DIR / "matrix-contract.json"
PHASES_PATH = CONTRACT_DIR / "phases.json"
MAX_FRAME_BYTES = 1_048_576
MAX_EVIDENCE_BYTES = 8_388_608
MAX_READY_BYTES = 65_536
POLL_SECONDS = 0.25
TOKEN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
GIT_HEAD = re.compile(r"^[0-9a-f]{40,64}$")
UUID4 = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
)

# SessionInput's common wire is closed.  These values are intentionally
# duplicated from the reviewed Edge contract instead of being configurable.
PRODUCT_VERSION = "2026.36.2"
EDGE_PROFILE_SHA256 = "e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224"
ADAPTER_ID = "edge_v2"
ACTOR_IDS = (
    "installed_controller",
    "edge_normal",
    "edge_producer",
    "edge_reference",
    "edge_fault",
    "edge_transport",
)
EDGE_LINUX_ARCHITECTURE_BY_CELL = {
    "edge_v2__macos_arm64": "arm64",
    "edge_v2__debian13_amd64": "amd64",
    "edge_v2__debian13_arm64": "arm64",
}
REQUIRED_CASES = (
    "candidate_artifact_identity",
    "installed_service_runtime",
    "edge_linux_runtime",
    "edge_mtls_bearer_identity",
    "edge_no_v1_downgrade",
    "edge_source_owner_exclusivity",
    "edge_uninterrupted_delivery_parity",
    "edge_restart_delivery_parity",
    "edge_duplicate_reenqueue_dedup",
    "edge_gap_interleaving_later_ack_rejected",
    "edge_unsupported_event_bounded_disposition",
    "edge_conflicting_identity_blocks_ack",
    "edge_transaction_fault_redelivery",
    "edge_lost_ack_body_redelivery",
    "edge_pending_publication_offline_recovery",
    "edge_clean_shutdown_resume",
)

ACTOR_CONTRACT = {
    "installed_controller": {
        "kind": "installed_controller",
        "execution": "coordinator",
        "runtime_ref": "root_controller",
        "entrypoint_ref": "installed_controller",
        "artifact_roles": ("hub_executable",),
        "source_roles": ("hub_source", "protocol_source"),
        "phase": False,
    },
    "edge_normal": {
        "kind": "edge_normal",
        "execution": "coordinator",
        "runtime_ref": "root_python",
        "entrypoint_ref": "edge_matrix",
        "artifact_roles": (),
        "source_roles": ("hub_source", "protocol_source"),
        "phase": True,
    },
    "edge_producer": {
        "kind": "edge_normal",
        "execution": "docker_worker",
        "runtime_ref": "edge_linux",
        "entrypoint_ref": "edge_producer",
        "artifact_roles": ("edge_executable",),
        "source_roles": ("edge_source", "protocol_source"),
        "phase": True,
    },
    "edge_reference": {
        "kind": "edge_normal",
        "execution": "local_worker",
        "runtime_ref": "hub_target_aux",
        "entrypoint_ref": "edge_reference",
        "artifact_roles": ("hub_executable",),
        "source_roles": ("hub_source", "protocol_source"),
        "phase": True,
    },
    "edge_fault": {
        "kind": "edge_fault",
        "execution": "local_worker",
        "runtime_ref": "hub_target_aux",
        "entrypoint_ref": "edge_fault",
        "artifact_roles": (),
        "source_roles": ("hub_source", "protocol_source"),
        "phase": True,
    },
    "edge_transport": {
        "kind": "edge_fault",
        "execution": "docker_worker",
        "runtime_ref": "edge_linux",
        "entrypoint_ref": "edge_negative_transport",
        "artifact_roles": (),
        "source_roles": ("hub_source", "protocol_source"),
        "phase": True,
    },
}

# Worker phase -> normalized case operation.  This table is code-owned and
# closed; a worker cannot select or rename a case by editing its evidence.
CASE_PHASES = {
    "candidate_artifact_identity": (("initial", "observe_identity", "edge_producer"),),
    "installed_service_runtime": (("initial", "observe_identity", "installed_controller"),),
    "edge_linux_runtime": (("initial", "observe_runtime", "edge_producer"),),
    "edge_mtls_bearer_identity": (
        ("initial", "deliver_positive", "edge_producer"),
        ("negative_auth", "negative_auth", "edge_transport"),
    ),
    "edge_no_v1_downgrade": (
        ("negative_v1_404", "negative_v1_404", "edge_transport"),
        ("negative_v1_body", "negative_v1_body", "edge_transport"),
        ("negative_redirect", "negative_redirect", "edge_transport"),
    ),
    "edge_source_owner_exclusivity": (("initial", "observe_owner", "installed_controller"),),
    "edge_uninterrupted_delivery_parity": (("reference_1", "reference_delivery", "edge_reference"),),
    "edge_restart_delivery_parity": (("primary_1", "restart_delivery", "edge_producer"),),
    "edge_duplicate_reenqueue_dedup": (("primary_2", "duplicate_reenqueue", "edge_producer"),),
    "edge_gap_interleaving_later_ack_rejected": (
        ("gap_prepare", "gap_prepare", "edge_producer"),
        ("gap_reject", "gap_reject", "edge_normal"),
        ("gap_consume", "gap_consume", "edge_producer"),
    ),
    "edge_unsupported_event_bounded_disposition": (("primary_3", "unsupported_event", "edge_producer"),),
    "edge_conflicting_identity_blocks_ack": (("negative_conflict", "conflicting_identity", "edge_transport"),),
    "edge_transaction_fault_redelivery": tuple(
        (phase, phase, "edge_fault")
        for phase in (
            "fault_raw_insert",
            "fault_lifecycle_write",
            "fault_receipt_insert",
            "fault_frontier_update",
            "fault_commit",
            "fault_before_commit",
            "fault_after_commit",
        )
    ),
    "edge_lost_ack_body_redelivery": (("fault_lost_ack", "fault_lost_ack", "edge_fault"),),
    "edge_pending_publication_offline_recovery": (("fault_offline_recovery", "fault_offline_recovery", "edge_fault"),),
    "edge_clean_shutdown_resume": (
        ("resume_stop", "resume_stop", "edge_normal"),
        ("resume_start", "resume_start", "edge_normal"),
        ("actors_closed", "actors_closed", "edge_normal"),
    ),
}


class LauncherError(RuntimeError):
    """A permanent fail-closed coordinator error."""


def _exact(value: Any, keys: set[str], label: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping) or isinstance(value, list) or set(value) != keys:
        raise LauncherError(f"{label} fields invalid")
    return value


def _strict_json(raw: bytes, label: str = "JSON") -> Any:
    if not isinstance(raw, bytes):
        raise LauncherError(f"{label} must be bytes")
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise LauncherError(f"{label} is not UTF-8") from error

    def duplicate_free(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise LauncherError(f"{label} has duplicate keys")
            result[key] = value
        return result

    def finite(value: str) -> float:
        parsed = float(value)
        if not math.isfinite(parsed):
            raise LauncherError(f"{label} contains a nonfinite number")
        return parsed

    decoder = json.JSONDecoder(
        object_pairs_hook=duplicate_free,
        parse_constant=lambda _value: (_ for _ in ()).throw(
            LauncherError(f"{label} contains a nonfinite number")
        ),
        parse_float=finite,
    )
    try:
        value, end = decoder.raw_decode(text)
    except (json.JSONDecodeError, LauncherError) as error:
        raise LauncherError(f"{label} is invalid JSON") from error
    if text[end:].strip():
        raise LauncherError(f"{label} has trailing data")
    return value


def _sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _canonical_json(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def _digest(value: Any, label: str, *, nonzero: bool = True) -> str:
    if not isinstance(value, str) or SHA256.fullmatch(value) is None or (nonzero and value == "0" * 64):
        raise LauncherError(f"{label} digest invalid")
    return value


def _token(value: Any, label: str) -> str:
    if not isinstance(value, str) or TOKEN.fullmatch(value) is None:
        raise LauncherError(f"{label} token invalid")
    return value


def _uuid(value: Any, label: str) -> str:
    if not isinstance(value, str) or UUID4.fullmatch(value) is None:
        raise LauncherError(f"{label} UUID invalid")
    return value


def _path(value: Any, label: str) -> Path:
    if not isinstance(value, str) or not value.startswith("/") or any(c in value for c in "\x00\r\n"):
        raise LauncherError(f"{label} path invalid")
    path = Path(value)
    if str(path.resolve(strict=False)) != value:
        raise LauncherError(f"{label} path is not canonical")
    return path


def _metadata_identity(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_uid,
        metadata.st_nlink,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def _read_owned(path: Path, *, label: str, maximum: int) -> bytes:
    if str(path.resolve(strict=False)) != str(path):
        raise LauncherError(f"{label} path is not canonical")
    try:
        before = path.lstat()
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_nlink != 1
            or before.st_uid != os.getuid()
            or stat.S_IMODE(before.st_mode) & 0o077
            or before.st_size > maximum
        ):
            raise LauncherError(f"{label} file boundary invalid")
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            opened = os.fstat(fd)
            if _metadata_identity(opened) != _metadata_identity(before):
                raise LauncherError(f"{label} changed before read")
            chunks: list[bytes] = []
            size = 0
            while True:
                chunk = os.read(fd, min(65_536, maximum + 1 - size))
                if not chunk:
                    break
                chunks.append(chunk)
                size += len(chunk)
                if size > maximum:
                    raise LauncherError(f"{label} exceeds bound")
            after_fd = os.fstat(fd)
        finally:
            os.close(fd)
        after = path.lstat()
    except OSError as error:
        raise LauncherError(f"{label} cannot be read") from error
    if (
        _metadata_identity(after_fd) != _metadata_identity(before)
        or _metadata_identity(after) != _metadata_identity(before)
        or size != before.st_size
    ):
        raise LauncherError(f"{label} changed while read")
    return b"".join(chunks)


def _binding(value: Any, label: str, *, maximum: int = MAX_EVIDENCE_BYTES) -> tuple[Path, bytes]:
    record = _exact(value, {"path", "sha256"}, label)
    path = _path(record["path"], f"{label}.path")
    expected = _digest(record["sha256"], f"{label}.sha256")
    raw = _read_owned(path, label=label, maximum=maximum)
    if _sha256(raw) != expected:
        raise LauncherError(f"{label} digest mismatch")
    return path, raw


def _read_public(path: Path, *, label: str, maximum: int) -> bytes:
    """Read a reviewed source contract file without treating it as a secret.

    Manifest schema/validator bindings are root-side provenance.  They are
    normally checked-in mode 0644 files, unlike private SessionInput and raw
    evidence files.  They still must be regular, single-link, non-symlink
    files and are checked for replacement while being read.
    """
    try:
        before = path.lstat()
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size > maximum:
            raise LauncherError(f"{label} source boundary invalid")
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            opened = os.fstat(fd)
            if _metadata_identity(opened) != _metadata_identity(before):
                raise LauncherError(f"{label} source changed before read")
            raw = os.read(fd, maximum + 1)
            if len(raw) > maximum:
                raise LauncherError(f"{label} source exceeds bound")
            after_fd = os.fstat(fd)
        finally:
            os.close(fd)
        after = path.lstat()
    except OSError as error:
        raise LauncherError(f"{label} source cannot be read") from error
    if _metadata_identity(after_fd) != _metadata_identity(before) or _metadata_identity(after) != _metadata_identity(before):
        raise LauncherError(f"{label} source changed while read")
    return raw


def _staged(value: Any, label: str, *, read_local: bool = True) -> tuple[Mapping[str, Any], bytes | None]:
    record = _exact(value, {"id", "root", "local"}, label)
    _token(record["id"], f"{label}.id")
    root = _exact(record["root"], {"path", "sha256"}, f"{label}.root")
    local = _exact(record["local"], {"path", "sha256"}, f"{label}.local")
    _path(root["path"], f"{label}.root.path")
    _path(local["path"], f"{label}.local.path")
    root_hash = _digest(root["sha256"], f"{label}.root.sha256")
    local_hash = _digest(local["sha256"], f"{label}.local.sha256")
    if root_hash != local_hash:
        raise LauncherError(f"{label} root/local digest differs")
    if not read_local:
        return record, None
    raw = _read_owned(Path(local["path"]), label=f"{label}.local", maximum=MAX_EVIDENCE_BYTES)
    if _sha256(raw) != local_hash:
        raise LauncherError(f"{label}.local digest mismatch")
    return record, raw


def _validate_file_inventory(value: Any, label: str) -> None:
    if not isinstance(value, list) or len(value) > 4096:
        raise LauncherError(f"{label}.files invalid")
    seen: set[str] = set()
    for index, item in enumerate(value):
        item = _exact(item, {"path", "bytes", "mode", "sha256"}, f"{label}.files[{index}]")
        relative = item["path"]
        if not isinstance(relative, str) or not relative or relative.startswith("/") or "\x00" in relative or any(part == ".." for part in relative.split("/")) or relative in seen:
            raise LauncherError(f"{label}.files[{index}].path invalid")
        seen.add(relative)
        if type(item["bytes"]) is not int or item["bytes"] < 0 or type(item["mode"]) is not int or not 0 <= item["mode"] <= 0o7777:
            raise LauncherError(f"{label}.files[{index}] numeric fields invalid")
        _digest(item["sha256"], f"{label}.files[{index}].sha256")


def _validate_installed_manifest(raw: bytes, label: str, artifact_sha256: str | None = None) -> None:
    value = _strict_json(raw, label)
    if not isinstance(value, Mapping):
        raise LauncherError(f"{label} is not an object")
    if set(value) == {"schema_version", "artifact_sha256", "files"}:
        if value["schema_version"] != 1 or (artifact_sha256 is not None and value["artifact_sha256"] != artifact_sha256):
            raise LauncherError(f"{label} artifact identity invalid")
        _digest(value["artifact_sha256"], f"{label}.artifact_sha256")
        _validate_file_inventory(value["files"], label)
        return
    if set(value) == {"schema_version", "build_record", "files"}:
        if value["schema_version"] != 1:
            raise LauncherError(f"{label} build identity invalid")
        record = _exact(value["build_record"], {"path", "sha256"}, f"{label}.build_record")
        _path(record["path"], f"{label}.build_record.path")
        _digest(record["sha256"], f"{label}.build_record.sha256")
        _validate_file_inventory(value["files"], label)
        return
    raise LauncherError(f"{label} manifest shape invalid")


def _validate_manifest(manifest: Mapping[str, Any]) -> None:
    expected_keys = {"schema_version", "adapter_id", "revision", "required_cases", "actors", "cases", "raw_schemas", "phases", "validator"}
    _exact(manifest, expected_keys, "Edge matrix manifest")
    if manifest["schema_version"] != 1 or manifest["adapter_id"] != ADAPTER_ID or manifest["revision"] != 1:
        raise LauncherError("Edge matrix manifest identity invalid")
    if tuple(manifest["required_cases"]) != REQUIRED_CASES:
        raise LauncherError("Edge matrix manifest cases differ from fixed registry")
    actors = manifest["actors"]
    if not isinstance(actors, list) or tuple(item.get("id") for item in actors) != ACTOR_IDS:
        raise LauncherError("Edge matrix actor registry invalid")
    for item in actors:
        _exact(item, {"id", "kind", "required"}, "Edge matrix actor")
        expected = ACTOR_CONTRACT[item["id"]]
        if item["kind"] != expected["kind"] or item["required"] is not True:
            raise LauncherError("Edge matrix actor identity invalid")
    cases = manifest["cases"]
    if not isinstance(cases, list) or tuple(item.get("id") for item in cases) != REQUIRED_CASES:
        raise LauncherError("Edge matrix case registry invalid")
    for item in cases:
        _exact(item, {"id", "actor_ids", "evidence_kind", "operations", "raw_schema_ids"}, "Edge matrix case")
        if tuple(item["actor_ids"]) != tuple(_case_actor_ids(item["id"])):
            raise LauncherError("Edge matrix case actor identity invalid")
        if item["evidence_kind"] not in {"http", "zero_request", "identity"}:
            raise LauncherError("Edge matrix evidence kind invalid")
        if tuple(item["operations"]) != tuple(_case_operations(item["id"])):
            raise LauncherError("Edge matrix operations invalid")
        if tuple(item["raw_schema_ids"]) != tuple(_case_raw_schema_ids(item["id"])):
            raise LauncherError("Edge matrix raw schema binding invalid")
    raw_schemas = manifest["raw_schemas"]
    if not isinstance(raw_schemas, list) or tuple(item.get("id") for item in raw_schemas) != (
        "edge_fixture_v1", "edge_checkpoint_v1", "edge_http_v1", "edge_fault_v1", "edge_snapshot_v1", "edge_cleanup_v1"
    ):
        raise LauncherError("Edge raw schema registry invalid")
    for item in raw_schemas:
        _exact(item, {"id", "schema"}, "Edge raw schema")
        schema = _exact(item["schema"], {"path", "sha256"}, f"Edge raw schema {item['id']}.schema")
        schema_path = _path(schema["path"], f"Edge raw schema {item['id']}.schema.path")
        schema_raw = _read_public(schema_path, label=f"Edge raw schema {item['id']}", maximum=1_048_576)
        if _sha256(schema_raw) != _digest(schema["sha256"], f"Edge raw schema {item['id']}.schema.sha256"):
            raise LauncherError(f"Edge raw schema {item['id']} digest mismatch")
    phases = manifest["phases"]
    if not isinstance(phases, list) or len(phases) != 31:
        raise LauncherError("Edge phase registry invalid")
    for expected, item in zip(_fixed_phases(), phases, strict=True):
        _exact(item, {"actor_id", "phase_id", "ordinal", "recipe_id"}, "Edge phase")
        if tuple(item[key] for key in ("actor_id", "phase_id", "ordinal", "recipe_id")) != expected:
            raise LauncherError("Edge phase order differs from fixed recipe")
    validator = _exact(manifest["validator"], {"path", "sha256"}, "Edge validator")
    validator_path = _path(validator["path"], "Edge validator.path")
    validator_bytes = _read_public(validator_path, label="Edge validator", maximum=1_048_576)
    if _sha256(validator_bytes) != _digest(validator["sha256"], "Edge validator.sha256"):
        raise LauncherError("Edge validator digest mismatch")


def _fixed_phases() -> tuple[tuple[str, str, int, str], ...]:
    # Importing the pure module is safe and keeps one source of truth for the
    # reviewed recipe order.  The loaded module cannot open files or invoke
    # commands by contract.
    namespace: dict[str, Any] = {}
    source = _read_public(CONTRACT_DIR / "matrix_contract.py", label="Edge validator", maximum=1_048_576).decode("utf-8")
    exec(compile(source, str(CONTRACT_DIR / "matrix_contract.py"), "exec"), namespace)
    return tuple((actor, phase, ordinal, recipe) for actor, phase, ordinal, recipe, _ops in namespace["PHASE_SCHEDULE"])


def _case_actor_ids(case_id: str) -> tuple[str, ...]:
    namespace: dict[str, Any] = {}
    source = _read_public(CONTRACT_DIR / "matrix_contract.py", label="Edge validator", maximum=1_048_576).decode("utf-8")
    exec(compile(source, str(CONTRACT_DIR / "matrix_contract.py"), "exec"), namespace)
    return tuple(namespace["CASE_BINDINGS"][case_id][0]) if case_id in namespace["CASE_BINDINGS"] else ()


def _case_operations(case_id: str) -> tuple[str, ...]:
    namespace: dict[str, Any] = {}
    source = _read_public(CONTRACT_DIR / "matrix_contract.py", label="Edge validator", maximum=1_048_576).decode("utf-8")
    exec(compile(source, str(CONTRACT_DIR / "matrix_contract.py"), "exec"), namespace)
    return tuple(namespace["CASE_BINDINGS"][case_id][1])


def _case_raw_schema_ids(case_id: str) -> tuple[str, ...]:
    namespace: dict[str, Any] = {}
    source = _read_public(CONTRACT_DIR / "matrix_contract.py", label="Edge validator", maximum=1_048_576).decode("utf-8")
    exec(compile(source, str(CONTRACT_DIR / "matrix_contract.py"), "exec"), namespace)
    return tuple(namespace["CASE_BINDINGS"][case_id][3])


@dataclass(frozen=True)
class Session:
    value: Mapping[str, Any]
    raw: bytes
    digest: str
    header: Mapping[str, Any]
    manifest: Mapping[str, Any]
    phases: Mapping[str, Mapping[str, Any]]

    @property
    def session_id(self) -> str:
        return self.value["session_id"]

    @property
    def cell_id(self) -> str:
        return self.value["cell_id"]

    @property
    def instance_nonce(self) -> str:
        return self.value["instance_nonce"]

    @property
    def coordination_dir(self) -> Path:
        return Path(self.value["outputs"]["coordination_dir"])


def _validate_actor(actor: Mapping[str, Any], actor_id: str) -> None:
    expected = ACTOR_CONTRACT[actor_id]
    _exact(actor, {"id", "kind", "execution", "runtime_ref", "artifact_roles", "source_roles", "entrypoint_ref", "input_manifest", "phase_contract"}, f"actor {actor_id}")
    if actor["id"] != actor_id or actor["kind"] != expected["kind"] or actor["execution"] != expected["execution"] or actor["runtime_ref"] != expected["runtime_ref"] or actor["entrypoint_ref"] != expected["entrypoint_ref"]:
        raise LauncherError(f"actor {actor_id} identity mismatch")
    if tuple(actor["artifact_roles"]) != expected["artifact_roles"] or tuple(actor["source_roles"]) != expected["source_roles"]:
        raise LauncherError(f"actor {actor_id} role mismatch")
    _staged_record, input_manifest_raw = _staged(actor["input_manifest"], f"actor {actor_id}.input_manifest")
    _validate_installed_manifest(input_manifest_raw or b"", f"actor {actor_id}.input_manifest")
    phase_contract = actor["phase_contract"]
    if expected["phase"]:
        if phase_contract is None:
            raise LauncherError(f"actor {actor_id} phase contract missing")
        _staged(phase_contract, f"actor {actor_id}.phase_contract")
    elif phase_contract is not None:
        raise LauncherError(f"actor {actor_id} unexpectedly has a phase contract")


def _validate_header(header: Mapping[str, Any], session: Mapping[str, Any], product_inputs: list[Mapping[str, Any]], profile_manifest_sha256: str) -> None:
    _exact(header, {"schema_version", "execution_kind", "adapter", "cell_id", "product_version", "profile_id", "profile_revision", "profile_sha256", "source_identities", "artifacts", "runtime"}, "Edge evidence header")
    if header["schema_version"] != 1 or header["execution_kind"] != "actual_hub_acceptance" or header["adapter"] != ADAPTER_ID or header["cell_id"] != session["cell_id"] or header["product_version"] != PRODUCT_VERSION or header["profile_id"] != "hub-http-v1" or header["profile_revision"] != "1.0.0":
        raise LauncherError("Edge evidence header identity invalid")
    _digest(header["profile_sha256"], "Edge evidence profile")
    if profile_manifest_sha256 != header["profile_sha256"]:
        raise LauncherError("Hub HTTP profile manifest does not match the admitted header")
    if not isinstance(header["artifacts"], list):
        raise LauncherError("Edge evidence artifacts invalid")
    if not isinstance(header["source_identities"], list):
        raise LauncherError("Edge source identities invalid")
    for index, source in enumerate(header["source_identities"]):
        source = _exact(source, {"role", "repo", "head", "dirty_patch_sha256", "untracked_source_manifest_sha256"}, f"Edge source identity {index}")
        _token(source["role"], f"Edge source identity {index}.role")
        _path(source["repo"], f"Edge source identity {index}.repo")
        if not isinstance(source["head"], str) or GIT_HEAD.fullmatch(source["head"]) is None:
            raise LauncherError(f"Edge source identity {index}.head invalid")
        _digest(source["dirty_patch_sha256"], f"Edge source identity {index}.dirty_patch_sha256")
        _digest(source["untracked_source_manifest_sha256"], f"Edge source identity {index}.untracked_source_manifest_sha256")
    artifacts = {item.get("role"): item for item in header["artifacts"] if isinstance(item, Mapping)}
    if set(artifacts) != {"hub_executable", "edge_executable"} or len(artifacts) != len(header["artifacts"]):
        raise LauncherError("Edge evidence artifact roles invalid")
    for index, artifact in enumerate(header["artifacts"]):
        artifact = _exact(artifact, {"role", "name", "path", "embedded_version", "sha256"}, f"Edge artifact identity {index}")
        _token(artifact["role"], f"Edge artifact identity {index}.role")
        _token(artifact["name"], f"Edge artifact identity {index}.name")
        _path(artifact["path"], f"Edge artifact identity {index}.path")
        _token(artifact["embedded_version"], f"Edge artifact identity {index}.embedded_version")
        _digest(artifact["sha256"], f"Edge artifact identity {index}.sha256")
    edge = artifacts.get("edge_executable")
    if edge is None or not isinstance(edge.get("sha256"), str) or edge.get("embedded_version") != PRODUCT_VERSION:
        raise LauncherError("Edge executable artifact identity missing")
    selected = [item for item in product_inputs if item.get("artifact_role") == "edge_executable"]
    if len(selected) != 1:
        raise LauncherError("Edge product input must contain one edge executable")
    staged = selected[0]["staged"]
    if staged["local"]["sha256"] != edge["sha256"]:
        raise LauncherError("Edge executable artifact digest mismatch")
    local_path = Path(staged["local"]["path"])
    try:
        metadata = local_path.stat()
    except OSError as error:
        raise LauncherError("Edge executable cannot be observed") from error
    if not stat.S_ISREG(metadata.st_mode) or not os.access(local_path, os.X_OK):
        raise LauncherError("Edge executable is not an executable regular file")
    runtime = _exact(header["runtime"], {"hub", "client", "browser_engines", "client_transports"}, "Edge runtime identity")
    for runtime_name in ("hub", "client"):
        host = _exact(runtime[runtime_name], {"os", "architecture", "native_or_emulated", "service_mode", "tool_versions"}, f"Edge runtime {runtime_name}")
        for key in ("os", "architecture", "service_mode"):
            _token(host[key], f"Edge runtime {runtime_name}.{key}")
        if host["native_or_emulated"] not in {"native", "emulated"}:
            raise LauncherError(f"Edge runtime {runtime_name}.native_or_emulated invalid")
        versions = host["tool_versions"]
        if not isinstance(versions, Mapping) or not versions or any(not isinstance(key, str) or not key or not isinstance(item, str) or not item for key, item in versions.items()):
            raise LauncherError(f"Edge runtime {runtime_name}.tool_versions invalid")
    for key in ("browser_engines", "client_transports"):
        if not isinstance(runtime[key], list) or any(not isinstance(item, str) or not item for item in runtime[key]):
            raise LauncherError(f"Edge runtime {key} invalid")
    source_identities = header["source_identities"]
    source_roles = {item.get("role") for item in source_identities if isinstance(item, Mapping)}
    if source_roles != {"hub_source", "protocol_source", "edge_source"} or len(source_roles) != len(source_identities):
        raise LauncherError("Edge source roles invalid")


def load_session(path: str | Path) -> Session:
    session_path = _path(str(path), "SessionInput")
    raw = _read_owned(session_path, label="SessionInput", maximum=1_048_576)
    value = _strict_json(raw, "SessionInput")
    _exact(value, {"schema_version", "kind", "run_id", "cell_id", "adapter_id", "client_id", "session_id", "instance_nonce", "header", "case_contract", "host_session", "broker", "inputs", "actors", "outputs", "bounds"}, "SessionInput")
    if value["schema_version"] != 1 or value["kind"] != "matrix-adapter-session" or value["adapter_id"] != ADAPTER_ID or value["client_id"] != ADAPTER_ID:
        raise LauncherError("SessionInput identity invalid")
    _token(value["run_id"], "SessionInput.run_id")
    if value["cell_id"] != ADAPTER_ID + "__" + value["cell_id"].split("__", 1)[-1] or not re.fullmatch(r"edge_v2__(macos_arm64|debian13_amd64|debian13_arm64)", value["cell_id"]):
        raise LauncherError("SessionInput.cell_id invalid")
    _uuid(value["session_id"], "SessionInput.session_id")
    _digest(value["instance_nonce"], "SessionInput.instance_nonce")
    _staged(value["header"], "SessionInput.header")
    _staged(value["case_contract"], "SessionInput.case_contract")
    _, header_raw = _staged(value["header"], "SessionInput.header")
    _, manifest_raw = _staged(value["case_contract"], "SessionInput.case_contract")
    header = _strict_json(header_raw or b"", "Edge evidence header")
    manifest = _strict_json(manifest_raw or b"", "Edge matrix manifest")
    _validate_manifest(manifest)
    host = _exact(value["host_session"], {"schema_version", "kind", "broker_socket", "session_id", "registration_sha256"}, "SessionInput.host_session")
    if host["schema_version"] != 1 or host["kind"] != "installed-host" or host["session_id"] != value["session_id"]:
        raise LauncherError("SessionInput host identity invalid")
    _path(host["broker_socket"], "SessionInput.host_session.broker_socket")
    _digest(host["registration_sha256"], "SessionInput.host_session.registration_sha256")
    broker = _exact(value["broker"], {"kind", "socket_path"}, "SessionInput.broker")
    if broker["kind"] != "unix" or broker["socket_path"] != host["broker_socket"]:
        raise LauncherError("Edge coordinator requires the admitted Unix broker")
    _path(broker["socket_path"], "SessionInput.broker.socket_path")
    inputs = _exact(value["inputs"], {"profile_manifest", "profile_members", "scenario", "certificate", "certificate_der_sha256", "product_inputs"}, "SessionInput.inputs")
    _staged(inputs["profile_manifest"], "SessionInput.inputs.profile_manifest")
    members = inputs["profile_members"]
    if not isinstance(members, list) or len(members) != 18:
        raise LauncherError("Edge profile member count invalid")
    member_ids: list[str] = []
    for index, member in enumerate(members):
        staged, _ = _staged(member, f"SessionInput.inputs.profile_members[{index}]")
        member_ids.append(staged["id"])
    if len(set(member_ids)) != len(member_ids):
        raise LauncherError("Edge profile member IDs are not unique")
    _staged(inputs["scenario"], "SessionInput.inputs.scenario")
    _staged(inputs["certificate"], "SessionInput.inputs.certificate")
    _digest(inputs["certificate_der_sha256"], "SessionInput.inputs.certificate_der_sha256")
    if not isinstance(inputs["product_inputs"], list) or len(inputs["product_inputs"]) != 1:
        raise LauncherError("SessionInput product inputs invalid")
    for index, product in enumerate(inputs["product_inputs"]):
        product = _exact(product, {"artifact_role", "staged", "installed_manifest", "local_root"}, f"product input {index}")
        _token(product["artifact_role"], f"product input {index}.artifact_role")
        _staged(product["staged"], f"product input {index}.staged")
        if product["installed_manifest"] is not None:
            _manifest_record, manifest_raw = _staged(product["installed_manifest"], f"product input {index}.installed_manifest")
            _validate_installed_manifest(manifest_raw or b"", f"product input {index}.installed_manifest", product["staged"]["local"]["sha256"])
        elif product["artifact_role"] == "edge_executable":
            raise LauncherError("Edge executable lacks its installed member manifest")
        if (product["local_root"] is None) != (product["installed_manifest"] is None):
            raise LauncherError("product local_root/installed_manifest mismatch")
        if product["local_root"] is not None:
            _path(product["local_root"], f"product input {index}.local_root")
    if inputs["product_inputs"][0]["artifact_role"] != "edge_executable":
        raise LauncherError("Edge product input role invalid")
    _validate_header(header, value, inputs["product_inputs"], inputs["profile_manifest"]["local"]["sha256"])
    actors = value["actors"]
    if not isinstance(actors, list) or tuple(actor.get("id") for actor in actors) != ACTOR_IDS:
        raise LauncherError("SessionInput actor order invalid")
    for actor_id, actor in zip(ACTOR_IDS, actors, strict=True):
        _validate_actor(actor, actor_id)
    phase_contract_sha = _sha256(_read_public(PHASES_PATH, label="Edge phases", maximum=1_048_576))
    for actor_id, actor in zip(ACTOR_IDS, actors, strict=True):
        if ACTOR_CONTRACT[actor_id]["phase"] and actor["phase_contract"]["local"]["sha256"] != phase_contract_sha:
            raise LauncherError(f"actor {actor_id} phase contract is not the reviewed phase table")
    outputs = _exact(value["outputs"], {"normalized", "actor_evidence", "coordination_dir", "framework_log"}, "SessionInput.outputs")
    output_paths = [_path(outputs[key], f"SessionInput.outputs.{key}") for key in outputs]
    if len(set(output_paths)) != len(output_paths):
        raise LauncherError("SessionInput output paths alias")
    coordination = Path(outputs["coordination_dir"])
    parent = coordination.parent
    try:
        metadata = parent.stat()
    except OSError as error:
        raise LauncherError("SessionInput output parent is unavailable") from error
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
        raise LauncherError("SessionInput output parent is not owner-only")
    bounds = _exact(value["bounds"], {"cell_timeout_ms", "cleanup_timeout_ms", "frame_bytes", "evidence_bytes", "framework_log_bytes"}, "SessionInput.bounds")
    if type(bounds["cell_timeout_ms"]) is not int or not 1 <= bounds["cell_timeout_ms"] <= 3_600_000 or bounds["cleanup_timeout_ms"] != 45_000 or bounds["frame_bytes"] != MAX_FRAME_BYTES or bounds["evidence_bytes"] != MAX_EVIDENCE_BYTES or bounds["framework_log_bytes"] != MAX_EVIDENCE_BYTES:
        raise LauncherError("SessionInput bounds invalid")
    phases = _strict_json(_read_public(PHASES_PATH, label="Edge phases", maximum=1_048_576), "Edge phases")
    _exact(phases, {"schema_version", "kind", "adapter_id", "phases"}, "Edge phases")
    if phases["schema_version"] != 1 or phases["kind"] != "edge-worker-phases" or phases["adapter_id"] != ADAPTER_ID or not isinstance(phases["phases"], list) or len(phases["phases"]) != 31:
        raise LauncherError("Edge phase table invalid")
    phase_map: dict[str, Mapping[str, Any]] = {}
    fixed_schedule = {item[1]: item for item in _load_phase_schedule()}
    for phase in phases["phases"]:
        _exact(phase, {"actor_id", "phase_id", "ordinal", "recipe_id", "operations", "raw_schema_ids"}, "Edge phase entry")
        if phase["phase_id"] in phase_map:
            raise LauncherError("Edge phase ID duplicated")
        expected = fixed_schedule.get(phase["phase_id"])
        if expected is None or (phase["actor_id"], phase["phase_id"], phase["ordinal"], phase["recipe_id"]) != expected[:4] or tuple(phase["operations"]) != expected[4]:
            raise LauncherError("Edge phase recipe differs from fixed schedule")
        phase_map[phase["phase_id"]] = phase
    return Session(value, raw, _sha256(raw), header, manifest, phase_map)


class Broker:
    """Single strict NDJSON broker attachment."""

    def __init__(self, session: Session):
        self.session = session
        self.socket: socket.socket | None = None
        self.buffer = bytearray()
        self.challenge: str | None = None
        self.sequence = 0
        self.failed = False

    def _poison(self, error: Exception) -> LauncherError:
        self.failed = True
        self.close()
        return error if isinstance(error, LauncherError) else LauncherError(str(error))

    def _frame(self, timeout: float) -> Mapping[str, Any]:
        assert self.socket is not None
        deadline = time.monotonic() + timeout
        while True:
            newline = self.buffer.find(b"\n")
            if newline >= 0:
                if newline > MAX_FRAME_BYTES:
                    raise LauncherError("broker frame exceeds bound")
                raw = bytes(self.buffer[:newline])
                del self.buffer[: newline + 1]
                value = _strict_json(raw, "broker frame")
                if not isinstance(value, Mapping):
                    raise LauncherError("broker frame is not an object")
                return value
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise LauncherError("broker frame timed out")
            self.socket.settimeout(remaining)
            try:
                chunk = self.socket.recv(65_536)
            except OSError as error:
                raise LauncherError("broker read failed") from error
            if not chunk:
                raise LauncherError("premature broker EOF")
            self.buffer.extend(chunk)
            if len(self.buffer) > MAX_FRAME_BYTES + 1:
                raise LauncherError("broker frame exceeds bound")

    def open(self, timeout: float = 10.0) -> None:
        if self.failed or self.socket is not None:
            raise LauncherError("broker open state invalid")
        try:
            path = self.session.value["broker"]["socket_path"]
            self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self.socket.settimeout(timeout)
            self.socket.connect(path)
            greeting = self._frame(timeout)
            _exact(greeting, {"schema_version", "type", "session_id", "sequence", "challenge"}, "broker greeting")
            if greeting["schema_version"] != 1 or greeting["type"] != "challenge" or greeting["session_id"] != self.session.session_id or greeting["sequence"] != 0:
                raise LauncherError("broker greeting identity invalid")
            self.challenge = _digest(greeting["challenge"], "broker greeting.challenge")
        except Exception as error:
            raise self._poison(error)

    def request(self, operation: str, *, device_id: str | None = None, timeout: float = 160.0) -> Mapping[str, Any]:
        if self.failed or self.socket is None or self.challenge is None:
            raise LauncherError("broker is unavailable")
        if operation not in {"verify", "stop", "start", "pair", "revoke"}:
            raise LauncherError("broker operation is not registered")
        if operation == "revoke":
            _uuid(device_id, "broker revoke.device_id")
        try:
            self.sequence += 1
            request: dict[str, Any] = {"schema_version": 1, "session_id": self.session.session_id, "sequence": self.sequence, "challenge": self.challenge, "op": operation}
            if operation == "revoke":
                request["device_id"] = device_id
            payload = json.dumps(request, sort_keys=True, separators=(",", ":")).encode() + b"\n"
            if len(payload) - 1 > MAX_FRAME_BYTES:
                raise LauncherError("broker request exceeds bound")
            self.socket.sendall(payload)
            reply = self._frame(timeout)
            if reply.get("type") == "reply":
                _exact(reply, {"schema_version", "type", "session_id", "sequence", "challenge", "result"}, "broker reply")
            else:
                _exact(reply, {"schema_version", "type", "session_id", "sequence", "challenge", "error"}, "broker error")
            if reply["schema_version"] != 1 or reply["session_id"] != self.session.session_id or reply["sequence"] != self.sequence or reply["challenge"] == request["challenge"]:
                raise LauncherError("broker reply identity invalid")
            self.challenge = _digest(reply["challenge"], "broker reply.challenge")
            if reply["type"] != "reply":
                raise LauncherError("broker operation failed")
            result = reply["result"]
            if not isinstance(result, Mapping):
                raise LauncherError("broker result is not an object")
            if operation == "stop":
                if set(result) != {"stopped", "events"} or result["stopped"] is not True or not isinstance(result["events"], list):
                    raise LauncherError("broker stop result invalid")
            else:
                if set(result) != {"descriptor", "proof", "invitation", "expired_invitation", "events"} or not isinstance(result["proof"], Mapping):
                    raise LauncherError("broker running result invalid")
                proof = result["proof"]
                if proof.get("schema_version") != 1 or proof.get("status") != "verified" or proof.get("session_id") != self.session.session_id or proof.get("sequence") != self.sequence or proof.get("challenge") != request["challenge"]:
                    raise LauncherError("broker proof identity invalid")
            return result
        except Exception as error:
            raise self._poison(error)

    def close(self) -> None:
        if self.socket is not None:
            try:
                self.socket.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            self.socket.close()
            self.socket = None


def _write_exclusive(path: Path, value: Any, *, maximum: int = MAX_EVIDENCE_BYTES) -> tuple[Path, str]:
    raw = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode() + b"\n"
    if len(raw) > maximum:
        raise LauncherError(f"output {path} exceeds bound")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if str(path.resolve(strict=False)) != str(path):
        raise LauncherError(f"output {path} is not canonical")
    metadata = path.parent.stat()
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
        raise LauncherError(f"output {path} parent is not owner-only")
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        os.write(fd, raw)
        os.fsync(fd)
    finally:
        os.close(fd)
    return path, _sha256(raw)


def _wait_read(path: Path, *, deadline: float, label: str, maximum: int) -> bytes:
    while True:
        if time.monotonic() >= deadline:
            raise LauncherError(f"{label} deadline expired")
        try:
            return _read_owned(path, label=label, maximum=maximum)
        except LauncherError as error:
            if "cannot be read" not in str(error) and "No such file" not in str(error):
                # lstat ENOENT is represented as a read failure.  A file that
                # exists but is malformed or unsafe is never retried.
                if path.exists():
                    raise
        time.sleep(POLL_SECONDS)


def _raw_schema(payload: Mapping[str, Any], allowed: tuple[str, ...]) -> str:
    candidates = {
        "edge_fixture_v1": {"seal_sha256", "http_profile_sha256", "edge_profile_sha256", "primary_context_sha256", "baseline_clone_equal", "custody_manifest_sha256", "root_runtime_observation", "root_transport_observation"},
        "edge_checkpoint_v1": {"lane", "frontier", "applications", "sequences", "pending_publications", "batch", "dispositions", "snapshot", "public_current", "producer_generation_sha256", "hub_generation_sha256", "ack_census"},
        "edge_http_v1": {"role", "transcript", "tls", "transport_failures", "census"},
        "edge_fault_v1": {"point", "witness", "witness_text", "process", "before", "after", "delivery_before", "delivery_after", "publication_witness"},
        "edge_snapshot_v1": {"lane", "store_id", "store_schema_version", "selector_sha256", "current", "retained_observations", "lifecycle", "accumulator_jcs", "applications", "dispositions", "materialised_states", "open_rows", "sync_mutations", "counts"},
        "edge_cleanup_v1": {"resource_id", "acquired", "stop", "current_absence", "retention", "status", "errors"},
    }
    for schema_id in allowed:
        if set(payload) == candidates.get(schema_id):
            return schema_id
    raise LauncherError("worker payload does not match a reviewed raw schema")


def _raw_binding(value: Any, label: str) -> None:
    record = _exact(value, {"path", "sha256"}, label)
    _path(record["path"], f"{label}.path")
    _digest(record["sha256"], f"{label}.sha256")


def _raw_integer(value: Any, label: str, *, maximum: int | None = None) -> None:
    if type(value) is not int or value < 0 or (maximum is not None and value > maximum):
        raise LauncherError(f"{label} integer invalid")


def _validate_raw_payload(schema_id: str, payload: Mapping[str, Any]) -> None:
    if schema_id == "edge_fixture_v1":
        for key in ("seal_sha256", "http_profile_sha256", "edge_profile_sha256", "primary_context_sha256", "custody_manifest_sha256"):
            _digest(payload[key], f"raw.{key}")
        if type(payload["baseline_clone_equal"]) is not bool:
            raise LauncherError("raw.baseline_clone_equal invalid")
        _raw_binding(payload["root_runtime_observation"], "raw.root_runtime_observation")
        _raw_binding(payload["root_transport_observation"], "raw.root_transport_observation")
    elif schema_id == "edge_checkpoint_v1":
        if payload["lane"] not in {"primary", "reference", "fault", "negative"}:
            raise LauncherError("raw checkpoint lane invalid")
        if payload["frontier"] is not None:
            _raw_integer(payload["frontier"], "raw.frontier", maximum=10)
        for key in ("applications", "sequences", "pending_publications"):
            _raw_integer(payload[key], f"raw.{key}", maximum=10)
        for key in ("batch", "dispositions", "snapshot", "ack_census"):
            _raw_binding(payload[key], f"raw.{key}")
        if payload["public_current"] is not None:
            _raw_binding(payload["public_current"], "raw.public_current")
        _digest(payload["producer_generation_sha256"], "raw.producer_generation_sha256")
        _digest(payload["hub_generation_sha256"], "raw.hub_generation_sha256")
    elif schema_id == "edge_http_v1":
        if payload["role"] not in {"hub_public", "edge_delivery", "negative_responder"}:
            raise LauncherError("raw HTTP role invalid")
        transcript = payload["transcript"]
        if not isinstance(transcript, list) or len(transcript) > 128:
            raise LauncherError("raw HTTP transcript invalid")
        request_ids: set[str] = set()
        for item in transcript:
            item = _exact(item, {"request_id", "method", "route", "status", "request_body_sha256", "response_body"}, "raw HTTP transcript item")
            _token(item["request_id"], "raw HTTP request_id")
            if item["request_id"] in request_ids or item["method"] not in {"GET", "POST"} or not isinstance(item["route"], str) or not item["route"].startswith("/"):
                raise LauncherError("raw HTTP request identity invalid")
            request_ids.add(item["request_id"])
            _raw_integer(item["status"], "raw HTTP status")
            if not 100 <= item["status"] <= 599:
                raise LauncherError("raw HTTP status outside range")
            if item["request_body_sha256"] is not None:
                _digest(item["request_body_sha256"], "raw HTTP request body")
            _raw_binding(item["response_body"], "raw HTTP response body")
        tls = _exact(payload["tls"], {"server_der_sha256", "client_der_sha256", "chain_verified", "hostname_verified"}, "raw HTTP TLS")
        _digest(tls["server_der_sha256"], "raw HTTP server DER")
        if tls["client_der_sha256"] is not None:
            _digest(tls["client_der_sha256"], "raw HTTP client DER")
        if type(tls["chain_verified"]) is not bool or type(tls["hostname_verified"]) is not bool:
            raise LauncherError("raw HTTP TLS verification invalid")
        failures = payload["transport_failures"]
        if not isinstance(failures, list) or len(failures) > 32:
            raise LauncherError("raw HTTP transport failures invalid")
        for item in failures:
            item = _exact(item, {"request_id", "phase", "outcome"}, "raw HTTP transport failure")
            _token(item["request_id"], "raw HTTP failure request_id")
            if item["phase"] not in {"handshake", "connect", "body"} or item["outcome"] not in {"rejected", "aborted"}:
                raise LauncherError("raw HTTP transport failure identity invalid")
        census = _exact(payload["census"], {"v2_gets", "v2_acks", "v1_requests", "redirect_requests"}, "raw HTTP census")
        for key in census:
            _raw_integer(census[key], f"raw HTTP census.{key}")
    elif schema_id == "edge_fault_v1":
        if payload["point"] not in {"raw_insert", "lifecycle_write", "receipt_insert", "frontier_update", "commit", "before_commit", "after_commit", "after_ack_accepted_before_response"}:
            raise LauncherError("raw fault point invalid")
        if not isinstance(payload["witness_text"], str) or not 0 < len(payload["witness_text"] ) <= 128:
            raise LauncherError("raw fault witness text invalid")
        for key in ("witness", "process", "before", "after", "delivery_before", "delivery_after"):
            _raw_binding(payload[key], f"raw fault.{key}")
        if payload["publication_witness"] is not None:
            _raw_binding(payload["publication_witness"], "raw fault.publication_witness")
    elif schema_id == "edge_snapshot_v1":
        if payload["lane"] not in {"primary", "reference", "fault", "negative"}:
            raise LauncherError("raw snapshot lane invalid")
        _uuid(payload["store_id"], "raw snapshot.store_id")
        if payload["store_schema_version"] != 59:
            raise LauncherError("raw snapshot schema version invalid")
        _digest(payload["selector_sha256"], "raw snapshot.selector_sha256")
        for key in ("current", "retained_observations", "applications", "dispositions", "materialised_states", "open_rows", "sync_mutations"):
            if not isinstance(payload[key], list) or len(payload[key]) > 256:
                raise LauncherError(f"raw snapshot.{key} invalid")
        if not isinstance(payload["accumulator_jcs"], str) or len(payload["accumulator_jcs"].encode()) > 1_048_576:
            raise LauncherError("raw snapshot.accumulator_jcs invalid")
        if payload["lifecycle"] is not None and not isinstance(payload["lifecycle"], Mapping):
            raise LauncherError("raw snapshot.lifecycle invalid")
        counts = _exact(payload["counts"], {"applications", "sequences", "pending_publications", "ack_frontier"}, "raw snapshot counts")
        for key in ("applications", "sequences", "pending_publications"):
            _raw_integer(counts[key], f"raw snapshot counts.{key}")
        if counts["ack_frontier"] is not None:
            _raw_integer(counts["ack_frontier"], "raw snapshot counts.ack_frontier")
    elif schema_id == "edge_cleanup_v1":
        _token(payload["resource_id"], "raw cleanup.resource_id")
        for key in ("acquired", "stop", "current_absence", "retention"):
            _raw_binding(payload[key], f"raw cleanup.{key}")
        if payload["status"] not in {"clean", "failed"} or not isinstance(payload["errors"], list) or len(payload["errors"]) > 32 or not all(isinstance(item, str) and TOKEN.fullmatch(item) for item in payload["errors"]):
            raise LauncherError("raw cleanup status invalid")


def _read_phase(session: Session, phase: Mapping[str, Any], schema_id: str, deadline: float) -> tuple[Mapping[str, Any], str, Path, str]:
    actor_id = phase["actor_id"]
    ordinal = phase["ordinal"]
    if schema_id not in tuple(phase["raw_schema_ids"]):
        raise LauncherError(f"Edge phase {phase['phase_id']} schema is not registered")
    path = session.coordination_dir / "actors" / actor_id / f"phase-{ordinal:06d}-{schema_id}.json"
    raw = _wait_read(path, deadline=deadline, label=f"Edge phase {phase['phase_id']} {schema_id}", maximum=MAX_EVIDENCE_BYTES)
    value = _strict_json(raw, f"Edge phase {phase['phase_id']} {schema_id}")
    _exact(value, {"schema_version", "session_id", "cell_id", "instance_nonce", "session_input_sha256", "actor_id", "invocation_id", "stage", "observation", "payload"}, f"Edge phase {phase['phase_id']}")
    if value["schema_version"] != 1 or value["session_id"] != session.session_id or value["cell_id"] != session.cell_id or value["instance_nonce"] != session.instance_nonce or value["session_input_sha256"] != session.digest or value["actor_id"] != actor_id or value["invocation_id"] != f"edge-phase-{ordinal:06d}" or value["stage"] != phase["phase_id"]:
        raise LauncherError(f"Edge phase {phase['phase_id']} identity mismatch")
    observation = _exact(value["observation"], {"session_sequence", "proof_sha256"}, f"Edge phase {phase['phase_id']}.observation")
    if type(observation["session_sequence"]) is not int or observation["session_sequence"] <= 0:
        raise LauncherError(f"Edge phase {phase['phase_id']} sequence invalid")
    _digest(observation["proof_sha256"], f"Edge phase {phase['phase_id']}.proof_sha256")
    payload = value["payload"]
    if not isinstance(payload, Mapping):
        raise LauncherError(f"Edge phase {phase['phase_id']} payload invalid")
    actual_schema_id = _raw_schema(payload, (schema_id,))
    _validate_raw_payload(actual_schema_id, payload)
    return value, actual_schema_id, path, _sha256(raw)


def _request_transcript(raw: Mapping[str, Any]) -> list[dict[str, Any]]:
    payload = raw["payload"]
    if set(payload) != {"role", "transcript", "tls", "transport_failures", "census"}:
        return []
    result: list[dict[str, Any]] = []
    for item in payload["transcript"]:
        if not isinstance(item, Mapping) or set(item) != {"request_id", "method", "route", "status", "request_body_sha256", "response_body"}:
            raise LauncherError("Edge HTTP transcript shape invalid")
        result.append({"request_id": item["request_id"], "method": item["method"], "route": item["route"], "status": item["status"]})
    return result


def _facts(case_id: str, session: Session) -> dict[str, Any]:
    namespace: dict[str, Any] = {}
    source = _read_public(CONTRACT_DIR / "matrix_contract.py", label="Edge validator", maximum=1_048_576).decode("utf-8")
    exec(compile(source, str(CONTRACT_DIR / "matrix_contract.py"), "exec"), namespace)
    if case_id == "candidate_artifact_identity":
        edge = next(item for item in session.value["inputs"]["product_inputs"] if item["artifact_role"] == "edge_executable")
        return {"edge_sha256": edge["staged"]["local"]["sha256"], "product_version": PRODUCT_VERSION, "storage_format": 3, "normal_fault_distinct": True}
    if case_id == "installed_service_runtime":
        mode = session.header["runtime"]["hub"].get("service_mode")
        return {"service_mode": mode}
    if case_id == "edge_linux_runtime":
        # The header's closed runtime record describes the Hub and the root
        # coordinator only; it deliberately has no worker-defined `edge`
        # member.  SessionInput.cell_id is the root-owned binding for the
        # reviewed edge_producer target, so derive its architecture there.
        try:
            architecture = EDGE_LINUX_ARCHITECTURE_BY_CELL[session.cell_id]
        except KeyError as error:
            raise LauncherError("Edge producer cell is not registered") from error
        return {"process_bound": True, "namespace_bound": True, "architecture": architecture}
    return dict(namespace["STATIC_FACTS"].get(case_id, {}))


def _make_outputs(session: Session, phase_values: Mapping[str, Mapping[str, tuple[Mapping[str, Any], str, Path, str]]], controller_observations: Mapping[int, Mapping[str, Any]]) -> tuple[dict[str, Any], dict[str, Any], Mapping[int, Mapping[str, Any]]]:
    cases: list[dict[str, Any]] = []
    invocations: list[dict[str, Any]] = []
    raw_entries: list[dict[str, Any]] = []
    raw_actors: dict[str, set[str]] = {}
    observations: dict[int, Mapping[str, Any]] = dict(controller_observations)
    for phase_id, schema_values in phase_values.items():
        for value, schema_id, path_value, raw_hash in schema_values.values():
            evidence_id = f"edge-raw-{phase_id}-{schema_id}"
            if not any(item["id"] == evidence_id for item in raw_entries):
                raw_entries.append({"id": evidence_id, "schema_id": schema_id, "binding": {"path": str(path_value), "sha256": raw_hash}})
            raw_actors.setdefault(evidence_id, set()).add(value["actor_id"])
    for case_id in REQUIRED_CASES:
        expected = _facts(case_id, session)
        status = "pending" if case_id == "installed_service_runtime" else "passed"
        request_transcript: list[dict[str, Any]] = []
        for index, (phase_id, operation, actor_id) in enumerate(CASE_PHASES[case_id]):
            schema_ids = _case_raw_schema_ids(case_id)
            schema_id = next((candidate for candidate in schema_ids if candidate in phase_values[phase_id]), None)
            if schema_id is None:
                raise LauncherError(f"case {case_id} has no raw witness at phase {phase_id}")
            value, schema_id, path_value, raw_hash = phase_values[phase_id][schema_id]
            evidence_id = f"edge-raw-{phase_id}-{schema_id}"
            raw_actors.setdefault(evidence_id, set()).update((value["actor_id"], actor_id))
            sequence = value["observation"]["session_sequence"]
            invocation = {"id": f"{case_id}-{index + 1}", "case_id": case_id, "actor_id": actor_id, "operation": operation, "session_sequence_before": sequence, "session_sequence_after": sequence, "evidence_id": evidence_id, "request_ids": [item["request_id"] for item in _request_transcript(value)]}
            invocations.append(invocation)
            request_transcript.extend(_request_transcript(value))
        cases.append({"id": case_id, "status": status, "expected": expected, "actual": expected, "evidence_kind": "identity" if case_id in {"candidate_artifact_identity", "installed_service_runtime", "edge_linux_runtime", "edge_source_owner_exclusivity", "edge_clean_shutdown_resume"} else "http", "request_transcript": request_transcript})
    actors: list[dict[str, Any]] = []
    for actor_id in ACTOR_IDS:
        actor = next(item for item in session.value["actors"] if item["id"] == actor_id)
        actors.append({"id": actor_id, "kind": actor["kind"], "runtime_ref": actor["runtime_ref"], "entrypoint_ref": actor["entrypoint_ref"], "artifact_roles": actor["artifact_roles"], "source_roles": actor["source_roles"], "installed_manifest": actor["input_manifest"]["local"], "raw_evidence": [item for item in raw_entries if actor_id in raw_actors.get(item["id"], set())]})
    normalized = dict(session.header)
    normalized["cases"] = cases
    actor_evidence = {"schema_version": 1, "session_id": session.session_id, "cell_id": session.cell_id, "session_input_sha256": session.digest, "actors": actors, "invocations": invocations}
    return normalized, actor_evidence, observations


def _read_ack(session: Session, ready_raw: bytes, deadline: float) -> Mapping[str, Any]:
    path = session.coordination_dir / "ack-000001.json"
    raw = _wait_read(path, deadline=deadline, label="Edge acknowledgement", maximum=MAX_READY_BYTES)
    value = _strict_json(raw, "Edge acknowledgement")
    _exact(value, {"schema_version", "type", "session_id", "cell_id", "session_input_sha256", "instance_nonce", "sequence", "ready_sha256", "phase", "status", "action", "result"}, "Edge acknowledgement")
    if value["schema_version"] != 1 or value["type"] != "ack" or value["session_id"] != session.session_id or value["cell_id"] != session.cell_id or value["session_input_sha256"] != session.digest or value["instance_nonce"] != session.instance_nonce or value["sequence"] != 1 or value["ready_sha256"] != _sha256(ready_raw) or value["phase"] != "evidence_ready" or value["status"] != "accepted" or value["action"] != "close_completed":
        raise LauncherError("Edge acknowledgement identity invalid")
    result = _exact(value["result"], {"path", "sha256"}, "Edge acknowledgement.result")
    result_path, result_raw = _binding(result, "Edge acknowledgement.result", maximum=MAX_READY_BYTES)
    close = _strict_json(result_raw, "Edge close evidence")
    _exact(close, {"schema_version", "session_id", "state", "journal", "final_stopped", "cleanup_errors", "local_transport"}, "Edge close evidence")
    if close["schema_version"] != 1 or close["session_id"] != session.session_id or close["state"] != "closed" or close["cleanup_errors"] != []:
        raise LauncherError("Edge close evidence is not clean")
    for key in ("journal", "final_stopped", "local_transport"):
        if not isinstance(close[key], Mapping):
            raise LauncherError(f"Edge close evidence {key} invalid")
    return value


def run(session_path: str | Path) -> int:
    session = load_session(session_path)
    started = time.monotonic()
    deadline = started + session.value["bounds"]["cell_timeout_ms"] / 1000.0
    session.coordination_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    metadata = session.coordination_dir.stat()
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
        raise LauncherError("Edge coordination directory is not owner-only")
    for output_key in ("normalized", "actor_evidence", "framework_log"):
        output = Path(session.value["outputs"][output_key])
        if output.exists() or output.is_symlink():
            raise LauncherError(f"Edge output {output_key} is not fresh")
    for name in ("adapter-completion.json", "ready-000001.json", "ack-000001.json"):
        path = session.coordination_dir / name
        if path.exists() or path.is_symlink():
            raise LauncherError(f"Edge coordination output {name} is not fresh")
    broker = Broker(session)
    phase_values: dict[str, dict[str, tuple[Mapping[str, Any], str, Path, str]]] = {}
    controller_observations: dict[int, Mapping[str, Any]] = {}
    try:
        broker.open(timeout=min(10.0, max(1.0, deadline - time.monotonic())))
        # The operation order is fixed in the reviewed phase schedule.  The
        # worker file is consumed only after its recipe has run.
        for phase in sorted(session.phases.values(), key=lambda item: item["ordinal"]):
            if time.monotonic() >= deadline:
                raise LauncherError("Edge cell deadline expired")
            for operation in (next(item[4] for item in _load_phase_schedule() if item[1] == phase["phase_id"])):
                result = broker.request(operation, timeout=min(160.0, max(1.0, deadline - time.monotonic())))
                if operation != "stop":
                    proof = result["proof"]
                    controller_observations[proof["sequence"]] = {"status": "observed", "proof_sha256": _sha256(_canonical_json(proof))}
            phase_values[phase["phase_id"]] = {
                schema_id: _read_phase(session, phase, schema_id, deadline)
                for schema_id in phase["raw_schema_ids"]
            }
            for value, _schema, _path_value, _raw_hash in phase_values[phase["phase_id"]].values():
                sequence = value["observation"]["session_sequence"]
                actual = controller_observations.get(sequence)
                if actual is None or actual["proof_sha256"] != value["observation"]["proof_sha256"]:
                    raise LauncherError(f"Edge phase {phase['phase_id']} observation is not controller-bound")
        normalized, actor_evidence, observations = _make_outputs(session, phase_values, controller_observations)
        normalized_path, normalized_hash = _write_exclusive(Path(session.value["outputs"]["normalized"]), normalized)
        actor_path, actor_hash = _write_exclusive(Path(session.value["outputs"]["actor_evidence"]), actor_evidence)
        completion = {"schema_version": 1, "session_id": session.session_id, "cell_id": session.cell_id, "session_input_sha256": session.digest, "normalized": {"path": str(normalized_path), "sha256": normalized_hash}, "actor_evidence": {"path": str(actor_path), "sha256": actor_hash}}
        completion_path, completion_hash = _write_exclusive(session.coordination_dir / "adapter-completion.json", completion)
        observation_sequence = max(observations)
        observation = {"session_sequence": observation_sequence, "proof_sha256": observations[observation_sequence]["proof_sha256"]}
        ready = {"schema_version": 1, "type": "ready", "session_id": session.session_id, "cell_id": session.cell_id, "session_input_sha256": session.digest, "instance_nonce": session.instance_nonce, "sequence": 1, "phase": "evidence_ready", "observation": observation, "evidence": {"path": str(completion_path), "sha256": completion_hash}}
        ready_path, _ready_hash = _write_exclusive(session.coordination_dir / "ready-000001.json", ready, maximum=MAX_READY_BYTES)
        ready_raw = _read_owned(ready_path, label="Edge ready", maximum=MAX_READY_BYTES)
        _read_ack(session, ready_raw, deadline)
        broker.close()
        return 0
    except Exception as error:
        broker.close()
        try:
            _write_exclusive(Path(session.value["outputs"]["framework_log"]), {"status": "failed", "error": str(error)[:4096]}, maximum=session.value["bounds"]["framework_log_bytes"])
        except Exception:
            pass
        print(f"edge matrix coordinator: {error}", file=sys.stderr)
        return 1


def _load_phase_schedule() -> tuple[tuple[str, str, int, str, tuple[str, ...]], ...]:
    namespace: dict[str, Any] = {}
    source = _read_public(CONTRACT_DIR / "matrix_contract.py", label="Edge validator", maximum=1_048_576).decode("utf-8")
    exec(compile(source, str(CONTRACT_DIR / "matrix_contract.py"), "exec"), namespace)
    return tuple(namespace["PHASE_SCHEDULE"])


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1:
        print("usage: edge.py /absolute/session-input.json", file=sys.stderr)
        return 2
    try:
        return run(args[0])
    except Exception as error:
        print(f"edge matrix coordinator: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
