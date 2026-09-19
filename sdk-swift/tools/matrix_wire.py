"""Closed private file and broker wire for the installed Swift matrix adapter."""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import select
import socket
import stat
import time
import uuid
from pathlib import Path, PurePosixPath

MAX_FRAME_BYTES = 1_048_576
MAX_INPUT_BYTES = 1_048_576
MAX_EVIDENCE_BYTES = 8_388_608
OPERATIONS = frozenset({"verify", "stop", "start", "pair", "revoke"})
DEADLINES = {"verify": 30.0, "stop": 60.0, "start": 60.0, "pair": 160.0, "revoke": 160.0}
PHASE_TRANSFER_MS = 10_000
TOKEN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
SWIFT_CELLS = frozenset({"swift__macos_arm64", "swift__debian13_amd64", "swift__debian13_arm64"})
ACTORS = {
    "swift_macos": ("local_worker", "swift_macos", "swift_current_native_test"),
    "swift_linux": ("docker_worker", "swift_container", "swift_current_linux_test"),
}
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
    pass


class BrokerOperationError(MatrixWireError):
    def __init__(self, code):
        self.code = code
        super().__init__("installed-host broker operation failed")


def canonical_json_bytes(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON member")
        result[key] = value
    return result


def strict_json(raw):
    def reject(_):
        raise ValueError("non-finite JSON number")
    def finite(value):
        result = float(value)
        if not math.isfinite(result):
            raise ValueError("overflowing JSON number")
        return result
    return json.loads(raw.decode("utf-8", "strict"), object_pairs_hook=_unique_object, parse_constant=reject, parse_float=finite)


def _exact(value, keys, label):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise MatrixWireError(label + " shape is invalid")
    return value


def _digest(value, label):
    if not isinstance(value, str) or HEX64.fullmatch(value) is None:
        raise MatrixWireError(label + " is invalid")
    return value


def _session(value, label="session_id"):
    try:
        parsed = uuid.UUID(value) if isinstance(value, str) else None
    except ValueError as error:
        raise MatrixWireError(label + " is invalid") from error
    if parsed is None or parsed.version != 4 or str(parsed) != value:
        raise MatrixWireError(label + " is invalid")
    return value


def _absolute(value, label):
    if not isinstance(value, str) or not value or any(mark in value for mark in ("\x00", "\n", "\r")):
        raise MatrixWireError(label + " is invalid")
    path = PurePosixPath(value)
    if not path.is_absolute() or str(path) != value or ".." in path.parts:
        raise MatrixWireError(label + " is invalid")
    return value


def _private_admission_path(path, label):
    value = os.fspath(path)
    if not isinstance(value, str) or not value or any(mark in value for mark in ("\x00", "\n", "\r")):
        raise MatrixWireError(label + " path is invalid")
    pure = PurePosixPath(value)
    if not pure.is_absolute() or ".." in pure.parts:
        raise MatrixWireError(label + " path is invalid")

    # macOS exposes these fixed system directories through aliases. Resolve
    # only a canonical system alias; every caller-provided parent remains
    # descriptor-walked with O_NOFOLLOW below.
    for alias, target in (("/etc", "/private/etc"), ("/tmp", "/private/tmp"), ("/var", "/private/var")):
        if value == alias or value.startswith(alias + "/"):
            try:
                if os.path.islink(alias) and os.path.realpath(alias) == target:
                    return target + value[len(alias):]
            except OSError:
                raise MatrixWireError(label + " path is unavailable")
    return value


def _admit_private_directory(descriptor, label):
    try:
        metadata = os.fstat(descriptor)
    except OSError as error:
        raise MatrixWireError(label + " parent directory is unavailable") from error
    if not stat.S_ISDIR(metadata.st_mode):
        raise MatrixWireError(label + " parent directory is invalid")
    if metadata.st_uid not in (os.getuid(), 0):
        raise MatrixWireError(label + " parent directory ownership is invalid")
    # Readable/ searchable ancestors are fine. A writable ancestor must be
    # private or a sticky system temporary directory such as /tmp.
    if metadata.st_mode & 0o022 and not metadata.st_mode & stat.S_ISVTX:
        raise MatrixWireError(label + " parent directory mode is invalid")


def _open_private_parent(path, label):
    admitted = _private_admission_path(path, label)
    parts = PurePosixPath(admitted).parts
    if len(parts) < 2 or parts[0] != "/" or any(not part for part in parts[1:]):
        raise MatrixWireError(label + " path is invalid")
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    if hasattr(os, "O_CLOEXEC"):
        directory_flags |= os.O_CLOEXEC
    try:
        parent = os.open("/", directory_flags)
    except OSError as error:
        raise MatrixWireError(label + " parent directory is unavailable") from error
    try:
        _admit_private_directory(parent, label)
        for component in parts[1:-1]:
            try:
                child = os.open(component, directory_flags, dir_fd=parent)
            except OSError as error:
                raise MatrixWireError(label + " parent directory is unavailable") from error
            try:
                _admit_private_directory(child, label)
            except BaseException:
                os.close(child)
                raise
            os.close(parent)
            parent = child
        return parent, parts[-1]
    except BaseException:
        os.close(parent)
        raise


def _open_private_file(path, flags, label, mode=0):
    parent, leaf = _open_private_parent(path, label)
    try:
        try:
            descriptor = os.open(leaf, flags | os.O_NOFOLLOW, mode, dir_fd=parent)
        except OSError as error:
            raise MatrixWireError(label + " is unavailable") from error
        return descriptor
    finally:
        os.close(parent)


def _read_private(path, maximum, label):
    descriptor = _open_private_file(path, os.O_RDONLY | os.O_NONBLOCK, label)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
            raise MatrixWireError(label + " ownership or mode is invalid")
        raw = bytearray()
        while len(raw) <= maximum:
            part = os.read(descriptor, min(65536, maximum + 1 - len(raw)))
            if not part:
                break
            raw.extend(part)
        if len(raw) > maximum:
            raise MatrixWireError(label + " exceeds byte limit")
        return bytes(raw)
    finally:
        os.close(descriptor)


def file_binding(path, maximum=MAX_EVIDENCE_BYTES):
    absolute = os.path.abspath(os.fspath(path))
    raw = _read_private(absolute, maximum, "bound file")
    return {"path": absolute, "sha256": hashlib.sha256(raw).hexdigest()}


def write_exclusive_json(path, value):
    target = Path(path)
    raw = canonical_json_bytes(value) + b"\n"
    parent, leaf = _open_private_parent(target, "exclusive output")
    try:
        try:
            descriptor = os.open(
                leaf,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                0o600,
                dir_fd=parent,
            )
        except OSError as error:
            raise MatrixWireError("exclusive output is unavailable") from error
        try:
            offset = 0
            while offset < len(raw):
                written = os.write(descriptor, raw[offset:])
                if written <= 0:
                    raise MatrixWireError("exclusive output write failed")
                offset += written
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        os.fsync(parent)
    finally:
        os.close(parent)
    return {"path": str(target.resolve()), "sha256": hashlib.sha256(raw).hexdigest()}


def _binding(value, label):
    result = _exact(value, {"path", "sha256"}, label)
    _absolute(result["path"], label + " path")
    _digest(result["sha256"], label + " digest")
    return result


def _staged(value, label, read=False):
    result = _exact(value, {"id", "root", "local"}, label)
    if not isinstance(result["id"], str) or TOKEN.fullmatch(result["id"]) is None:
        raise MatrixWireError(label + " id is invalid")
    root = _binding(result["root"], label + " root")
    local = _binding(result["local"], label + " local")
    if root["sha256"] != local["sha256"]:
        raise MatrixWireError(label + " staged digests differ")
    if read:
        raw = _read_private(local["path"], MAX_INPUT_BYTES, label + " local")
        if hashlib.sha256(raw).hexdigest() != local["sha256"]:
            raise MatrixWireError(label + " local digest mismatch")
        return result, raw
    return result


def read_bound_json(binding, maximum=MAX_EVIDENCE_BYTES, label="bound JSON"):
    value = _binding(binding, label)
    raw = _read_private(value["path"], maximum, label)
    if hashlib.sha256(raw).hexdigest() != value["sha256"]:
        raise MatrixWireError(label + " digest mismatch")
    try:
        decoded = strict_json(raw)
    except (UnicodeError, ValueError, json.JSONDecodeError) as error:
        raise MatrixWireError(label + " is not strict JSON") from error
    if not isinstance(decoded, dict):
        raise MatrixWireError(label + " is not an object")
    return decoded


def _installed_manifest(staged, supplemental, label):
    _, raw = _staged(staged, label, read=True)
    try:
        value = strict_json(raw)
    except (UnicodeError, ValueError, json.JSONDecodeError) as error:
        raise MatrixWireError(label + " is not strict JSON") from error
    discriminator = "build_record" if supplemental else "artifact_sha256"
    manifest = _exact(value, {"schema_version", discriminator, "files"}, label)
    if manifest["schema_version"] != 1:
        raise MatrixWireError(label + " version is invalid")
    if supplemental:
        _binding(manifest["build_record"], label + " build record")
    else:
        _digest(manifest["artifact_sha256"], label + " artifact digest")
    files = manifest["files"]
    if not isinstance(files, list):
        raise MatrixWireError(label + " files are invalid")
    paths = []
    for item in files:
        _exact(item, {"path", "bytes", "mode", "sha256"}, label + " member")
        path = item["path"]
        if not isinstance(path, str) or not path or PurePosixPath(path).is_absolute() or ".." in PurePosixPath(path).parts:
            raise MatrixWireError(label + " member path is invalid")
        if type(item["bytes"]) is not int or item["bytes"] < 0 or type(item["mode"]) is not int or item["mode"] < 0:
            raise MatrixWireError(label + " member metadata is invalid")
        _digest(item["sha256"], label + " member digest")
        paths.append(path)
    if paths != sorted(set(paths)):
        raise MatrixWireError(label + " member order is invalid")
    return manifest


def validate_worker_config(value):
    required = {"schema_version", "kind", "actor_id", "session_id", "cell_id", "instance_nonce", "session_input_sha256", "remaining_cell_ms", "phase_contract", "inputs", "private_root", "coordination_dir", "evidence_path", "log_path"}
    config = _exact(value, required, "worker config")
    if type(config["schema_version"]) is not int or config["schema_version"] != 1 or config["kind"] != "matrix-actor-worker":
        raise MatrixWireError("worker config discriminator is invalid")
    if config["actor_id"] not in ACTORS or config["cell_id"] not in SWIFT_CELLS:
        raise MatrixWireError("worker identity is invalid")
    if type(config["remaining_cell_ms"]) is not int or not 1 <= config["remaining_cell_ms"] <= 3_600_000:
        raise MatrixWireError("worker remaining cell budget is invalid")
    _session(config["session_id"]); _digest(config["instance_nonce"], "worker nonce"); _digest(config["session_input_sha256"], "worker input digest")
    _staged(config["phase_contract"], "worker phase contract")
    if not isinstance(config["inputs"], list) or len(config["inputs"]) > 64:
        raise MatrixWireError("worker inputs are invalid")
    input_ids = []
    for item in config["inputs"]:
        _staged(item, "worker input")
        input_ids.append(item["id"])
    if len(input_ids) != len(set(input_ids)):
        raise MatrixWireError("worker inputs are duplicated")
    if input_ids.count("initial_observation") != 1:
        raise MatrixWireError("worker initial observation input is missing")
    for key in ("private_root", "coordination_dir", "evidence_path", "log_path"):
        _absolute(config[key], "worker " + key)
    return config


def load_phase_contract(path, actor_id):
    raw = _read_private(path, MAX_INPUT_BYTES, "phase contract")
    value = strict_json(raw)
    contract = _exact(value, {"schema_version", "kind", "actor_id", "resources", "phases"}, "phase contract")
    if contract["schema_version"] != 1 or contract["kind"] != "swift-worker-phases" or contract["actor_id"] != actor_id:
        raise MatrixWireError("phase contract identity is invalid")
    if not isinstance(contract["phases"], list) or len(contract["phases"]) != 6:
        raise MatrixWireError("phase contract is incomplete")
    if contract["resources"] != [{
        "id": "initial_observation", "kind": "staged_private_input",
        "schema": {"session_sequence": "positive_integer", "proof_sha256": "sha256"},
    }]:
        raise MatrixWireError("phase contract resources are invalid")
    expected = ["bootstrap_pair", "revoke_and_pair", "restart", "outage_stop", "outage_start", "final_verify"]
    for ordinal, phase in enumerate(contract["phases"], 1):
        _exact(phase, {"phase_id", "ordinal", "recipe_id", "operations", "timeout_ms"}, "phase")
        if phase["phase_id"] != expected[ordinal - 1] or phase["ordinal"] != ordinal or phase["recipe_id"] != "swift-" + phase["phase_id"]:
            raise MatrixWireError("phase order is invalid")
        if not isinstance(phase["operations"], list) or any(operation not in OPERATIONS for operation in phase["operations"]):
            raise MatrixWireError("phase recipe is invalid")
        expected_timeout = int(sum(DEADLINES[operation] for operation in phase["operations"]) * 1000) + PHASE_TRANSFER_MS
        if type(phase["timeout_ms"]) is not int or phase["timeout_ms"] != expected_timeout:
            raise MatrixWireError("phase timeout is invalid")
    return contract


def load_session_input(path):
    raw = _read_private(path, MAX_INPUT_BYTES, "session input")
    try:
        value = strict_json(raw)
    except (UnicodeError, ValueError, json.JSONDecodeError) as error:
        raise MatrixWireError("session input is not strict JSON") from error
    required = {"schema_version", "kind", "run_id", "cell_id", "adapter_id", "client_id", "session_id", "instance_nonce", "header", "case_contract", "host_session", "broker", "inputs", "actors", "outputs", "bounds"}
    session_input = _exact(value, required, "session input")
    if session_input["schema_version"] != 1 or session_input["kind"] != "matrix-adapter-session" or session_input["adapter_id"] != "swift" or session_input["client_id"] != "swift" or session_input["cell_id"] not in SWIFT_CELLS:
        raise MatrixWireError("session input identity is invalid")
    _session(session_input["session_id"]); _digest(session_input["instance_nonce"], "instance nonce")
    host = _exact(session_input["host_session"], {"schema_version", "kind", "broker_socket", "session_id", "registration_sha256"}, "host session")
    if host["schema_version"] != 1 or host["kind"] != "installed-host" or host["session_id"] != session_input["session_id"]:
        raise MatrixWireError("host session identity is invalid")
    _absolute(host["broker_socket"], "host broker socket"); _digest(host["registration_sha256"], "registration digest")
    broker = _exact(session_input["broker"], {"kind", "socket_path"}, "broker")
    if broker != {"kind": "unix", "socket_path": host["broker_socket"]}:
        raise MatrixWireError("Swift coordinator requires the host Unix broker")
    _, header_raw = _staged(session_input["header"], "header", read=True)
    _staged(session_input["case_contract"], "case contract", read=True)
    try:
        header = strict_json(header_raw)
    except (UnicodeError, ValueError, json.JSONDecodeError) as error:
        raise MatrixWireError("header is not strict JSON") from error
    header_keys = {"schema_version", "execution_kind", "adapter", "cell_id", "product_version", "profile_id", "profile_revision", "profile_sha256", "source_identities", "artifacts", "runtime"}
    _exact(header, header_keys, "header")
    if header["schema_version"] != 1 or header["execution_kind"] != "actual_hub_acceptance" or header["adapter"] != "swift" or header["cell_id"] != session_input["cell_id"] or header["product_version"] != "2026.36.2" or header["profile_id"] != "hub-http-v1" or header["profile_revision"] != "1.0.0":
        raise MatrixWireError("header identity is invalid")
    _digest(header["profile_sha256"], "header profile digest")
    inputs = _exact(session_input["inputs"], {"profile_manifest", "profile_members", "scenario", "certificate", "certificate_der_sha256", "product_inputs"}, "inputs")
    profile_manifest, profile_raw = _staged(inputs["profile_manifest"], "profile manifest", read=True)
    if profile_manifest["local"]["sha256"] != header["profile_sha256"]:
        raise MatrixWireError("profile checksum manifest differs from header")
    _staged(inputs["scenario"], "scenario", read=True)
    _staged(inputs["certificate"], "certificate", read=True)
    _digest(inputs["certificate_der_sha256"], "certificate DER digest")
    if not isinstance(inputs["profile_members"], list) or len(inputs["profile_members"]) != len(PROFILE_MEMBERS):
        raise MatrixWireError("profile members are incomplete")
    profile_ids = []
    member_paths = {}
    root_profile_roots = set()
    local_profile_roots = set()
    for item in inputs["profile_members"]:
        staged, member_raw = _staged(item, "profile member", read=True)
        profile_ids.append(item["id"])
        root_path = PurePosixPath(staged["root"]["path"])
        matches = [
            name for name in PROFILE_MEMBERS
            if tuple(root_path.parts[-len(PurePosixPath(name).parts):])
            == PurePosixPath(name).parts
        ]
        if len(matches) != 1 or matches[0] in member_paths:
            raise MatrixWireError("profile member layout is invalid")
        name = matches[0]
        name_parts = PurePosixPath(name).parts
        local_path = PurePosixPath(staged["local"]["path"])
        if tuple(local_path.parts[-len(name_parts):]) != name_parts:
            raise MatrixWireError("profile member local layout is invalid")
        root_profile_roots.add(PurePosixPath(*root_path.parts[:-len(name_parts)]))
        local_profile_roots.add(PurePosixPath(*local_path.parts[:-len(name_parts)]))
        member_paths[name] = (staged, member_raw)
    expected_profile_ids = [f"profile_member_{index:02d}" for index in range(1, len(PROFILE_MEMBERS) + 1)]
    if profile_ids != expected_profile_ids:
        raise MatrixWireError("profile members are reordered or duplicated")
    if (
        set(member_paths) != set(PROFILE_MEMBERS)
        or len(root_profile_roots) != 1
        or len(local_profile_roots) != 1
        or next(iter(root_profile_roots)).name != "1.0.0"
        or next(iter(root_profile_roots)).parent.name != "hub-http-v1"
        or next(iter(local_profile_roots)).name != "1.0.0"
        or next(iter(local_profile_roots)).parent.name != "hub-http-v1"
    ):
        raise MatrixWireError("profile member set is invalid")
    checksum_staged, checksum_raw = member_paths["SHA256SUMS"]
    if (
        profile_manifest["root"] != checksum_staged["root"]
        or profile_manifest["local"] != checksum_staged["local"]
        or profile_raw != checksum_raw
    ):
        raise MatrixWireError("profile manifest differs from SHA256SUMS member")
    try:
        lines = profile_raw.decode("utf-8", "strict").splitlines()
    except UnicodeError as error:
        raise MatrixWireError("profile SHA256SUMS is not UTF-8") from error
    checksums = {}
    for line in lines:
        match = re.fullmatch(r"([0-9a-f]{64})  ((?!/)(?!.*(?:^|/)\.\.(?:/|$))[^\x00\r\n]+)", line)
        if match is None:
            raise MatrixWireError("profile SHA256SUMS syntax is invalid")
        if match.group(2) in checksums:
            raise MatrixWireError("profile SHA256SUMS members are duplicated")
        checksums[match.group(2)] = match.group(1)
    expected_checksum_members = set(PROFILE_MEMBERS) - {"SHA256SUMS"}
    if set(checksums) != expected_checksum_members or list(checksums) != sorted(expected_checksum_members):
        raise MatrixWireError("profile SHA256SUMS members are incomplete or reordered")
    for name, digest in checksums.items():
        if hashlib.sha256(member_paths[name][1]).hexdigest() != digest:
            raise MatrixWireError("profile member checksum mismatch")
    if not isinstance(inputs["product_inputs"], list) or len(inputs["product_inputs"]) != 1:
        raise MatrixWireError("Swift product input is missing")
    product = _exact(inputs["product_inputs"][0], {"artifact_role", "staged", "installed_manifest", "local_root"}, "Swift product input")
    if product["artifact_role"] != "swift_sdk_product" or product["installed_manifest"] is None or product["local_root"] is None:
        raise MatrixWireError("Swift product input identity is invalid")
    product_staged, _ = _staged(product["staged"], "Swift product", read=True)
    _absolute(product["local_root"], "Swift product root")
    manifest = _installed_manifest(product["installed_manifest"], False, "Swift installed product manifest")
    selected = [item for item in header["artifacts"] if isinstance(item, dict) and item.get("role") == "swift_sdk_product"] if isinstance(header["artifacts"], list) else []
    if len(selected) != 1 or selected[0].get("sha256") != product_staged["local"]["sha256"] or manifest["artifact_sha256"] != product_staged["local"]["sha256"]:
        raise MatrixWireError("Swift product does not match header")
    if not isinstance(session_input["actors"], list) or [item.get("id") for item in session_input["actors"] if isinstance(item, dict)] != list(ACTORS):
        raise MatrixWireError("Swift actors are incomplete or reordered")
    for actor in session_input["actors"]:
        _exact(actor, {"id", "kind", "execution", "runtime_ref", "artifact_roles", "source_roles", "entrypoint_ref", "input_manifest", "phase_contract"}, "actor")
        execution, runtime_ref, entrypoint = ACTORS[actor["id"]]
        if actor["kind"] != "current_swift_transport" or actor["execution"] != execution or actor["runtime_ref"] != runtime_ref or actor["entrypoint_ref"] != entrypoint or actor["artifact_roles"] != ["swift_sdk_product"] or actor["source_roles"] != ["swift_sdk_source"] or actor["phase_contract"] is None:
            raise MatrixWireError("Swift actor contract is invalid")
        _installed_manifest(actor["input_manifest"], True, "actor input manifest")
        phase_binding, _ = _staged(actor["phase_contract"], "actor phase contract", read=True)
        load_phase_contract(phase_binding["local"]["path"], actor["id"])
    outputs = _exact(session_input["outputs"], {"normalized", "actor_evidence", "coordination_dir", "framework_log"}, "outputs")
    for value in outputs.values(): _absolute(value, "output path")
    if len(set(outputs.values())) != len(outputs):
        raise MatrixWireError("output paths alias")
    bounds = _exact(session_input["bounds"], {"cell_timeout_ms", "cleanup_timeout_ms", "frame_bytes", "evidence_bytes", "framework_log_bytes"}, "bounds")
    if type(bounds["cell_timeout_ms"]) is not int or not 1 <= bounds["cell_timeout_ms"] <= 3600000 or bounds["cleanup_timeout_ms"] != 45000 or bounds["frame_bytes"] != MAX_FRAME_BYTES or bounds["evidence_bytes"] != MAX_EVIDENCE_BYTES or bounds["framework_log_bytes"] != MAX_EVIDENCE_BYTES:
        raise MatrixWireError("session bounds are invalid")
    return session_input, hashlib.sha256(raw).hexdigest()


def _readline(connection, deadline):
    raw = bytearray()
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([connection], [], [], remaining)[0]:
            raise MatrixWireError("broker frame timed out")
        part = connection.recv(1)
        if not part:
            raise MatrixWireError("premature broker EOF")
        if part == b"\n":
            break
        raw.extend(part)
        if len(raw) > MAX_FRAME_BYTES:
            raise MatrixWireError("broker frame exceeds byte limit")
    try:
        value = strict_json(bytes(raw))
    except (ValueError, UnicodeError, json.JSONDecodeError) as error:
        raise MatrixWireError("broker frame is not strict JSON") from error
    if not isinstance(value, dict):
        raise MatrixWireError("broker frame is not an object")
    return value


def _validate_invitation(value, label):
    invitation = _exact(
        value,
        {"pairingId", "secret", "expiresAtMs", "endpoint", "tlsPin", "pairingUri"},
        label,
    )
    if (
        not all(isinstance(invitation[key], str) and invitation[key] for key in ("pairingId", "secret", "endpoint", "pairingUri"))
        or type(invitation["expiresAtMs"]) is not int
        or invitation["expiresAtMs"] <= 0
    ):
        raise MatrixWireError(label + " identity is invalid")
    _digest(invitation["tlsPin"], label + " TLS pin")
    return invitation


def _validate_broker_result(operation, result, session_id):
    if operation == "stop":
        value = _exact(result, {"stopped", "events"}, "stop result")
        if value["stopped"] is not True or not isinstance(value["events"], list) or len(value["events"]) > 512:
            raise MatrixWireError("stop result is invalid")
        return value
    value = _exact(result, {"descriptor", "proof", "invitation", "expired_invitation", "events"}, "running result")
    descriptor = _exact(value["descriptor"], {
        "status", "provenance", "endpoint", "hub_id", "hub_pid", "hub_started_at",
        "service_generation", "binary_sha256", "seed_binary_sha256", "profile_id",
        "profile_path", "profile_sha256", "scenario_path", "scenario_sha256",
        "certificate_path",
    }, "running descriptor")
    if descriptor["status"] != "ready" or descriptor["provenance"] != "installed-package-service":
        raise MatrixWireError("running descriptor status is invalid")
    for key in ("profile_path", "scenario_path", "certificate_path"):
        _absolute(descriptor[key], "descriptor " + key)
    for key in ("binary_sha256", "seed_binary_sha256", "profile_sha256", "scenario_sha256"):
        _digest(descriptor[key], "descriptor " + key)
    proof = value["proof"]
    if (
        not isinstance(proof, dict)
        or proof.get("status") != "verified"
        or proof.get("session_id") != session_id
        or type(proof.get("sequence")) is not int
        or proof["sequence"] <= 0
    ):
        raise MatrixWireError("running proof identity is invalid")
    invitation = _validate_invitation(value["invitation"], "active invitation")
    expired = _validate_invitation(value["expired_invitation"], "expired invitation")
    if invitation["pairingId"] == expired["pairingId"]:
        raise MatrixWireError("running invitations are not distinct")
    if not isinstance(value["events"], list) or len(value["events"]) > 512:
        raise MatrixWireError("running events are invalid")
    return value


class BrokerClient:
    def __init__(self, connection, session_id, sequence, challenge):
        self.connection = connection
        self.session_id = session_id
        self.sequence = sequence
        self.challenge = challenge
        self.attached = True

    @classmethod
    def attach(cls, connection, session_id):
        greeting = _readline(connection, time.monotonic() + 10.0)
        _exact(greeting, {"schema_version", "type", "session_id", "sequence", "challenge"}, "broker greeting")
        if greeting["schema_version"] != 1 or greeting["type"] != "challenge" or greeting["session_id"] != session_id or greeting["sequence"] != 0 or not isinstance(greeting["challenge"], str):
            raise MatrixWireError("broker greeting identity is invalid")
        return cls(connection, session_id, 0, greeting["challenge"])

    @classmethod
    def connect(cls, socket_path, session_id):
        connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        connection.connect(socket_path)
        try:
            return cls.attach(connection, session_id)
        except BaseException:
            connection.close(); raise

    def request(self, operation, device_id=None):
        if not self.attached or operation not in OPERATIONS or (operation == "revoke") != (device_id is not None):
            raise MatrixWireError("broker operation is invalid")
        sequence = self.sequence + 1
        request = {"schema_version": 1, "session_id": self.session_id, "sequence": sequence, "challenge": self.challenge, "op": operation}
        if device_id is not None: request["device_id"] = device_id
        self.connection.sendall(canonical_json_bytes(request) + b"\n")
        reply = _readline(self.connection, time.monotonic() + DEADLINES[operation])
        expected = {"schema_version", "type", "session_id", "sequence", "challenge", "result"} if reply.get("type") == "reply" else {"schema_version", "type", "session_id", "sequence", "challenge", "error"}
        _exact(reply, expected, "broker reply")
        if reply["schema_version"] != 1 or reply["session_id"] != self.session_id or reply["sequence"] != sequence or not isinstance(reply["challenge"], str):
            raise MatrixWireError("broker reply identity is invalid")
        self.sequence = sequence; self.challenge = reply["challenge"]
        if reply["type"] == "error":
            error = _exact(reply["error"], {"code"}, "broker error")
            if error["code"] not in {"invalid-request", "operation-failed"}: raise MatrixWireError("broker error code is invalid")
            raise BrokerOperationError(error["code"])
        if reply["type"] != "reply" or not isinstance(reply["result"], dict):
            raise MatrixWireError("broker reply type is invalid")
        return _validate_broker_result(operation, reply["result"], self.session_id)

    def assert_attached(self):
        if not self.attached:
            raise MatrixWireError("broker attachment is closed")

    def close(self):
        if self.attached:
            self.attached = False
            self.connection.close()


def validate_completion_ack(ack, session_id, cell_id, session_input_sha256, instance_nonce, sequence, ready_sha256):
    value = _exact(ack, {"schema_version", "type", "session_id", "cell_id", "session_input_sha256", "instance_nonce", "sequence", "ready_sha256", "phase", "status", "action", "result"}, "completion ack")
    if value["schema_version"] != 1 or value["type"] != "ack" or value["session_id"] != session_id or value["cell_id"] != cell_id or value["session_input_sha256"] != session_input_sha256 or value["instance_nonce"] != instance_nonce or value["sequence"] != sequence or value["ready_sha256"] != ready_sha256 or value["phase"] != "evidence_ready" or value["status"] != "accepted" or value["action"] != "close_completed":
        raise MatrixWireError("completion acknowledgement is invalid")
    _binding(value["result"], "completion close evidence")
    return value


class CoordinationChannel:
    def __init__(self, session, session_input_sha256):
        self.session = session; self.session_input_sha256 = session_input_sha256
        self.root = Path(session["outputs"]["coordination_dir"]); self.sequence = 0

    def barrier(self, observation, evidence, timeout_seconds=45.0):
        self.sequence += 1
        ready = {"schema_version": 1, "type": "ready", "session_id": self.session["session_id"], "cell_id": self.session["cell_id"], "session_input_sha256": self.session_input_sha256, "instance_nonce": self.session["instance_nonce"], "sequence": self.sequence, "phase": "evidence_ready", "observation": observation, "evidence": evidence}
        binding = write_exclusive_json(self.root / ("ready-%06d.json" % self.sequence), ready)
        ack_path = self.root / ("ack-%06d.json" % self.sequence)
        deadline = time.monotonic() + timeout_seconds
        while time.monotonic() < deadline:
            if ack_path.exists():
                raw = _read_private(ack_path, 65536, "completion ack")
                ack = strict_json(raw)
                return validate_completion_ack(ack, self.session["session_id"], self.session["cell_id"], self.session_input_sha256, self.session["instance_nonce"], self.sequence, binding["sha256"])
            time.sleep(0.25)
        raise MatrixWireError("completion acknowledgement timed out")
