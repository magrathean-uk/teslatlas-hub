"""Strict private wire primitives for the installed HA matrix coordinator."""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import select
import ssl
import stat
import time
import uuid
from contextlib import suppress
from pathlib import Path, PurePosixPath

MAX_FRAME_BYTES = 1_048_576
MAX_INPUT_BYTES = 1_048_576
MAX_CERTIFICATE_BYTES = 65_536
MAX_EVIDENCE_BYTES = 8_388_608
OPERATIONS = frozenset({"verify", "stop", "start", "pair", "revoke"})
DEADLINES = {
    "verify": 30.0,
    "stop": 60.0,
    "start": 60.0,
    "pair": 160.0,
    "revoke": 160.0,
}
TOKEN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
HA_CELLS = frozenset(
    {
        "home_assistant__macos_arm64",
        "home_assistant__debian13_arm64",
        "home_assistant__debian13_amd64",
    }
)
PROFILE_MEMBERS = (
    "SHA256SUMS",
    "auth.schema.json",
    "cases.json",
    "discovery.schema.json",
    "errors.schema.json",
    "examples/claim.json",
    "examples/current.json",
    "examples/discovery.json",
    "examples/drives.json",
    "examples/health.json",
    "examples/invitation.json",
    "examples/ready.json",
    "examples/vehicles.json",
    "field-semantics.json",
    "openapi.json",
    "profile.json",
    "resources.schema.json",
    "sync-regression.json",
)


class MatrixWireError(Exception):
    """One fixed-label input, framing, or coordination failure."""


class BrokerOperationError(MatrixWireError):
    """The installed-host broker rejected one admitted operation."""

    def __init__(self, code: str) -> None:
        self.code = code
        super().__init__("installed-host broker operation failed")


def canonical_json_bytes(value: object) -> bytes:
    """Return the contract's canonical UTF-8 JSON encoding."""
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")


def _unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    value: dict[str, object] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate JSON member")
        value[key] = item
    return value


def _reject_constant(_value: str) -> None:
    raise ValueError("non-finite JSON number")


def _finite_float(value: str) -> float:
    result = float(value)
    if not math.isfinite(result):
        raise ValueError("overflowing JSON number")
    return result


def strict_json(raw: bytes) -> object:
    """Decode one strict UTF-8 JSON value with unique object members."""
    return json.loads(
        raw.decode("utf-8", "strict"),
        object_pairs_hook=_unique_object,
        parse_constant=_reject_constant,
        parse_float=_finite_float,
    )


def _exact(value: object, keys: set[str], label: str) -> dict:
    if not isinstance(value, dict) or set(value) != keys:
        raise MatrixWireError(f"{label} shape is invalid")
    return value


def _token(value: object, label: str) -> str:
    if not isinstance(value, str) or TOKEN.fullmatch(value) is None:
        raise MatrixWireError(f"{label} is invalid")
    return value


def _digest(value: object, label: str) -> str:
    if not isinstance(value, str) or HEX64.fullmatch(value) is None:
        raise MatrixWireError(f"{label} is invalid")
    return value


def _session_id(value: object, label: str = "session_id") -> str:
    try:
        parsed = uuid.UUID(value) if isinstance(value, str) else None
    except ValueError as error:
        raise MatrixWireError(f"{label} is invalid") from error
    if parsed is None or parsed.version != 4 or str(parsed) != value:
        raise MatrixWireError(f"{label} is invalid")
    return value


def _absolute(value: object, label: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or "\x00" in value
        or "\n" in value
        or "\r" in value
    ):
        raise MatrixWireError(f"{label} is invalid")
    pure = PurePosixPath(value)
    if not pure.is_absolute() or str(pure) != value or ".." in pure.parts:
        raise MatrixWireError(f"{label} is invalid")
    return value


def _read_regular_private(path: str, maximum: int, label: str) -> bytes:
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError as error:
        raise MatrixWireError(f"{label} is unavailable") from error
    try:
        metadata = os.fstat(descriptor)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or metadata.st_mode & 0o077
        ):
            raise MatrixWireError(f"{label} ownership or mode is invalid")
        chunks = bytearray()
        while len(chunks) <= maximum:
            part = os.read(descriptor, min(65_536, maximum + 1 - len(chunks)))
            if not part:
                break
            chunks.extend(part)
        if len(chunks) > maximum:
            raise MatrixWireError(f"{label} exceeds byte limit")
        return bytes(chunks)
    finally:
        os.close(descriptor)


def _file_binding(value: object, label: str) -> dict:
    binding = _exact(value, {"path", "sha256"}, label)
    _absolute(binding["path"], f"{label} path")
    _digest(binding["sha256"], f"{label} digest")
    return binding


def file_binding(
    path: str | os.PathLike[str], maximum: int = MAX_EVIDENCE_BYTES
) -> dict:
    """Bind one existing private regular file by its exact bytes."""
    absolute = str(Path(path).resolve(strict=True))
    raw = _read_regular_private(absolute, maximum, "evidence file")
    return {"path": absolute, "sha256": hashlib.sha256(raw).hexdigest()}


def write_exclusive_json(path: str | os.PathLike[str], value: object) -> dict:
    """Write one owner-only canonical JSON file, flush it, and bind its bytes."""
    target = Path(path)
    raw = canonical_json_bytes(value) + b"\n"
    try:
        descriptor = os.open(
            target,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
            0o600,
        )
    except OSError as error:
        raise MatrixWireError("exclusive evidence output is unavailable") from error
    try:
        offset = 0
        while offset < len(raw):
            written = os.write(descriptor, raw[offset:])
            if written <= 0:
                raise MatrixWireError("exclusive evidence output write failed")
            offset += written
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    directory = os.open(target.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
    return {"path": str(target.resolve()), "sha256": hashlib.sha256(raw).hexdigest()}


def _staged_file(
    value: object,
    label: str,
    *,
    maximum: int = MAX_INPUT_BYTES,
) -> tuple[dict, bytes]:
    staged = _exact(value, {"id", "root", "local"}, label)
    _token(staged["id"], f"{label} id")
    root = _file_binding(staged["root"], f"{label} root")
    local = _file_binding(staged["local"], f"{label} local")
    if root["sha256"] != local["sha256"]:
        raise MatrixWireError(f"{label} staged digests differ")
    raw = _read_regular_private(local["path"], maximum, f"{label} local file")
    if hashlib.sha256(raw).hexdigest() != local["sha256"]:
        raise MatrixWireError(f"{label} local digest mismatch")
    return staged, raw


def read_staged_json(
    staged: dict, *, maximum: int = MAX_INPUT_BYTES, label: str = "staged JSON"
) -> dict:
    """Read one mapped local JSON file while leaving its root path untouched."""
    _value, raw = _staged_file(staged, label, maximum=maximum)
    try:
        value = strict_json(raw)
    except (UnicodeError, ValueError, json.JSONDecodeError) as error:
        raise MatrixWireError(f"{label} is not strict JSON") from error
    if not isinstance(value, dict):
        raise MatrixWireError(f"{label} is not an object")
    return value


def _host_session(value: object, expected_session_id: str) -> dict:
    host = _exact(
        value,
        {
            "schema_version",
            "kind",
            "broker_socket",
            "session_id",
            "registration_sha256",
        },
        "host_session",
    )
    if type(host["schema_version"]) is not int or host["schema_version"] != 1:
        raise MatrixWireError("host_session schema_version is invalid")
    if host["kind"] != "installed-host":
        raise MatrixWireError("host_session kind is invalid")
    _absolute(host["broker_socket"], "host_session broker_socket")
    host_session_id = _session_id(host["session_id"], "host_session session_id")
    if host_session_id != expected_session_id:
        raise MatrixWireError("host_session session_id mismatch")
    _digest(host["registration_sha256"], "host_session registration_sha256")
    return host


def _actor(value: object) -> dict:
    actor = _exact(
        value,
        {
            "id",
            "kind",
            "execution",
            "runtime_ref",
            "artifact_roles",
            "source_roles",
            "entrypoint_ref",
            "input_manifest",
            "phase_contract",
        },
        "actor",
    )
    expected = {
        "ha_flow": ("installed_ha_flow", "ha_matrix_flow"),
        "ha_client": ("installed_ha_client", "ha_matrix_client"),
    }
    if actor["id"] not in expected:
        raise MatrixWireError("actor id is invalid")
    kind, entrypoint = expected[actor["id"]]
    if (
        actor["kind"] != kind
        or actor["execution"] != "coordinator"
        or actor["runtime_ref"] != "ha_container"
        or actor["entrypoint_ref"] != entrypoint
        or actor["artifact_roles"] != ["home_assistant_integration_archive"]
        or actor["source_roles"] != ["home_assistant_source"]
        or actor["phase_contract"] is not None
    ):
        raise MatrixWireError("actor contract is invalid")
    _staged_file(actor["input_manifest"], "actor input manifest")
    return actor


def load_session_input(path: str | os.PathLike[str]) -> dict:
    """Read and validate one closed Docker HA SessionInput.

    Staged root bindings are identity-only; this function opens only each
    explicitly mapped local file.
    """
    raw = _read_regular_private(str(path), MAX_INPUT_BYTES, "session input")
    try:
        value = strict_json(raw)
    except (UnicodeError, ValueError, json.JSONDecodeError) as error:
        raise MatrixWireError("session input is not strict JSON") from error
    session = _exact(
        value,
        {
            "schema_version",
            "kind",
            "run_id",
            "cell_id",
            "adapter_id",
            "client_id",
            "session_id",
            "instance_nonce",
            "header",
            "case_contract",
            "host_session",
            "broker",
            "inputs",
            "actors",
            "outputs",
            "bounds",
        },
        "session input",
    )
    if type(session["schema_version"]) is not int or session["schema_version"] != 1:
        raise MatrixWireError("session input schema_version is invalid")
    if session["kind"] != "matrix-adapter-session":
        raise MatrixWireError("session input kind is invalid")
    _token(session["run_id"], "run_id")
    if session["cell_id"] not in HA_CELLS:
        raise MatrixWireError("cell_id is invalid")
    if (
        session["adapter_id"] != "home_assistant"
        or session["client_id"] != "home_assistant"
    ):
        raise MatrixWireError("adapter identity is invalid")
    session_id = _session_id(session["session_id"])
    _digest(session["instance_nonce"], "instance_nonce")
    local_inputs: set[str] = set()

    def remember_local(staged: dict) -> None:
        local_path = staged["local"]["path"]
        if local_path in local_inputs:
            raise MatrixWireError("staged input path is duplicated")
        local_inputs.add(local_path)

    header: dict | None = None
    for key in ("header", "case_contract"):
        staged, staged_raw = _staged_file(session[key], key)
        remember_local(staged)
        if key == "header":
            try:
                parsed_header = strict_json(staged_raw)
            except (UnicodeError, ValueError, json.JSONDecodeError) as error:
                raise MatrixWireError("evidence header is not strict JSON") from error
            if not isinstance(parsed_header, dict):
                raise MatrixWireError("evidence header is invalid")
            header = parsed_header
    _host_session(session["host_session"], session_id)
    broker = _exact(session["broker"], {"kind", "socket_path"}, "broker")
    if broker != {"kind": "stdio", "socket_path": None}:
        raise MatrixWireError("HA broker must use stdio")
    inputs = _exact(
        session["inputs"],
        {
            "profile_manifest",
            "profile_members",
            "scenario",
            "certificate",
            "certificate_der_sha256",
            "product_inputs",
        },
        "inputs",
    )
    manifest_staged, profile_raw = _staged_file(
        inputs["profile_manifest"], "profile_manifest"
    )
    staged, _scenario_raw = _staged_file(inputs["scenario"], "scenario")
    remember_local(staged)
    staged, certificate_raw = _staged_file(
        inputs["certificate"], "certificate", maximum=MAX_CERTIFICATE_BYTES
    )
    remember_local(staged)
    certificate_digest = _digest(
        inputs["certificate_der_sha256"], "certificate DER digest"
    )
    try:
        pem = certificate_raw.decode("ascii", "strict")
        der = ssl.PEM_cert_to_DER_cert(pem)
    except (UnicodeError, ValueError) as error:
        raise MatrixWireError("certificate PEM is invalid") from error
    if hashlib.sha256(der).hexdigest() != certificate_digest:
        raise MatrixWireError("certificate DER digest mismatch")
    members = inputs["profile_members"]
    if not isinstance(members, list) or len(members) != 18:
        raise MatrixWireError("profile members must contain exactly 18 files")
    member_ids: set[str] = set()
    member_paths: dict[str, tuple[dict, bytes]] = {}
    profile_roots: set[PurePosixPath] = set()
    local_profile_roots: set[PurePosixPath] = set()
    for member in members:
        staged, member_raw = _staged_file(member, "profile member")
        if staged["id"] in member_ids:
            raise MatrixWireError("profile member id is duplicated")
        member_ids.add(staged["id"])
        local_path = staged["local"]["path"]
        if local_path in local_inputs:
            raise MatrixWireError("staged input path is duplicated")
        root_path = PurePosixPath(staged["root"]["path"])
        matching = [
            name
            for name in PROFILE_MEMBERS
            if tuple(root_path.parts[-len(PurePosixPath(name).parts) :])
            == PurePosixPath(name).parts
        ]
        if len(matching) != 1 or matching[0] in member_paths:
            raise MatrixWireError("profile member layout is invalid")
        name = matching[0]
        member_parts = PurePosixPath(name).parts
        profile_roots.add(PurePosixPath(*root_path.parts[: -len(member_parts)]))
        local_member = PurePosixPath(local_path)
        if tuple(local_member.parts[-len(member_parts) :]) != member_parts:
            raise MatrixWireError("profile member local layout is invalid")
        local_profile_roots.add(
            PurePosixPath(*local_member.parts[: -len(member_parts)])
        )
        member_paths[name] = (staged, member_raw)
        local_inputs.add(local_path)
    if (
        set(member_paths) != set(PROFILE_MEMBERS)
        or len(profile_roots) != 1
        or len(local_profile_roots) != 1
        or next(iter(profile_roots)).name != "1.0.0"
        or next(iter(profile_roots)).parent.name != "hub-http-v1"
        or next(iter(local_profile_roots)).name != "1.0.0"
        or next(iter(local_profile_roots)).parent.name != "hub-http-v1"
    ):
        raise MatrixWireError("profile member set is invalid")
    checksum_staged, checksum_raw = member_paths["SHA256SUMS"]
    if (
        manifest_staged["root"] != checksum_staged["root"]
        or manifest_staged["local"] != checksum_staged["local"]
        or profile_raw != checksum_raw
    ):
        raise MatrixWireError("profile manifest differs from SHA256SUMS member")
    if header is None or header.get("profile_sha256") != manifest_staged["local"][
        "sha256"
    ]:
        raise MatrixWireError("profile manifest differs from evidence header")
    try:
        checksum_lines = checksum_raw.decode("ascii", "strict").splitlines()
    except UnicodeError as error:
        raise MatrixWireError("profile checksums are invalid") from error
    expected_checksum_members = set(PROFILE_MEMBERS) - {"SHA256SUMS"}
    checksums: dict[str, str] = {}
    for line in checksum_lines:
        parts = line.split("  ")
        if len(parts) != 2 or HEX64.fullmatch(parts[0]) is None:
            raise MatrixWireError("profile checksums are invalid")
        if parts[1] in checksums:
            raise MatrixWireError("profile checksums are duplicated")
        checksums[parts[1]] = parts[0]
    if set(checksums) != expected_checksum_members:
        raise MatrixWireError("profile checksum member set is invalid")
    for name, digest in checksums.items():
        if hashlib.sha256(member_paths[name][1]).hexdigest() != digest:
            raise MatrixWireError("profile member checksum mismatch")
    try:
        profile = strict_json(member_paths["profile.json"][1])
    except (UnicodeError, ValueError, json.JSONDecodeError) as error:
        raise MatrixWireError("profile.json is not strict JSON") from error
    if (
        not isinstance(profile, dict)
        or profile.get("profile_id") != "hub-http-v1@1.0.0"
        or profile.get("contract_version") != "1.0.0"
        or profile.get("schema_dialect")
        != "https://json-schema.org/draft/2020-12/schema"
        or profile.get("status") != "candidate"
    ):
        raise MatrixWireError("profile.json identity is invalid")
    products = inputs["product_inputs"]
    if not isinstance(products, list) or len(products) != 1:
        raise MatrixWireError("HA product input is invalid")
    product = _exact(
        products[0],
        {"artifact_role", "staged", "installed_manifest", "local_root"},
        "product input",
    )
    if product["artifact_role"] != "home_assistant_integration_archive":
        raise MatrixWireError("HA product role is invalid")
    staged, _raw = _staged_file(product["staged"], "HA product archive")
    remember_local(staged)
    staged, _raw = _staged_file(product["installed_manifest"], "HA installed manifest")
    remember_local(staged)
    local_root = _absolute(product["local_root"], "HA installed root")
    root_metadata = os.stat(local_root, follow_symlinks=False)
    if not stat.S_ISDIR(root_metadata.st_mode):
        raise MatrixWireError("HA installed root is not a directory")
    actors = session["actors"]
    actor_ids = [
        item.get("id") for item in actors if isinstance(item, dict)
    ] if isinstance(actors, list) else []
    if not isinstance(actors, list) or actor_ids != ["ha_flow", "ha_client"]:
        raise MatrixWireError("HA actor set is invalid")
    for item in actors:
        actor = _actor(item)
        remember_local(actor["input_manifest"])
    outputs = _exact(
        session["outputs"],
        {"normalized", "actor_evidence", "coordination_dir", "framework_log"},
        "outputs",
    )
    output_paths = [_absolute(outputs[key], f"outputs {key}") for key in outputs]
    if len(set(output_paths)) != len(output_paths):
        raise MatrixWireError("output paths are duplicated")
    for output in output_paths:
        if output in local_inputs:
            raise MatrixWireError("output path aliases an input")
        if Path(output).exists():
            raise MatrixWireError("output path already exists")
    output_root = Path(outputs["normalized"]).parent
    if (
        Path(outputs["actor_evidence"]).parent != output_root
        or Path(outputs["coordination_dir"]).parent != output_root
        or Path(outputs["framework_log"]).parent != output_root
    ):
        raise MatrixWireError("HA output paths do not share one private root")
    bounds = _exact(
        session["bounds"],
        {
            "cell_timeout_ms",
            "cleanup_timeout_ms",
            "frame_bytes",
            "evidence_bytes",
            "framework_log_bytes",
        },
        "bounds",
    )
    if (
        type(bounds["cell_timeout_ms"]) is not int
        or not 1 <= bounds["cell_timeout_ms"] <= 3_600_000
        or bounds["cleanup_timeout_ms"] != 45_000
        or bounds["frame_bytes"] != MAX_FRAME_BYTES
        or bounds["evidence_bytes"] != MAX_EVIDENCE_BYTES
        or bounds["framework_log_bytes"] != MAX_EVIDENCE_BYTES
    ):
        raise MatrixWireError("bounds are invalid")
    return session


class StdioBroker:
    """One strict installed-host broker attachment over saved stdio fds."""

    def __init__(
        self,
        read_fd: int,
        write_fd: int,
        host_session: dict,
        challenge: str,
        buffer: bytearray,
    ) -> None:
        self._read_fd = read_fd
        self._write_fd = write_fd
        self.host_session = host_session
        self._challenge = challenge
        self._buffer = buffer
        self._sequence = 0
        self._closed = False

    @classmethod
    def attach(cls, read_fd: int, write_fd: int, host_session: dict) -> StdioBroker:
        """Consume and validate the initial challenge on saved descriptors."""
        expected = _host_session(host_session, host_session.get("session_id"))
        buffer = bytearray()
        greeting = cls._read_frame(read_fd, buffer, time.monotonic() + 10.0)
        greeting = _exact(
            greeting,
            {"schema_version", "type", "session_id", "sequence", "challenge"},
            "broker greeting",
        )
        if (
            type(greeting["schema_version"]) is not int
            or greeting["schema_version"] != 1
            or greeting["type"] != "challenge"
            or greeting["session_id"] != expected["session_id"]
            or type(greeting["sequence"]) is not int
            or greeting["sequence"] != 0
        ):
            raise MatrixWireError("broker greeting identity is invalid")
        _digest(greeting["challenge"], "broker greeting challenge")
        return cls(read_fd, write_fd, expected, greeting["challenge"], buffer)

    @staticmethod
    def _remaining(deadline: float) -> float:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise MatrixWireError("broker total deadline expired")
        return remaining

    @classmethod
    def _read_frame(cls, fd: int, buffer: bytearray, deadline: float) -> object:
        while True:
            newline = buffer.find(b"\n")
            if newline >= 0:
                if newline > MAX_FRAME_BYTES:
                    raise MatrixWireError("broker frame exceeds bound")
                raw = bytes(buffer[:newline])
                del buffer[: newline + 1]
                break
            if len(buffer) > MAX_FRAME_BYTES:
                raise MatrixWireError("broker frame exceeds bound")
            readable, _, _ = select.select([fd], [], [], cls._remaining(deadline))
            if not readable:
                raise MatrixWireError("broker total deadline expired")
            chunk = os.read(fd, min(65_536, MAX_FRAME_BYTES + 1 - len(buffer)))
            if not chunk:
                raise MatrixWireError("broker attachment closed")
            buffer.extend(chunk)
        try:
            value = strict_json(raw)
        except (UnicodeError, ValueError, json.JSONDecodeError) as error:
            raise MatrixWireError("broker frame is not strict JSON") from error
        cls._remaining(deadline)
        return value

    def request(self, operation: str, *, device_id: str | None = None) -> dict:
        """Perform one fixed, sequenced operation without reconnecting."""
        if self._closed:
            raise MatrixWireError("broker attachment is closed")
        if operation not in OPERATIONS:
            raise MatrixWireError("broker operation is unsupported")
        if operation == "revoke":
            _session_id(device_id, "revoke device_id")
        elif device_id is not None:
            raise MatrixWireError("device_id is valid only for revoke")
        self._sequence += 1
        request = {
            "schema_version": 1,
            "session_id": self.host_session["session_id"],
            "sequence": self._sequence,
            "challenge": self._challenge,
            "op": operation,
        }
        if device_id is not None:
            request["device_id"] = device_id
        raw = canonical_json_bytes(request) + b"\n"
        deadline = time.monotonic() + DEADLINES[operation]
        try:
            _, writable, _ = select.select(
                [], [self._write_fd], [], self._remaining(deadline)
            )
            if not writable:
                raise MatrixWireError("broker total deadline expired")
            offset = 0
            while offset < len(raw):
                written = os.write(self._write_fd, raw[offset:])
                if written <= 0:
                    raise MatrixWireError("broker request write failed")
                offset += written
            reply = self._read_frame(self._read_fd, self._buffer, deadline)
            if not isinstance(reply, dict) or reply.get("type") not in {
                "reply",
                "error",
            }:
                raise MatrixWireError("broker reply type is invalid")
            payload_key = "result" if reply["type"] == "reply" else "error"
            _exact(
                reply,
                {
                    "schema_version",
                    "type",
                    "session_id",
                    "sequence",
                    "challenge",
                    payload_key,
                },
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
                or reply["session_id"] != self.host_session["session_id"]
                or type(reply["sequence"]) is not int
                or not sequence_valid
            ):
                raise MatrixWireError("broker reply identity is invalid")
            fresh = _digest(reply["challenge"], "broker reply challenge")
            if fresh == self._challenge:
                raise MatrixWireError("broker reply challenge was replayed")
            self._challenge = fresh
            if reply["type"] == "error":
                error = _exact(reply["error"], {"code"}, "broker error")
                if error["code"] not in {"invalid-request", "operation-failed"}:
                    raise MatrixWireError("broker error code is invalid")
                raise BrokerOperationError(error["code"])
            result = reply["result"]
            if operation == "stop":
                _exact(result, {"stopped", "events"}, "stop result")
                if result["stopped"] is not True or not isinstance(
                    result["events"], list
                ):
                    raise MatrixWireError("stop result is invalid")
            else:
                _exact(
                    result,
                    {
                        "descriptor",
                        "proof",
                        "invitation",
                        "expired_invitation",
                        "events",
                    },
                    "running result",
                )
                if not all(
                    isinstance(result[key], dict)
                    for key in (
                        "descriptor",
                        "proof",
                        "invitation",
                        "expired_invitation",
                    )
                ) or not isinstance(result["events"], list):
                    raise MatrixWireError("running result is invalid")
            return result
        except BaseException:
            self.close()
            raise

    def close(self) -> None:
        """Close both saved descriptors exactly once."""
        if self._closed:
            return
        self._closed = True
        for descriptor in (self._read_fd, self._write_fd):
            with suppress(OSError):
                os.close(descriptor)

    def assert_attached(self) -> None:
        """Reject broker EOF or unsolicited bytes before final readiness."""
        if self._closed:
            raise MatrixWireError("broker attachment is closed")
        if self._buffer:
            self.close()
            raise MatrixWireError("broker sent an unsolicited frame")
        readable, _, _ = select.select([self._read_fd], [], [], 0)
        if readable:
            self.close()
            raise MatrixWireError("broker closed or sent data before readiness")


class CoordinationChannel:
    """Exclusive top-level HA readiness and acknowledgement files."""

    def __init__(
        self,
        path: str | os.PathLike[str],
        *,
        session_id: str,
        cell_id: str,
        session_input_sha256: str,
        instance_nonce: str,
        cleanup_timeout_ms: int,
    ) -> None:
        self.path = Path(path)
        self.session_id = _session_id(session_id)
        if cell_id not in HA_CELLS:
            raise MatrixWireError("coordination cell_id is invalid")
        self.cell_id = cell_id
        self.session_input_sha256 = _digest(
            session_input_sha256, "coordination session input digest"
        )
        self.instance_nonce = _digest(instance_nonce, "coordination nonce")
        if type(cleanup_timeout_ms) is not int or cleanup_timeout_ms != 45_000:
            raise MatrixWireError("coordination cleanup bound is invalid")
        self.cleanup_timeout = cleanup_timeout_ms / 1000
        try:
            self.path.mkdir(mode=0o700)
        except OSError as error:
            raise MatrixWireError("coordination directory is unavailable") from error
        metadata = self.path.stat(follow_symlinks=False)
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or metadata.st_mode & 0o077
        ):
            raise MatrixWireError("coordination directory ownership is invalid")
        self._sequence = 0
        self._frozen: dict[str, str] = {}

    def _read_ack(self, path: Path, deadline: float) -> dict:
        while True:
            try:
                raw = _read_regular_private(str(path), 65_536, "coordination ack")
                break
            except MatrixWireError as error:
                if path.exists():
                    raise
                if time.monotonic() >= deadline:
                    raise MatrixWireError(
                        "coordination acknowledgement timed out"
                    ) from error
                time.sleep(0.25)
        try:
            value = strict_json(raw)
        except (UnicodeError, ValueError, json.JSONDecodeError) as error:
            raise MatrixWireError("coordination ack is not strict JSON") from error
        return _exact(
            value,
            {
                "schema_version",
                "type",
                "session_id",
                "cell_id",
                "session_input_sha256",
                "instance_nonce",
                "sequence",
                "ready_sha256",
                "phase",
                "status",
                "action",
                "result",
            },
            "coordination ack",
        )

    def barrier(
        self,
        *,
        phase: str,
        observation: dict,
        evidence: dict,
    ) -> dict:
        """Publish one ready file and wait for its exact accepted ack."""
        if phase not in {"ha_initial_observation", "evidence_ready"}:
            raise MatrixWireError("coordination phase is invalid")
        observation = _exact(
            observation, {"session_sequence", "proof_sha256"}, "ready observation"
        )
        if (
            type(observation["session_sequence"]) is not int
            or observation["session_sequence"] <= 0
        ):
            raise MatrixWireError("ready observation sequence is invalid")
        _digest(observation["proof_sha256"], "ready proof digest")
        evidence = _file_binding(evidence, "ready evidence")
        current = file_binding(evidence["path"])
        if current != evidence:
            raise MatrixWireError("ready evidence digest mismatch")
        self._sequence += 1
        if self._sequence > 128:
            raise MatrixWireError("coordination exchange bound exceeded")
        ready = {
            "schema_version": 1,
            "type": "ready",
            "session_id": self.session_id,
            "cell_id": self.cell_id,
            "session_input_sha256": self.session_input_sha256,
            "instance_nonce": self.instance_nonce,
            "sequence": self._sequence,
            "phase": phase,
            "observation": observation,
            "evidence": evidence,
        }
        ready_path = self.path / f"ready-{self._sequence:06d}.json"
        ready_binding = write_exclusive_json(ready_path, ready)
        self._frozen[evidence["path"]] = evidence["sha256"]
        ack_path = self.path / f"ack-{self._sequence:06d}.json"
        ack = self._read_ack(ack_path, time.monotonic() + self.cleanup_timeout)
        expected_action = (
            "advance_once" if phase == "ha_initial_observation" else "close_completed"
        )
        if (
            type(ack["schema_version"]) is not int
            or ack["schema_version"] != 1
            or ack["type"] != "ack"
            or ack["session_id"] != self.session_id
            or ack["cell_id"] != self.cell_id
            or ack["session_input_sha256"] != self.session_input_sha256
            or ack["instance_nonce"] != self.instance_nonce
            or type(ack["sequence"]) is not int
            or ack["sequence"] != self._sequence
            or ack["ready_sha256"] != ready_binding["sha256"]
            or ack["phase"] != phase
        ):
            raise MatrixWireError("coordination ack identity is invalid")
        if ack["status"] == "rejected":
            if ack["action"] != "abort" or ack["result"] is not None:
                raise MatrixWireError("rejected coordination ack is invalid")
            raise MatrixWireError("coordination readiness was rejected")
        if (
            ack["status"] != "accepted"
            or ack["action"] != expected_action
            or not isinstance(ack["result"], dict)
        ):
            raise MatrixWireError("accepted coordination ack is invalid")
        result = _file_binding(ack["result"], "coordination ack result")
        if file_binding(result["path"]) != result:
            raise MatrixWireError("coordination ack result digest mismatch")
        self.assert_frozen([evidence])
        return ack

    def read_ack_result(self, ack: dict) -> dict:
        """Read one already validated private result binding."""
        binding = _file_binding(ack.get("result"), "coordination ack result")
        raw = _read_regular_private(
            binding["path"], MAX_EVIDENCE_BYTES, "coordination ack result"
        )
        if hashlib.sha256(raw).hexdigest() != binding["sha256"]:
            raise MatrixWireError("coordination ack result digest mismatch")
        try:
            value = strict_json(raw)
        except (UnicodeError, ValueError, json.JSONDecodeError) as error:
            raise MatrixWireError(
                "coordination ack result is not strict JSON"
            ) from error
        if not isinstance(value, dict):
            raise MatrixWireError("coordination ack result is not an object")
        return value

    def assert_frozen(self, bindings: list[dict]) -> None:
        """Rehash immutable evidence and reject any later byte change."""
        for binding in bindings:
            value = _file_binding(binding, "frozen evidence")
            try:
                current = file_binding(value["path"])
            except (OSError, MatrixWireError) as error:
                raise MatrixWireError("evidence changed after readiness") from error
            if current != value:
                raise MatrixWireError("evidence changed after readiness")
